//! Versioned, validated per-user RIFE package. Mutations apply on next startup.

use super::{InterpolationError, RIFE_RUNTIME_ABI, RIFE_TENSORRT_VERSION};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const RELEASE_TAG: &str = "rife-component-x64-v1";
pub const ASSET_NAME: &str = "MediaStationGo-frame-interpolation-x64-v1.zip";
pub const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
pub const SUPPORTED: bool = cfg!(all(windows, target_arch = "x86_64"));
const MAX_UNPACKED_BYTES: u64 = 768 * 1024 * 1024;
const MANIFEST: &str = "component-manifest.json";
const FILES: &[&str] = &[
    "rife_runtime.dll",
    "cudart64_12.dll",
    "tensorrt_rtx_1_4.dll",
    "tensorrt_onnxparser_rtx_1_4.dll",
    "rife_engine_builder.exe",
    "frame-interpolation/runtime-manifest.json",
    "frame-interpolation/models/rife_v4.26_fp16_io.onnx",
    "frame-interpolation/models/rife_v4.26_scale0.5.onnx",
    "frame-interpolation/models/rife_v4.25_lite_fp16_io.onnx",
    "licenses/MediaStationGo-GPL-2.0.txt",
    "licenses/RIFE-LICENSE.txt",
    "licenses/RIFE-ENGINE-BUILDER-MIT.txt",
    "licenses/NVIDIA-RUNTIME-NOTICE.txt",
    "licenses/TensorRT-RTX-Acknowledgements.txt",
    "licenses/TensorRT-RTX-README.txt",
];

fn error(detail: impl Into<String>) -> InterpolationError {
    InterpolationError::new("frame_interpolation_component_invalid", detail)
}

fn io_error(operation: &str, cause: impl std::fmt::Display) -> InterpolationError {
    error(format!("{operation}: {cause}"))
}

#[derive(Clone)]
struct Paths {
    root: PathBuf,
    active: PathBuf,
    pending: PathBuf,
    cache: PathBuf,
    downloads: PathBuf,
}

impl Paths {
    fn new(cache_root: &Path) -> Result<Self, InterpolationError> {
        if !cache_root.is_absolute()
            || cache_root.file_name().and_then(|name| name.to_str()) != Some("frame-interpolation")
        {
            return Err(error(
                "Component cache root must be an absolute frame-interpolation directory",
            ));
        }
        let parent = cache_root
            .parent()
            .ok_or_else(|| error("Component cache has no parent"))?;
        let root = parent.join("components").join("rife");
        Ok(Self {
            active: root.join("active"),
            pending: root.join("pending"),
            cache: cache_root.join("engine-cache"),
            downloads: cache_root.join("downloads"),
            root,
        })
    }
}

fn reject_links(path: &Path) -> Result<(), InterpolationError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|cause| io_error("Inspect component path", cause))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(error("Component paths must not be reparse points"));
        }
    }
    if metadata.file_type().is_symlink() {
        return Err(error("Component paths must not be symbolic links"));
    }
    Ok(())
}

fn exists(path: &Path) -> Result<bool, InterpolationError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(cause) if cause.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(cause) => Err(io_error("Inspect component path existence", cause)),
    }
}

fn reject_existing_ancestors(path: &Path) -> Result<(), InterpolationError> {
    for ancestor in path.ancestors() {
        if exists(ancestor)? {
            reject_links(ancestor)?;
        }
    }
    Ok(())
}

fn managed_root(paths: &Paths) -> Result<(), InterpolationError> {
    reject_existing_ancestors(&paths.root)?;
    fs::create_dir_all(&paths.root)
        .map_err(|cause| io_error("Create component directory", cause))?;
    for ancestor in paths.root.ancestors() {
        reject_links(ancestor)?;
    }
    Ok(())
}

#[cfg(windows)]
fn lock(paths: &Paths, name: &str, exclusive: bool) -> Result<File, InterpolationError> {
    use std::os::windows::fs::OpenOptionsExt as _;
    let path = paths.root.join(name);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => drop(file),
        Err(cause) if cause.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(cause) => return Err(io_error("Create component lease", cause)),
    }
    reject_links(&path)?;
    OpenOptions::new()
        .read(true)
        .write(exclusive)
        .share_mode(if exclusive { 0 } else { 1 })
        .open(path)
        .map_err(|cause| {
            InterpolationError::new(
                "frame_interpolation_component_busy",
                format!(
                    "Close all other player instances before applying component changes: {cause}"
                ),
            )
        })
}

