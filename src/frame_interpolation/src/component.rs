//! Versioned, validated per-user native component packages. Mutations apply on next startup.

use super::components::ComponentKind;
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
const STAGING_OWNER: &str = "staging-owner.txt";
const STAGING_IDENTITY: &[u8] = b"MediaStationGo RIFE operation staging v1\n";
pub(crate) const FILES: &[&str] = &[
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
    kind: ComponentKind,
    root: PathBuf,
    active: PathBuf,
    pending: PathBuf,
    cache: PathBuf,
    downloads: PathBuf,
}

impl Paths {
    fn new(cache_root: &Path) -> Result<Self, InterpolationError> {
        let kind = ComponentKind::ALL
            .into_iter()
            .find(|kind| {
                cache_root.file_name().and_then(|name| name.to_str()) == Some(kind.cache_name())
            })
            .ok_or_else(|| error("Unknown component cache directory"))?;
        if !cache_root.is_absolute() {
            return Err(error(
                "Component cache root must be an absolute frame-interpolation directory",
            ));
        }
        let parent = cache_root
            .parent()
            .ok_or_else(|| error("Component cache has no parent"))?;
        let root = parent.join("components").join(kind.id());
        Ok(Self {
            kind,
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

pub fn sha256(path: &Path) -> Result<String, InterpolationError> {
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

fn parse_manifest_for(
    kind: ComponentKind,
    bytes: &[u8],
) -> Result<BTreeMap<String, (u64, String)>, InterpolationError> {
    if bytes.len() > 64 * 1024 {
        return Err(error("Component manifest exceeds 64 KiB"));
    }
    let manifest: Value = serde_json::from_slice(bytes)
        .map_err(|cause| io_error("Parse component manifest", cause))?;
    if manifest["schema"] != 1
        || manifest["component"] != kind.id()
        || manifest["version"] != "1"
        || manifest["platform"] != "windows-x64"
        || (kind == ComponentKind::Rife
            && (manifest["runtimeAbi"] != RIFE_RUNTIME_ABI
                || manifest["tensorRtVersion"] != RIFE_TENSORRT_VERSION))
        || (kind != ComponentKind::Rife
            && (manifest["runtimeAbi"] != 1
                || manifest["providerVersion"] != kind.runtime_version()))
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
        if !kind.files().contains(&path)
            || size == 0
            || !valid_sha(hash)
            || files.contains_key(path)
        {
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
    if files.len() != kind.files().len() {
        return Err(error("Component file set is incomplete"));
    }
    Ok(files)
}

fn validate_directory_for(kind: ComponentKind, directory: &Path) -> Result<(), InterpolationError> {
    reject_links(directory)?;
    validate_file_set(kind, directory, directory)?;
    for (name, (size, hash)) in manifest_files(kind, directory)? {
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

fn manifest_files(
    kind: ComponentKind,
    directory: &Path,
) -> Result<BTreeMap<String, (u64, String)>, InterpolationError> {
    let manifest_path = directory.join(MANIFEST);
    reject_links(&manifest_path)?;
    let mut bytes = Vec::new();
    File::open(&manifest_path)
        .map_err(|cause| io_error("Open component manifest", cause))?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|cause| io_error("Read component manifest", cause))?;
    parse_manifest_for(kind, &bytes)
}

fn validate_file_set(
    kind: ComponentKind,
    root: &Path,
    directory: &Path,
) -> Result<(), InterpolationError> {
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
            if !kind
                .files()
                .iter()
                .any(|name| name.starts_with(&(relative.clone() + "/")))
            {
                return Err(error("Unexpected directory in the installed component"));
            }
            validate_file_set(kind, root, &path)?;
        } else if relative != MANIFEST && !kind.files().contains(&relative.as_str()) {
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
        "id": paths.kind.id(), "name": paths.kind.name(),
        "releaseTag": paths.kind.release_tag(), "assetName": paths.kind.asset_name()}),
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

// Only call while holding operations.lock, including staging creation: no
// live install can be mistaken for an abandoned operation in another process.
fn operation_staging(paths: &Paths) -> Result<tempfile::TempDir, InterpolationError> {
    let staging = tempfile::Builder::new()
        .prefix("operation-")
        .tempdir_in(&paths.root)
        .map_err(|cause| io_error("Create component operation staging", cause))?;
    write_staging_identity(staging.path())?;
    Ok(staging)
}

fn write_staging_identity(directory: &Path) -> Result<(), InterpolationError> {
    let mut marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(STAGING_OWNER))
        .map_err(|cause| io_error("Create component staging identity", cause))?;
    marker
        .write_all(STAGING_IDENTITY)
        .map_err(|cause| io_error("Write component staging identity", cause))?;
    marker
        .sync_all()
        .map_err(|cause| io_error("Sync component staging identity", cause))?;
    // No payload is created until this identity has been durably written.
    Ok(())
}

fn validate_staging_contents(
    directory: &Path,
    allow_cancelled: bool,
    kind: ComponentKind,
) -> Result<(), InterpolationError> {
    reject_links(directory)?;
    for child in fs::read_dir(directory)
        .map_err(|cause| io_error("Inspect abandoned staging contents", cause))?
    {
        let child = child.map_err(|cause| io_error("Inspect staging content", cause))?;
        reject_links(&child.path())?;
        let file_type = child
            .file_type()
            .map_err(|cause| io_error("Inspect staging content type", cause))?;
        match child.file_name().to_str() {
            Some("runtime") if file_type.is_dir() => {
                validate_file_set(kind, &child.path(), &child.path())?;
            }
            Some("cancelled-install") if allow_cancelled && file_type.is_dir() => {
                validate_staging_contents(&child.path(), false, kind)?;
            }
            Some(STAGING_OWNER | "action.json") if file_type.is_file() => {}
            _ => return Err(error("Unexpected content in abandoned component staging")),
        }
    }
    Ok(())
}

fn remove_owned_staging(directory: &Path, kind: ComponentKind) -> Result<(), InterpolationError> {
    validate_staging_contents(directory, true, kind)?;
    for child in fs::read_dir(directory)
        .map_err(|cause| io_error("Inspect component staging cleanup", cause))?
    {
        let child = child.map_err(|cause| io_error("Inspect staging cleanup entry", cause))?;
        if child.file_name() == STAGING_OWNER {
            continue;
        }
        if child
            .file_type()
            .map_err(|cause| io_error("Inspect staging cleanup type", cause))?
            .is_dir()
        {
            remove_tree(&child.path())?;
        } else {
            fs::remove_file(child.path())
                .map_err(|cause| io_error("Remove staging file", cause))?;
        }
    }
    // Keep ownership after partial/blocked cleanup, so the next attempt can
    // safely reclaim the remaining payload even if its manifest was deleted.
    fs::remove_file(directory.join(STAGING_OWNER))
        .map_err(|cause| io_error("Remove staging identity", cause))?;
    fs::remove_dir(directory).map_err(|cause| io_error("Remove empty component staging", cause))
}

fn cleanup_abandoned_staging(paths: &Paths) -> Result<(), InterpolationError> {
    let mut owned = Vec::new();
    for entry in fs::read_dir(&paths.root)
        .map_err(|cause| io_error("Inspect abandoned component staging", cause))?
    {
        let entry = entry.map_err(|cause| io_error("Inspect component staging", cause))?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with("operation-")
        {
            continue;
        }
        let path = entry.path();
        reject_links(&path)?;
        if !entry
            .file_type()
            .map_err(|cause| io_error("Inspect component staging type", cause))?
            .is_dir()
        {
            continue;
        }
        let marker = path.join(STAGING_OWNER);
        let runtime = path.join("runtime");
        if exists(&marker)? {
            reject_links(&marker)?;
            let mut bytes = Vec::new();
            File::open(marker)
                .map_err(|cause| io_error("Read component staging identity", cause))?
                .take(STAGING_IDENTITY.len() as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|cause| io_error("Read component staging identity", cause))?;
            if bytes != STAGING_IDENTITY {
                // Incomplete/foreign identities cannot establish ownership.
                continue;
            }
        } else if exists(&runtime.join(MANIFEST))? {
            // Legacy staging had no identity marker. A complete, supported
            // manifest plus only known payload paths identifies its ownership;
            // payload files may still be incomplete after an interrupted copy.
            reject_links(&runtime)?;
            manifest_files(paths.kind, &runtime)?;
        } else {
            // A matching directory prefix alone never authorizes deletion.
            continue;
        }
        validate_staging_contents(&path, true, paths.kind)?;
        owned.push(path);
    }
    // Validate all candidates before deleting any of them.
    for path in owned {
        if !exists(&path.join(STAGING_OWNER))? {
            write_staging_identity(&path)?;
        }
        remove_owned_staging(&path, paths.kind)?;
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
        cleanup_abandoned_staging(&paths)?;
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
                        validate_directory_for(paths.kind, &incoming)?;
                        if sha256(&incoming.join(MANIFEST))? != hash {
                            return Err(error("Pending install manifest changed"));
                        }
                        fs::rename(incoming, &paths.active)
                            .map_err(|cause| io_error("Activate component", cause))?;
                    } else {
                        // Explicit crash recovery after the atomic directory move.
                        validate_directory_for(paths.kind, &paths.active)?;
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

/// Explicitly discard only the queued install. An already-activated component
/// and all Engine/download caches are preserved, even after activation crashed.
pub fn cancel_pending_install(cache_root: &Path) -> Result<(), InterpolationError> {
    let paths = Paths::new(cache_root)?;
    managed_root(&paths)?;
    let _operation = lock(&paths, "operations.lock", true)?;
    let action =
        read_action(&paths)?.ok_or_else(|| error("No component installation is pending"))?;
    if action["operation"] != "install" {
        return Err(error("Only a pending component install can be cancelled"));
    }
    validate_staging_contents(&paths.pending, false, paths.kind)?;
    let staging = operation_staging(&paths)?;
    fs::rename(&paths.pending, staging.path().join("cancelled-install"))
        .map_err(|cause| io_error("Cancel pending component installation", cause))?;
    // The queue is removed atomically; preserve the owned staging directory
    // if cleanup fails, rather than leaving a half-deleted pending action.
    let directory = staging.keep();
    remove_owned_staging(&directory, paths.kind)
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
    cleanup_abandoned_staging(&paths)?;
    let staging = operation_staging(&paths)?;
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
    cleanup_abandoned_staging(&paths)?;
    let staging = operation_staging(&paths)?;
    let runtime = staging.path().join("runtime");
    fs::create_dir(&runtime).map_err(|cause| io_error("Create package runtime staging", cause))?;
    let mut archive = zip::ZipArchive::new(
        File::open(archive_path).map_err(|cause| io_error("Open component ZIP", cause))?,
    )
    .map_err(|cause| io_error("Parse component ZIP", cause))?;
    if archive.len() != paths.kind.files().len() + 1 {
        return Err(error("Component ZIP file count is invalid"));
    }
    let mut manifest_bytes = Vec::new();
    archive
        .by_name(MANIFEST)
        .map_err(|cause| io_error("Read ZIP manifest", cause))?
        .take(65537)
        .read_to_end(&mut manifest_bytes)
        .map_err(|cause| io_error("Read ZIP manifest", cause))?;
    let files = parse_manifest_for(paths.kind, &manifest_bytes)?;
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
    validate_directory_for(paths.kind, &runtime)?;
    let manifest_hash = sha256(&runtime.join(MANIFEST))?;
    commit_action(
        &paths,
        staging,
        json!({"schema": 1, "operation": "install", "manifestSha256": manifest_hash}),
    )
}

pub fn validate_directory(directory: &Path) -> Result<(), InterpolationError> {
    validate_directory_for(ComponentKind::Rife, directory)
}

pub fn validate_component_directory(
    kind: ComponentKind,
    directory: &Path,
) -> Result<(), InterpolationError> {
    validate_directory_for(kind, directory)
}

pub fn prepare_runtime_cache(cache_root: &Path) -> Result<PathBuf, InterpolationError> {
    let paths = Paths::new(cache_root)?;
    reject_existing_ancestors(&paths.cache)?;
    fs::create_dir_all(&paths.cache)
        .map_err(|cause| io_error("Create native runtime cache", cause))?;
    reject_links(&paths.cache)?;
    Ok(paths.cache)
}

#[cfg(all(test, windows))]
fn parse_manifest(bytes: &[u8]) -> Result<BTreeMap<String, (u64, String)>, InterpolationError> {
    parse_manifest_for(ComponentKind::Rife, bytes)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn native_package(root: &Path, kind: ComponentKind) -> (PathBuf, String) {
        let path = root.join(format!("{}.zip", kind.id()));
        let hash = format!("{:x}", Sha256::digest(b"fixture"));
        let files = kind
            .files()
            .iter()
            .map(|name| json!({"path": name, "bytes": 7, "sha256": hash}))
            .collect::<Vec<_>>();
        let manifest = json!({"schema": 1, "component": kind.id(), "version": "1", "platform": "windows-x64", "runtimeAbi": 1, "providerVersion": kind.runtime_version(), "files": files});
        let mut archive = zip::ZipWriter::new(File::create(&path).unwrap());
        archive
            .start_file(MANIFEST, SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        for name in kind.files() {
            archive
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            archive.write_all(b"fixture").unwrap();
        }
        archive.finish().unwrap();
        let sha = sha256(&path).unwrap();
        (path, sha)
    }

    #[test]
    fn native_components_are_isolated_and_reject_a_different_provider_package() {
        let temp = tempfile::tempdir().unwrap();
        let vsr_cache = temp.path().join("rtx-video");
        let vfg_cache = temp.path().join("nvidia-vfg");
        let (vsr_zip, vsr_sha) = native_package(temp.path(), ComponentKind::RtxVideo);
        let (vfg_zip, vfg_sha) = native_package(temp.path(), ComponentKind::NvidiaVfg);
        assert!(queue_install(&vfg_cache, &vsr_zip, &vsr_sha).is_err());
        assert!(
            !status(&vfg_cache).unwrap()["restartRequired"]
                .as_bool()
                .unwrap()
        );
        queue_install(&vsr_cache, &vsr_zip, &vsr_sha).unwrap();
        queue_install(&vfg_cache, &vfg_zip, &vfg_sha).unwrap();
        let (vsr_root, vsr_lease) = startup(&vsr_cache).unwrap();
        let (vfg_root, vfg_lease) = startup(&vfg_cache).unwrap();
        assert!(vsr_root.join("nvngx_vsr.dll").is_file());
        assert!(vfg_root.join("nvVFXVideoFrameGeneration.dll").is_file());
        queue_uninstall(&vsr_cache).unwrap();
        drop(vsr_lease);
        let (_, lease) = startup(&vsr_cache).unwrap();
        drop(lease);
        assert!(!vsr_root.exists());
        assert!(vfg_root.join("nvVFXVideoFrameGeneration.dll").is_file());
        assert_eq!(status(&vfg_cache).unwrap()["pendingOperation"], Value::Null);
        drop(vfg_lease);
    }

    #[test]
    #[ignore = "requires locally packaged original NVIDIA components"]
    fn real_native_packages_install_validate_and_uninstall_without_touching_user_components() {
        let packages = PathBuf::from(
            std::env::var_os("MEDIASTATION_VIDEO_COMPONENT_DIR")
                .expect("local package directory required"),
        );
        let temp = tempfile::tempdir().unwrap();
        for kind in [
            ComponentKind::RtxVideo,
            ComponentKind::DlssNr,
            ComponentKind::NvidiaVfg,
        ] {
            let cache = temp.path().join(kind.cache_name());
            let archive = packages.join(kind.asset_name());
            queue_install(&cache, &archive, &sha256(&archive).unwrap()).unwrap();
            assert_eq!(status(&cache).unwrap()["pendingOperation"], "install");
            let (active, lease) = startup(&cache).unwrap();
            validate_component_directory(kind, &active).unwrap();
            assert_eq!(status(&cache).unwrap()["installed"], true);
            queue_uninstall(&cache).unwrap();
            drop(lease);
            let (_, lease) = startup(&cache).unwrap();
            drop(lease);
            assert!(!active.exists());
        }
    }

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

    #[test]
    fn failed_pending_install_can_be_cancelled_and_requeued() {
        for damage in ["missing-file", "bad-hash", "bad-manifest"] {
            let temp = tempfile::tempdir().unwrap();
            let cache = temp.path().join("frame-interpolation");
            let paths = Paths::new(&cache).unwrap();
            let (archive, sha) = package(temp.path(), None, false);
            queue_install(&cache, &archive, &sha).unwrap();
            let incoming = paths.pending.join("runtime");
            match damage {
                "missing-file" => fs::remove_file(incoming.join("rife_runtime.dll")).unwrap(),
                "bad-hash" => fs::write(incoming.join("rife_runtime.dll"), b"changed").unwrap(),
                _ => fs::write(incoming.join(MANIFEST), b"invalid JSON").unwrap(),
            }
            fs::create_dir_all(&paths.cache).unwrap();
            fs::write(paths.cache.join("keep.engine"), b"keep engine").unwrap();
            let downloads = download_directory(&cache).unwrap();
            fs::write(downloads.join("keep.zip"), b"keep download").unwrap();
            assert_eq!(
                startup(&cache).unwrap_err().code,
                "frame_interpolation_component_invalid"
            );
            assert_eq!(status(&cache).unwrap()["restartRequired"], true);
            cancel_pending_install(&cache).unwrap();
            assert!(!paths.pending.exists());
            assert!(!paths.active.exists());
            assert_eq!(status(&cache).unwrap()["restartRequired"], false);
            assert_eq!(
                fs::read(paths.cache.join("keep.engine")).unwrap(),
                b"keep engine"
            );
            assert_eq!(
                fs::read(downloads.join("keep.zip")).unwrap(),
                b"keep download"
            );
            queue_install(&cache, &archive, &sha).unwrap();
            let (active, lease) = startup(&cache).unwrap();
            validate_directory(&active).unwrap();
            drop(lease);
            temp.close().unwrap();
        }
    }

    #[test]
    fn cancellation_preserves_activated_runtime_and_never_cancels_uninstall() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("frame-interpolation");
        let paths = Paths::new(&cache).unwrap();
        let (archive, sha) = package(temp.path(), None, false);
        queue_install(&cache, &archive, &sha).unwrap();
        fs::rename(paths.pending.join("runtime"), &paths.active).unwrap();
        let mut action = read_action(&paths).unwrap().unwrap();
        action["manifestSha256"] = json!("0".repeat(64));
        fs::write(
            paths.pending.join("action.json"),
            serde_json::to_vec(&action).unwrap(),
        )
        .unwrap();
        assert!(startup(&cache).is_err());
        let active_lease = lock(&paths, "use.lock", false).unwrap();
        cancel_pending_install(&cache).unwrap();
        validate_directory(&paths.active).unwrap();
        assert!(!paths.pending.exists());
        assert!(cancel_pending_install(&cache).is_err());
        let (_, lease) = startup(&cache).unwrap();
        queue_uninstall(&cache).unwrap();
        assert!(cancel_pending_install(&cache).is_err());
        assert_eq!(status(&cache).unwrap()["pendingOperation"], "uninstall");
        drop(lease);
        drop(active_lease);
        let (_, lease) = startup(&cache).unwrap();
        assert!(!paths.active.exists());
        drop(lease);
        temp.close().unwrap();
    }

    fn abandoned_partial_staging(paths: &Paths) -> PathBuf {
        let _operation = lock(paths, "operations.lock", true).unwrap();
        let staging = operation_staging(paths).unwrap();
        fs::create_dir(staging.path().join("runtime")).unwrap();
        fs::write(staging.path().join("runtime/rife_runtime.dll"), b"partial").unwrap();
        // Simulate process termination: no TempDir destructor runs.
        staging.keep()
    }

    #[test]
    fn startup_and_uninstall_reclaim_only_owned_abandoned_staging() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("frame-interpolation");
        let paths = Paths::new(&cache).unwrap();
        let (archive, sha) = package(temp.path(), None, false);
        queue_install(&cache, &archive, &sha).unwrap();
        let legacy = paths.root.join("operation-legacy");
        fs::rename(&paths.pending, &legacy).unwrap();
        fs::remove_file(legacy.join(STAGING_OWNER)).unwrap();
        fs::remove_file(legacy.join("runtime/rife_runtime.dll")).unwrap();
        let marked = abandoned_partial_staging(&paths);
        let unknown = paths.root.join("operation-user-data");
        fs::create_dir(&unknown).unwrap();
        fs::write(unknown.join("keep.txt"), b"user data").unwrap();
        let incomplete = paths.root.join("operation-incomplete-marker");
        fs::create_dir(&incomplete).unwrap();
        fs::write(incomplete.join(STAGING_OWNER), b"incomplete identity").unwrap();
        let (_, lease) = startup(&cache).unwrap();
        assert!(!marked.exists());
        assert!(!legacy.exists());
        assert_eq!(fs::read(unknown.join("keep.txt")).unwrap(), b"user data");
        assert!(incomplete.exists());
        queue_install(&cache, &archive, &sha).unwrap();
        drop(lease);
        let (_, lease) = startup(&cache).unwrap();
        let next_orphan = abandoned_partial_staging(&paths);
        queue_uninstall(&cache).unwrap();
        assert!(!next_orphan.exists());
        assert!(paths.active.exists());
        drop(lease);
        let (_, lease) = startup(&cache).unwrap();
        assert!(!paths.active.exists());
        assert!(unknown.exists());
        drop(lease);
        temp.close().unwrap();
    }

    #[test]
    fn staging_cleanup_cannot_race_a_live_operation() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("frame-interpolation");
        let paths = Paths::new(&cache).unwrap();
        managed_root(&paths).unwrap();
        let operation = lock(&paths, "operations.lock", true).unwrap();
        let staging = operation_staging(&paths).unwrap().keep();
        assert_eq!(
            startup(&cache).unwrap_err().code,
            "frame_interpolation_component_busy"
        );
        assert!(staging.exists());
        assert_eq!(
            cancel_pending_install(&cache).unwrap_err().code,
            "frame_interpolation_component_busy"
        );
        drop(operation);
        let (_, lease) = startup(&cache).unwrap();
        assert!(!staging.exists());
        drop(lease);
        temp.close().unwrap();
    }

    #[test]
    fn unknown_staging_content_blocks_cleanup_before_any_delete() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("frame-interpolation");
        let paths = Paths::new(&cache).unwrap();
        managed_root(&paths).unwrap();
        let ordinary = abandoned_partial_staging(&paths);
        let unexpected = abandoned_partial_staging(&paths);
        fs::write(unexpected.join("runtime/user-note.txt"), b"preserve").unwrap();
        assert!(startup(&cache).is_err());
        assert!(ordinary.exists());
        assert_eq!(
            fs::read(unexpected.join("runtime/user-note.txt")).unwrap(),
            b"preserve"
        );
        temp.close().unwrap();
    }

    #[test]
    fn blocked_staging_cleanup_retains_ownership_until_retry() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("frame-interpolation");
        let paths = Paths::new(&cache).unwrap();
        managed_root(&paths).unwrap();
        let staging = abandoned_partial_staging(&paths);
        let blocker = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(staging.join("runtime/rife_runtime.dll"))
            .unwrap();
        assert!(startup(&cache).is_err());
        assert_eq!(
            fs::read(staging.join(STAGING_OWNER)).unwrap(),
            STAGING_IDENTITY
        );
        drop(blocker);
        let (_, lease) = startup(&cache).unwrap();
        assert!(!staging.exists());
        drop(lease);
        temp.close().unwrap();
    }
}
