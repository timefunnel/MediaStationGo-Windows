#[cfg(not(windows))]
fn main() {
    eprintln!("MediaStationGo portable updates are only supported on Windows");
}

#[cfg(windows)]
mod windows_updater {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Write};
    use std::path::{Component, Path, PathBuf};
    use std::process::Command;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use zip::ZipArchive;

    const APP_EXECUTABLE: &str = "jellium-desktop.exe";
    const HELPER_EXECUTABLE: &str = "mediastation-portable-updater.exe";
    const PORTABLE_MARKER: &str = ".mediastation-portable";
    const PORTABLE_MANIFEST: &str = ".mediastation-portable-files.txt";
    const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
    const MAX_EXTRACTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
    const MAX_ARCHIVE_ENTRIES: usize = 50_000;
    const PARENT_EXIT_TIMEOUT_MS: u32 = 120_000;

    struct Args {
        parent_pid: u32,
        archive: PathBuf,
        install_dir: PathBuf,
        expected_version: String,
        expected_sha256: String,
        log_file: PathBuf,
    }

    struct Logger {
        path: PathBuf,
    }

    impl Logger {
        fn line(&self, message: &str) {
            if let Some(parent) = self.path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                let _ = writeln!(file, "{message}");
            }
        }
    }

    struct AppliedUpdate {
        backed_up: Vec<String>,
        installed: Vec<String>,
        backup_root: PathBuf,
    }

    pub fn main() {
        if let Err(error) = run() {
            show_error(&format!(
                "MediaStationGo portable update failed.\n\n{error}"
            ));
            std::process::exit(1);
        }
    }

    fn run() -> Result<(), String> {
        let args = parse_args(std::env::args_os().skip(1))?;
        let logger = Logger {
            path: args.log_file.clone(),
        };
        logger.line("portable update started");

        validate_version(&args.expected_version)?;
        validate_sha256(&args.expected_sha256)?;
        let install_dir = fs::canonicalize(&args.install_dir)
            .map_err(|error| format!("cannot resolve install directory: {error}"))?;
        if !install_dir.is_dir() || install_dir.file_name().is_none() {
            return Err("install directory is not a managed application directory".to_string());
        }
        let current_files = read_manifest(&install_dir)?;
        require_package_files(&current_files)?;

        let archive = fs::canonicalize(&args.archive)
            .map_err(|error| format!("cannot resolve update archive: {error}"))?;
        verify_archive(&archive, &args.expected_sha256)?;

        let versions_parent = install_dir
            .parent()
            .ok_or_else(|| "install directory has no parent".to_string())?;
        let workspace = versions_parent.join(format!(
            ".mediastation-update-{}-{}",
            args.expected_version,
            std::process::id()
        ));
        if workspace.exists() {
            return Err(format!(
                "update workspace already exists: {}",
                workspace.display()
            ));
        }
        let staging_root = workspace.join("staging");
        let backup_root = workspace.join("backup");
        fs::create_dir_all(&staging_root)
            .map_err(|error| format!("cannot create update workspace: {error}"))?;

        let prepared = (|| {
            let extracted_files = extract_archive(&archive, &staging_root)?;
            let new_files = read_manifest(&staging_root)?;
            require_package_files(&new_files)?;
            if extracted_files != new_files {
                return Err("portable archive contents do not match its file manifest".to_string());
            }
            let staged_version = fs::read_to_string(staging_root.join(PORTABLE_MARKER))
                .map_err(|error| format!("cannot read staged portable marker: {error}"))?;
            if staged_version.trim() != args.expected_version {
                return Err(format!(
                    "portable archive version mismatch: expected {}, found {}",
                    args.expected_version,
                    staged_version.trim()
                ));
            }
            Ok(new_files)
        })();
        let new_files = match prepared {
            Ok(files) => files,
            Err(error) => {
                let _ = fs::remove_dir_all(&workspace);
                return Err(error);
            }
        };

        wait_for_parent(args.parent_pid)?;
        logger.line("main application exited; applying files");
        let applied = apply_update(
            &install_dir,
            &staging_root,
            &backup_root,
            &current_files,
            &new_files,
        )?;

        let executable = install_dir.join(APP_EXECUTABLE);
        if let Err(error) = Command::new(&executable).current_dir(&install_dir).spawn() {
            let rollback = rollback_update(&install_dir, &applied);
            return Err(match rollback {
                Ok(()) => {
                    format!("cannot restart updated application; update rolled back: {error}")
                }
                Err(rollback_error) => format!(
                    "cannot restart updated application: {error}; rollback also failed: {rollback_error}"
                ),
            });
        }

        if let Err(error) = fs::remove_dir_all(&workspace) {
            logger.line(&format!("warning: cannot remove update workspace: {error}"));
        }
        if let Err(error) = fs::remove_file(&archive) {
            logger.line(&format!(
                "warning: cannot remove downloaded archive: {error}"
            ));
        }
        logger.line("portable update completed");
        Ok(())
    }

    fn parse_args<I>(args: I) -> Result<Args, String>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut parent_pid = None;
        let mut archive = None;
        let mut install_dir = None;
        let mut expected_version = None;
        let mut expected_sha256 = None;
        let mut log_file = None;
        let mut values = args.into_iter();
        while let Some(flag) = values.next() {
            let flag = flag
                .to_str()
                .ok_or_else(|| "update argument name is not valid Unicode".to_string())?;
            let value = values
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag {
                "--parent-pid" => {
                    parent_pid = Some(
                        value
                            .to_str()
                            .ok_or_else(|| "parent PID is not valid Unicode".to_string())?
                            .parse()
                            .map_err(|error| format!("invalid parent PID: {error}"))?,
                    );
                }
                "--archive" => archive = Some(PathBuf::from(value)),
                "--install-dir" => install_dir = Some(PathBuf::from(value)),
                "--expected-version" => {
                    expected_version = Some(
                        value
                            .into_string()
                            .map_err(|_| "expected version is not valid Unicode".to_string())?,
                    );
                }
                "--expected-sha256" => {
                    expected_sha256 = Some(
                        value
                            .into_string()
                            .map_err(|_| "expected checksum is not valid Unicode".to_string())?,
                    );
                }
                "--log-file" => log_file = Some(PathBuf::from(value)),
                _ => return Err(format!("unsupported update argument: {flag}")),
            }
        }
        Ok(Args {
            parent_pid: parent_pid.ok_or_else(|| "parent PID is missing".to_string())?,
            archive: archive.ok_or_else(|| "archive path is missing".to_string())?,
            install_dir: install_dir.ok_or_else(|| "install directory is missing".to_string())?,
            expected_version: expected_version
                .ok_or_else(|| "expected version is missing".to_string())?,
            expected_sha256: expected_sha256
                .ok_or_else(|| "expected checksum is missing".to_string())?,
            log_file: log_file.ok_or_else(|| "log path is missing".to_string())?,
        })
    }

    fn validate_version(version: &str) -> Result<(), String> {
        let parts = version.split('.').collect::<Vec<_>>();
        if parts.len() != 3
            || parts
                .iter()
                .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err(format!("invalid update version: {version}"));
        }
        Ok(())
    }

    fn validate_sha256(value: &str) -> Result<(), String> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid expected SHA-256 value".to_string());
        }
        Ok(())
    }

    fn verify_archive(path: &Path, expected: &str) -> Result<(), String> {
        let metadata = fs::metadata(path)
            .map_err(|error| format!("cannot inspect update archive: {error}"))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ARCHIVE_BYTES {
            return Err("update archive size is outside the allowed range".to_string());
        }
        let mut file =
            File::open(path).map_err(|error| format!("cannot open update archive: {error}"))?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; 256 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| format!("cannot read update archive: {error}"))?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        let actual = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != expected.to_ascii_lowercase() {
            return Err("update archive SHA-256 verification failed".to_string());
        }
        Ok(())
    }

    fn extract_archive(path: &Path, staging_root: &Path) -> Result<BTreeSet<String>, String> {
        let file =
            File::open(path).map_err(|error| format!("cannot open portable archive: {error}"))?;
        let mut archive = ZipArchive::new(file)
            .map_err(|error| format!("portable archive is invalid: {error}"))?;
        if archive.len() > MAX_ARCHIVE_ENTRIES {
            return Err("portable archive contains too many entries".to_string());
        }
        let mut extracted = BTreeSet::new();
        let mut total_size = 0_u64;
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|error| format!("cannot read archive entry {index}: {error}"))?;
            let enclosed = entry.enclosed_name().ok_or_else(|| {
                format!("archive entry escapes the destination: {}", entry.name())
            })?;
            let relative = normalize_relative(&enclosed)?;
            let destination = staging_root.join(relative_path(&relative)?);
            if entry.is_dir() {
                fs::create_dir_all(&destination)
                    .map_err(|error| format!("cannot create archive directory: {error}"))?;
                continue;
            }
            if let Some(mode) = entry.unix_mode() {
                let file_type = mode & 0o170000;
                if file_type != 0 && file_type != 0o100000 {
                    return Err(format!("unsupported archive entry type: {relative}"));
                }
            }
            total_size = total_size
                .checked_add(entry.size())
                .ok_or_else(|| "portable archive size overflow".to_string())?;
            if total_size > MAX_EXTRACTED_BYTES {
                return Err("portable archive expands beyond the allowed size".to_string());
            }
            if !extracted.insert(relative.clone()) {
                return Err(format!("duplicate archive entry: {relative}"));
            }
            let parent = destination
                .parent()
                .ok_or_else(|| format!("archive entry has no parent: {relative}"))?;
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create archive entry directory: {error}"))?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
                .map_err(|error| format!("cannot create archive entry {relative}: {error}"))?;
            std::io::copy(&mut entry, &mut output)
                .map_err(|error| format!("cannot extract archive entry {relative}: {error}"))?;
            output
                .sync_all()
                .map_err(|error| format!("cannot flush archive entry {relative}: {error}"))?;
        }
        Ok(extracted)
    }

    fn read_manifest(root: &Path) -> Result<BTreeSet<String>, String> {
        let content = fs::read_to_string(root.join(PORTABLE_MANIFEST))
            .map_err(|error| format!("cannot read portable file manifest: {error}"))?;
        parse_manifest(&content)
    }

    fn parse_manifest(content: &str) -> Result<BTreeSet<String>, String> {
        let mut files = BTreeSet::new();
        for raw in content.lines() {
            let line = raw.trim_end_matches('\r');
            if line.is_empty() {
                return Err("portable file manifest contains an empty path".to_string());
            }
            let normalized = normalize_relative(Path::new(line))?;
            if normalized != line.replace('\\', "/") {
                return Err(format!(
                    "portable file manifest path is not normalized: {line}"
                ));
            }
            if !files.insert(normalized.clone()) {
                return Err(format!(
                    "portable file manifest contains a duplicate: {normalized}"
                ));
            }
        }
        if files.is_empty() || files.len() > MAX_ARCHIVE_ENTRIES {
            return Err("portable file manifest has an invalid entry count".to_string());
        }
        Ok(files)
    }

    fn require_package_files(files: &BTreeSet<String>) -> Result<(), String> {
        for required in [
            APP_EXECUTABLE,
            HELPER_EXECUTABLE,
            PORTABLE_MARKER,
            PORTABLE_MANIFEST,
        ] {
            if !files.contains(required) {
                return Err(format!("portable file manifest is missing {required}"));
            }
        }
        Ok(())
    }

    fn normalize_relative(path: &Path) -> Result<String, String> {
        let mut parts = Vec::new();
        for component in path.components() {
            let Component::Normal(part) = component else {
                return Err(format!("unsafe relative path: {}", path.display()));
            };
            let value = part
                .to_str()
                .ok_or_else(|| format!("path is not valid Unicode: {}", path.display()))?;
            if value.is_empty() || value.contains(':') {
                return Err(format!("unsafe path component: {value}"));
            }
            parts.push(value);
        }
        if parts.is_empty() {
            return Err("empty relative path".to_string());
        }
        Ok(parts.join("/"))
    }

    fn relative_path(value: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(value);
        normalize_relative(&path)?;
        Ok(path)
    }

    fn wait_for_parent(parent_pid: u32) -> Result<(), String> {
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
        if handle.is_null() {
            let error = unsafe { GetLastError() };
            if error == ERROR_INVALID_PARAMETER {
                return Ok(());
            }
            return Err(format!(
                "cannot open parent process for waiting: Windows error {error}"
            ));
        }
        let result = unsafe { WaitForSingleObject(handle, PARENT_EXIT_TIMEOUT_MS) };
        unsafe {
            CloseHandle(handle);
        }
        if result != WAIT_OBJECT_0 {
            return Err(format!(
                "parent process did not exit in time: wait result {result}"
            ));
        }
        Ok(())
    }

    fn apply_update(
        install_dir: &Path,
        staging_root: &Path,
        backup_root: &Path,
        current_files: &BTreeSet<String>,
        new_files: &BTreeSet<String>,
    ) -> Result<AppliedUpdate, String> {
        preflight_targets(install_dir, current_files, new_files)?;
        fs::create_dir_all(backup_root)
            .map_err(|error| format!("cannot create update backup: {error}"))?;
        let mut state = AppliedUpdate {
            backed_up: Vec::new(),
            installed: Vec::new(),
            backup_root: backup_root.to_path_buf(),
        };
        let result = (|| {
            let affected = current_files
                .union(new_files)
                .cloned()
                .collect::<BTreeSet<_>>();
            for relative in affected {
                let target = install_dir.join(relative_path(&relative)?);
                if !target.exists() {
                    continue;
                }
                let backup = backup_root.join(relative_path(&relative)?);
                if let Some(parent) = backup.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| format!("cannot create backup directory: {error}"))?;
                }
                fs::rename(&target, &backup)
                    .map_err(|error| format!("cannot back up {relative}: {error}"))?;
                state.backed_up.push(relative);
            }
            for relative in new_files {
                let source = staging_root.join(relative_path(relative)?);
                if !source.is_file() {
                    return Err(format!("staged update file is missing: {relative}"));
                }
                let target = install_dir.join(relative_path(relative)?);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| format!("cannot create install directory: {error}"))?;
                }
                fs::rename(&source, &target)
                    .map_err(|error| format!("cannot install {relative}: {error}"))?;
                state.installed.push(relative.clone());
            }
            Ok(())
        })();
        if let Err(error) = result {
            let rollback = rollback_update(install_dir, &state);
            return Err(match rollback {
                Ok(()) => format!("{error}; update rolled back"),
                Err(rollback_error) => format!("{error}; rollback failed: {rollback_error}"),
            });
        }
        Ok(state)
    }

    fn preflight_targets(
        install_dir: &Path,
        current_files: &BTreeSet<String>,
        new_files: &BTreeSet<String>,
    ) -> Result<(), String> {
        let canonical_root = fs::canonicalize(install_dir)
            .map_err(|error| format!("cannot resolve install directory: {error}"))?;
        let affected = current_files
            .union(new_files)
            .cloned()
            .collect::<BTreeSet<_>>();
        for relative in affected {
            let target = install_dir.join(relative_path(&relative)?);
            validate_existing_ancestors(&canonical_root, install_dir, &target)?;
            if !target.exists() {
                continue;
            }
            let metadata = fs::symlink_metadata(&target)
                .map_err(|error| format!("cannot inspect {relative}: {error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "managed update target is not a regular file: {relative}"
                ));
            }
            if new_files.contains(&relative) && !current_files.contains(&relative) {
                return Err(format!(
                    "update would overwrite an unmanaged file: {relative}"
                ));
            }
        }
        Ok(())
    }

    fn validate_existing_ancestors(
        canonical_root: &Path,
        install_dir: &Path,
        target: &Path,
    ) -> Result<(), String> {
        let relative = target
            .strip_prefix(install_dir)
            .map_err(|_| "update target escapes the install directory".to_string())?;
        let mut current = install_dir.to_path_buf();
        for component in relative.components() {
            current.push(component.as_os_str());
            if !current.exists() || current == target {
                continue;
            }
            let metadata = fs::symlink_metadata(&current)
                .map_err(|error| format!("cannot inspect update path: {error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!(
                    "unsafe update path component: {}",
                    current.display()
                ));
            }
            let canonical = fs::canonicalize(&current)
                .map_err(|error| format!("cannot resolve update path: {error}"))?;
            if !canonical.starts_with(canonical_root) {
                return Err(format!(
                    "update path leaves the install directory: {}",
                    current.display()
                ));
            }
        }
        Ok(())
    }

    fn rollback_update(install_dir: &Path, state: &AppliedUpdate) -> Result<(), String> {
        let mut errors = Vec::new();
        for relative in state.installed.iter().rev() {
            match relative_path(relative) {
                Ok(path) => {
                    let target = install_dir.join(path);
                    if target.exists() {
                        if let Err(error) = fs::remove_file(&target) {
                            errors.push(format!("cannot remove {relative}: {error}"));
                        }
                    }
                }
                Err(error) => errors.push(error),
            }
        }
        for relative in state.backed_up.iter().rev() {
            let Ok(path) = relative_path(relative) else {
                errors.push(format!("cannot restore invalid path: {relative}"));
                continue;
            };
            let backup = state.backup_root.join(&path);
            let target = install_dir.join(&path);
            if let Some(parent) = target.parent() {
                if let Err(error) = fs::create_dir_all(parent) {
                    errors.push(format!("cannot recreate directory for {relative}: {error}"));
                    continue;
                }
            }
            if let Err(error) = fs::rename(&backup, &target) {
                errors.push(format!("cannot restore {relative}: {error}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn show_error(message: &str) {
        let text = wide(message);
        let title = wide("MediaStationGo Update");
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn write_file(path: &Path, value: &str) {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, value).unwrap();
        }

        #[test]
        fn manifest_rejects_unsafe_and_duplicate_paths() {
            assert!(parse_manifest("../escape.exe\n").is_err());
            assert!(parse_manifest("C:/escape.exe\n").is_err());
            assert!(parse_manifest("app.exe\napp.exe\n").is_err());
            assert_eq!(
                parse_manifest("app.exe\nlocales/en-US.pak\n")
                    .unwrap()
                    .len(),
                2
            );
        }

        #[test]
        fn managed_update_preserves_unknown_files_and_removes_stale_files() {
            let temp = tempfile::tempdir().unwrap();
            let install = temp.path().join("MediaStationGo");
            let staging = temp.path().join("staging");
            let backup = temp.path().join("backup");
            fs::create_dir_all(&install).unwrap();
            fs::create_dir_all(&staging).unwrap();
            write_file(&install.join("app.exe"), "old");
            write_file(&install.join("stale.dll"), "stale");
            write_file(&install.join("user-note.txt"), "keep");
            write_file(&staging.join("app.exe"), "new");
            write_file(&staging.join("new.dll"), "added");
            let current = ["app.exe", "stale.dll"]
                .into_iter()
                .map(str::to_string)
                .collect();
            let next = ["app.exe", "new.dll"]
                .into_iter()
                .map(str::to_string)
                .collect();

            let applied = apply_update(&install, &staging, &backup, &current, &next).unwrap();
            assert_eq!(fs::read_to_string(install.join("app.exe")).unwrap(), "new");
            assert_eq!(
                fs::read_to_string(install.join("new.dll")).unwrap(),
                "added"
            );
            assert_eq!(
                fs::read_to_string(install.join("user-note.txt")).unwrap(),
                "keep"
            );
            assert!(!install.join("stale.dll").exists());

            rollback_update(&install, &applied).unwrap();
            assert_eq!(fs::read_to_string(install.join("app.exe")).unwrap(), "old");
            assert_eq!(
                fs::read_to_string(install.join("stale.dll")).unwrap(),
                "stale"
            );
            assert!(!install.join("new.dll").exists());
            assert_eq!(
                fs::read_to_string(install.join("user-note.txt")).unwrap(),
                "keep"
            );
        }

        #[test]
        fn update_rejects_an_unmanaged_file_collision() {
            let temp = tempfile::tempdir().unwrap();
            let install = temp.path().join("MediaStationGo");
            fs::create_dir_all(&install).unwrap();
            write_file(&install.join("user.dll"), "mine");
            let current = BTreeSet::new();
            let next = ["user.dll"].into_iter().map(str::to_string).collect();
            assert!(preflight_targets(&install, &current, &next).is_err());
            assert_eq!(
                fs::read_to_string(install.join("user.dll")).unwrap(),
                "mine"
            );
        }
    }
}

#[cfg(windows)]
fn main() {
    windows_updater::main();
}