#[cfg(not(windows))]
fn lock(_paths: &Paths, _name: &str, _exclusive: bool) -> Result<File, InterpolationError> {
    Err(InterpolationError::new(
        "frame_interpolation_platform_unsupported",
        "Components require Windows",
    ))
}

fn sha256(path: &Path) -> Result<String, InterpolationError> {
    let mut file = File::open(path).map_err(|cause| io_error("Open component file", cause))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 256 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|cause| io_error("Read component file", cause))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn valid_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_manifest(bytes: &[u8]) -> Result<BTreeMap<String, (u64, String)>, InterpolationError> {
    if bytes.len() > 64 * 1024 {
        return Err(error("Component manifest exceeds 64 KiB"));
    }
    let manifest: Value = serde_json::from_slice(bytes)
        .map_err(|cause| io_error("Parse component manifest", cause))?;
    if manifest["schema"] != 1
        || manifest["component"] != "rife"
        || manifest["version"] != "1"
        || manifest["platform"] != "windows-x64"
        || manifest["runtimeAbi"] != RIFE_RUNTIME_ABI
        || manifest["tensorRtVersion"] != RIFE_TENSORRT_VERSION
    {
        return Err(error(
            "Unsupported component version, platform or runtime tuple",
        ));
    }
    let entries = manifest["files"]
        .as_array()
        .ok_or_else(|| error("Component file list missing"))?;
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for entry in entries {
        let path = entry["path"]
            .as_str()
            .ok_or_else(|| error("Component path missing"))?;
        let size = entry["bytes"]
            .as_u64()
            .ok_or_else(|| error("Component size missing"))?;
        let hash = entry["sha256"]
            .as_str()
            .ok_or_else(|| error("Component SHA missing"))?;
        if !FILES.contains(&path) || size == 0 || !valid_sha(hash) || files.contains_key(path) {
            return Err(error(
                "Unexpected, empty, duplicated or unverified component file",
            ));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| error("Component size overflow"))?;
        if total > MAX_UNPACKED_BYTES {
            return Err(error("Component exceeds unpacked size limit"));
        }
        files.insert(path.to_string(), (size, hash.to_string()));
    }
    if files.len() != FILES.len() {
        return Err(error("Component file set is incomplete"));
    }
    Ok(files)
}

pub fn validate_directory(directory: &Path) -> Result<(), InterpolationError> {
    reject_links(directory)?;
    validate_file_set(directory, directory)?;
    let manifest_path = directory.join(MANIFEST);
    reject_links(&manifest_path)?;
    let mut bytes = Vec::new();
    File::open(&manifest_path)
        .map_err(|cause| io_error("Open component manifest", cause))?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|cause| io_error("Read component manifest", cause))?;
    for (name, (size, hash)) in parse_manifest(&bytes)? {
        let path = directory.join(name);
        for ancestor in path
            .ancestors()
            .take_while(|path| path.starts_with(directory))
        {
            reject_links(ancestor)?;
        }
        let metadata =
            fs::metadata(&path).map_err(|cause| io_error("Inspect component file", cause))?;
        if !metadata.is_file() || metadata.len() != size || sha256(&path)? != hash {
            return Err(error(format!(
                "Component file integrity check failed: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_file_set(root: &Path, directory: &Path) -> Result<(), InterpolationError> {
    for entry in
        fs::read_dir(directory).map_err(|cause| io_error("Inspect component files", cause))?
    {
        let entry = entry.map_err(|cause| io_error("Inspect component files", cause))?;
        reject_links(&entry.path())?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|cause| io_error("Resolve component file", cause))?
            .to_string_lossy()
            .replace('\\', "/");
        if entry
            .file_type()
            .map_err(|cause| io_error("Inspect component file type", cause))?
            .is_dir()
        {
            if !matches!(
                relative.as_str(),
                "frame-interpolation" | "frame-interpolation/models" | "licenses"
            ) {
                return Err(error("Unexpected directory in the installed component"));
            }
            validate_file_set(root, &path)?;
        } else if relative != MANIFEST && !FILES.contains(&relative.as_str()) {
            return Err(error("Unexpected file in the installed component"));
        }
    }
    Ok(())
}

/// Dedicated resumable-download cache, never shared with app updates or media.
pub fn download_directory(cache_root: &Path) -> Result<PathBuf, InterpolationError> {
    let paths = Paths::new(cache_root)?;
    reject_existing_ancestors(&paths.downloads)?;
    fs::create_dir_all(&paths.downloads)
        .map_err(|cause| io_error("Create component download cache", cause))?;
    reject_links(&paths.downloads)?;
    Ok(paths.downloads)
}

fn read_action(paths: &Paths) -> Result<Option<Value>, InterpolationError> {
    if !exists(&paths.pending)? {
        return Ok(None);
    }
    reject_links(&paths.pending)?;
    let file = paths.pending.join("action.json");
    reject_links(&file)?;
    let mut bytes = Vec::new();
    File::open(file)
        .map_err(|cause| io_error("Read pending operation", cause))?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|cause| io_error("Read pending operation", cause))?;
    if bytes.len() > 4096 {
        return Err(error("Pending operation is too large"));
    }
    let action: Value = serde_json::from_slice(&bytes)
        .map_err(|cause| io_error("Parse pending operation", cause))?;
    if action["schema"] != 1
        || !matches!(action["operation"].as_str(), Some("install" | "uninstall"))
    {
        return Err(error("Invalid pending component operation"));
    }
    Ok(Some(action))
}

pub fn download_lease(cache_root: &Path) -> Result<File, InterpolationError> {
    let paths = Paths::new(cache_root)?;
    managed_root(&paths)?;
    lock(&paths, "download.lock", true)
}

pub fn status(cache_root: &Path) -> Result<Value, InterpolationError> {
    let paths = Paths::new(cache_root)?;
    reject_existing_ancestors(&paths.root)?;
    let action = read_action(&paths)?;
    let installed = exists(&paths.active)?;
    if installed {
        reject_links(&paths.active)?;
        if !paths.active.is_dir() {
            return Err(error("Installed component is not a directory"));
        }
    }
    Ok(
        json!({"installed": installed, "supported": SUPPORTED, "version": "1",
        "pendingOperation": action.as_ref().and_then(|value| value["operation"].as_str()),
        "restartRequired": action.is_some(), "installPath": paths.active,
        "releaseTag": RELEASE_TAG, "assetName": ASSET_NAME}),
    )
}

fn remove_tree(path: &Path) -> Result<(), InterpolationError> {
    if !exists(path)? {
        return Ok(());
    }
    reject_links(path)?;
    // Reject reparse points recursively before removing any managed content.
    for entry in fs::read_dir(path).map_err(|cause| io_error("Inspect component cleanup", cause))? {
        let entry = entry.map_err(|cause| io_error("Inspect component cleanup", cause))?;
        reject_links(&entry.path())?;
        if entry
            .file_type()
            .map_err(|cause| io_error("Inspect component cleanup", cause))?
            .is_dir()
        {
            check_tree(&entry.path())?;
        }
    }
    fs::remove_dir_all(path).map_err(|cause| io_error("Remove managed component directory", cause))
}

fn check_tree(path: &Path) -> Result<(), InterpolationError> {
    reject_links(path)?;
    for entry in fs::read_dir(path).map_err(|cause| io_error("Inspect managed tree", cause))? {
        let entry = entry.map_err(|cause| io_error("Inspect managed tree", cause))?;
        reject_links(&entry.path())?;
        if entry
            .file_type()
            .map_err(|cause| io_error("Inspect managed tree", cause))?
            .is_dir()
        {
            check_tree(&entry.path())?;
        }
    }
    Ok(())
}

/// Called before loading any RIFE DLL. Hold the returned read lease for the
/// entire process: the native DLL intentionally pins itself until process exit.
pub fn startup(cache_root: &Path) -> Result<(PathBuf, File), InterpolationError> {
    if !SUPPORTED {
        return Err(InterpolationError::new(
            "frame_interpolation_platform_unsupported",
            "Components require Windows x64",
        ));
    }
    let paths = Paths::new(cache_root)?;
    managed_root(&paths)?;
    {
        let _operation = lock(&paths, "operations.lock", true)?;
        if let Some(action) = read_action(&paths)? {
            let _exclusive = lock(&paths, "use.lock", true)?;
            match action["operation"].as_str() {
                Some("install") => {
                    let hash = action["manifestSha256"]
                        .as_str()
                        .filter(|hash| valid_sha(hash))
                        .ok_or_else(|| error("Pending install manifest SHA missing"))?;
                    let incoming = paths.pending.join("runtime");
                    if exists(&incoming)? {
                        if exists(&paths.active)? {
                            return Err(error(
                                "Existing component must be uninstalled before installation",
                            ));
                        }
                        validate_directory(&incoming)?;
                        if sha256(&incoming.join(MANIFEST))? != hash {
                            return Err(error("Pending install manifest changed"));
                        }
                        fs::rename(incoming, &paths.active)
                            .map_err(|cause| io_error("Activate component", cause))?;
                    } else {
                        // Explicit crash recovery after the atomic directory move.
                        validate_directory(&paths.active)?;
                        if sha256(&paths.active.join(MANIFEST))? != hash {
                            return Err(error(
                                "Interrupted install does not match the pending operation",
                            ));
                        }
                    }
                }
                Some("uninstall") => {
                    let _download = lock(&paths, "download.lock", true)?;
                    // Check every cleanup target and its ancestors before the first delete.
                    for target in [&paths.active, &paths.cache, &paths.downloads] {
                        reject_existing_ancestors(target)?;
                        if exists(target)? {
                            check_tree(target)?;
                        }
                    }
                    remove_tree(&paths.active)?;
                    remove_tree(&paths.cache)?;
                    remove_tree(&paths.downloads)?;
                }
                _ => return Err(error("Unsupported component operation")),
            }
            remove_tree(&paths.pending)?;
        }
        let lease = lock(&paths, "use.lock", false)?;
        Ok((paths.active, lease))
    }
}

fn commit_action(
    paths: &Paths,
    staging: tempfile::TempDir,
    action: Value,
) -> Result<(), InterpolationError> {
    let bytes = serde_json::to_vec_pretty(&action)
        .map_err(|cause| io_error("Encode component operation", cause))?;
    let mut file = File::create(staging.path().join("action.json"))
        .map_err(|cause| io_error("Create component operation", cause))?;
    file.write_all(&bytes)
        .map_err(|cause| io_error("Write component operation", cause))?;
    file.sync_all()
        .map_err(|cause| io_error("Sync component operation", cause))?;
    drop(file);
    fs::rename(staging.path(), &paths.pending)
        .map_err(|cause| io_error("Queue component operation", cause))?;
    // Ownership moved atomically to pending; the old staging name no longer
    // exists and must not be passed to TempDir's recursive cleanup.
    let _old_name = staging.keep();
    Ok(())
}

pub fn queue_uninstall(cache_root: &Path) -> Result<(), InterpolationError> {
    let paths = Paths::new(cache_root)?;
    managed_root(&paths)?;
    let _operation = lock(&paths, "operations.lock", true)?;
    if exists(&paths.pending)? {
        return Err(error(
            "A component operation is already pending; restart first",
        ));
    }
    if !paths.active.is_dir() {
        return Err(error("No component is installed"));
    }
    let staging = tempfile::Builder::new()
        .prefix("operation-")
        .tempdir_in(&paths.root)
        .map_err(|cause| io_error("Create uninstall staging", cause))?;
    commit_action(
        &paths,
        staging,
        json!({"schema": 1, "operation": "uninstall"}),
    )
}

pub fn queue_install(
    cache_root: &Path,
    archive_path: &Path,
    archive_sha256: &str,
) -> Result<(), InterpolationError> {
    if !valid_sha(archive_sha256)
        || fs::metadata(archive_path)
            .map_err(|cause| io_error("Inspect component package", cause))?
            .len()
            > MAX_PACKAGE_BYTES
        || sha256(archive_path)? != archive_sha256
    {
        return Err(error("Component package SHA or size is invalid"));
    }
    let paths = Paths::new(cache_root)?;
    managed_root(&paths)?;
    let _operation = lock(&paths, "operations.lock", true)?;
    if exists(&paths.pending)? {
        return Err(error(
            "A component operation is already pending; restart first",
        ));
    }
    if exists(&paths.active)? {
        return Err(error(
            "A component is already installed; uninstall it first",
        ));
    }
    let staging = tempfile::Builder::new()
        .prefix("operation-")
        .tempdir_in(&paths.root)
        .map_err(|cause| io_error("Create component staging", cause))?;
    let runtime = staging.path().join("runtime");
    fs::create_dir(&runtime).map_err(|cause| io_error("Create package runtime staging", cause))?;
    let mut archive = zip::ZipArchive::new(
        File::open(archive_path).map_err(|cause| io_error("Open component ZIP", cause))?,
    )
    .map_err(|cause| io_error("Parse component ZIP", cause))?;
    if archive.len() != FILES.len() + 1 {
        return Err(error("Component ZIP file count is invalid"));
    }
    let mut manifest_bytes = Vec::new();
    archive
        .by_name(MANIFEST)
        .map_err(|cause| io_error("Read ZIP manifest", cause))?
        .take(65537)
        .read_to_end(&mut manifest_bytes)
        .map_err(|cause| io_error("Read ZIP manifest", cause))?;
    let files = parse_manifest(&manifest_bytes)?;
    let mut seen = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|cause| io_error("Read ZIP entry", cause))?;
        let name = entry.name().to_string();
        if !seen.insert(name.clone())
            || entry.is_dir()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            || !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            )
        {
            return Err(error(
                "Component ZIP contains duplicates, links, directories or unsupported compression",
            ));
        }
        let expected = if name == MANIFEST {
            manifest_bytes.len() as u64
        } else {
            files
                .get(&name)
                .ok_or_else(|| error("Component ZIP contains an unexpected path"))?
                .0
        };
        if entry.size() != expected {
            return Err(error("ZIP entry size differs from the verified manifest"));
        }
        let destination = runtime.join(&name);
        fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| error("ZIP entry has no parent"))?,
        )
        .map_err(|cause| io_error("Create ZIP file parent", cause))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
            .map_err(|cause| io_error("Create ZIP output", cause))?;
        let count = std::io::copy(&mut entry.by_ref().take(expected + 1), &mut file)
            .map_err(|cause| io_error("Extract ZIP entry", cause))?;
        if count != expected {
            return Err(error(
                "ZIP decompression exceeded or truncated the declared size",
            ));
        }
        file.sync_all()
            .map_err(|cause| io_error("Sync component file", cause))?;
    }
    validate_directory(&runtime)?;
    let manifest_hash = sha256(&runtime.join(MANIFEST))?;
    commit_action(
        &paths,
        staging,
        json!({"schema": 1, "operation": "install", "manifestSha256": manifest_hash}),
    )
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn package(root: &Path, bad_path: Option<&str>, bad_hash: bool) -> (PathBuf, String) {
        let archive = root.join("component.zip");
        let hash = format!("{:x}", Sha256::digest(b"fixture"));
        let files = FILES.iter().map(|name| json!({"path": name, "bytes": 7, "sha256": if bad_hash { "0".repeat(64) } else { hash.clone() }})).collect::<Vec<_>>();
        let manifest = json!({"schema": 1, "component": "rife", "version": "1", "platform": "windows-x64", "runtimeAbi": RIFE_RUNTIME_ABI, "tensorRtVersion": RIFE_TENSORRT_VERSION, "files": files});
        let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file(MANIFEST, options).unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        for (index, name) in FILES.iter().enumerate() {
            zip.start_file(
                if index == 0 {
                    bad_path.unwrap_or(name)
                } else {
                    name
                },
                options,
            )
            .unwrap();
            zip.write_all(b"fixture").unwrap();
        }
        zip.finish().unwrap();
        let sha = sha256(&archive).unwrap();
        (archive, sha)
    }

    #[test]
    fn install_and_uninstall_apply_only_on_restart_and_preserve_unrelated_data() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("frame-interpolation");
        let (archive, sha) = package(temp.path(), None, false);
        let (_, old_lease) = startup(&cache).unwrap();
        queue_install(&cache, &archive, &sha).unwrap();
        assert_eq!(status(&cache).unwrap()["pendingOperation"], "install");
        assert_eq!(status(&cache).unwrap()["installed"], false);
        assert_eq!(
            startup(&cache).unwrap_err().code,
            "frame_interpolation_component_busy"
        );
        drop(old_lease);
        let (active, lease) = startup(&cache).unwrap();
        validate_directory(&active).unwrap();
        assert_eq!(status(&cache).unwrap()["restartRequired"], false);
        let paths = Paths::new(&cache).unwrap();
        fs::create_dir_all(&paths.cache).unwrap();
        fs::write(paths.cache.join("engine.plan"), b"engine").unwrap();
        let downloads = download_directory(&cache).unwrap();
        fs::write(downloads.join(ASSET_NAME), b"download").unwrap();
        let account = temp.path().join("account.json");
        fs::write(&account, b"preserve").unwrap();
        queue_uninstall(&cache).unwrap();
        assert!(active.exists());
        assert!(queue_install(&cache, &archive, &sha).is_err());
        assert_eq!(
            startup(&cache).unwrap_err().code,
            "frame_interpolation_component_busy"
        );
        drop(lease);
        let (_, lease) = startup(&cache).unwrap();
        assert!(!active.exists());
        assert!(!paths.cache.exists());
        assert!(!downloads.exists());
        assert_eq!(fs::read(account).unwrap(), b"preserve");
        drop(lease);
        temp.close().unwrap();
    }

    #[test]
    fn tampered_hash_and_traversal_packages_are_never_queued() {
        for (path, hash) in [
            (None, true),
            (Some("../escape.dll"), false),
            (Some("extra.dll"), false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let cache = temp.path().join("frame-interpolation");
            let (archive, sha) = package(temp.path(), path, hash);
            assert!(queue_install(&cache, &archive, &sha).is_err());
            assert_eq!(status(&cache).unwrap()["restartRequired"], false);
            assert!(!temp.path().join("escape.dll").exists());
            temp.close().unwrap();
        }
    }

    #[test]
    fn interrupted_activation_recovers_only_the_matching_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("frame-interpolation");
        let paths = Paths::new(&cache).unwrap();
        let (archive, sha) = package(temp.path(), None, false);
        queue_install(&cache, &archive, &sha).unwrap();
        fs::rename(paths.pending.join("runtime"), &paths.active).unwrap();
        let (_, lease) = startup(&cache).unwrap();
        assert!(!paths.pending.exists());
        fs::write(paths.active.join("rife_runtime.dll"), b"changed").unwrap();
        assert!(validate_directory(&paths.active).is_err());
        drop(lease);
        temp.close().unwrap();
    }

    #[test]
    fn unexpected_installed_files_and_wrong_cache_root_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("frame-interpolation");
        let (archive, sha) = package(temp.path(), None, false);
        queue_install(&cache, &archive, &sha).unwrap();
        let (active, lease) = startup(&cache).unwrap();
        fs::write(active.join("injected.dll"), b"inject").unwrap();
        assert!(validate_directory(&active).is_err());
        assert!(Paths::new(Path::new("frame-interpolation")).is_err());
        assert!(Paths::new(temp.path()).is_err());
        drop(lease);
        temp.close().unwrap();
    }

    #[test]
    fn manifest_rejects_wrong_tuple_and_incomplete_or_duplicate_file_sets() {
        let temp = tempfile::tempdir().unwrap();
        let (archive, _) = package(temp.path(), None, false);
        let mut zip = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
        let mut bytes = Vec::new();
        zip.by_name(MANIFEST)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        let manifest: Value = serde_json::from_slice(&bytes).unwrap();
        for field in [
            "schema",
            "version",
            "platform",
            "runtimeAbi",
            "tensorRtVersion",
        ] {
            let mut bad = manifest.clone();
            bad[field] = json!("wrong");
            assert!(parse_manifest(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let mut bad = manifest.clone();
        bad["files"].as_array_mut().unwrap().pop();
        assert!(parse_manifest(&serde_json::to_vec(&bad).unwrap()).is_err());
        let mut bad = manifest;
        bad["files"][1] = bad["files"][0].clone();
        assert!(parse_manifest(&serde_json::to_vec(&bad).unwrap()).is_err());
        drop(zip);
        temp.close().unwrap();
    }

    #[test]
    fn download_lock_is_cross_instance_and_blocks_uninstall_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("frame-interpolation");
        let (archive, sha) = package(temp.path(), None, false);
        queue_install(&cache, &archive, &sha).unwrap();
        let (active, lease) = startup(&cache).unwrap();
        queue_uninstall(&cache).unwrap();
        drop(lease);
        let download = download_lease(&cache).unwrap();
        assert_eq!(
            download_lease(&cache).unwrap_err().code,
            "frame_interpolation_component_busy"
        );
        assert_eq!(
            startup(&cache).unwrap_err().code,
            "frame_interpolation_component_busy"
        );
        assert!(active.exists());
        assert_eq!(status(&cache).unwrap()["pendingOperation"], "uninstall");
        drop(download);
        let (_, lease) = startup(&cache).unwrap();
        assert!(!active.exists());
        drop(lease);
        temp.close().unwrap();
    }
}
