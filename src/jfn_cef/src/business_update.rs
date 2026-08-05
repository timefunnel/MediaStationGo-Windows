//! GitHub Releases updater used by the About layer.

use parking_lot::Mutex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use url::Url;

use crate::client::{Inner, RendererValue, post_renderer_message};

const RELEASE_API_URL: &str =
    "https://api.github.com/repos/timefunnel/MediaStationGo-Windows/releases/latest";
const RELEASE_REPOSITORY_PATH: &str = "/timefunnel/MediaStationGo-Windows/releases/";
const USER_AGENT: &str = "MediaStationGo-Windows-Updater";
const CHECKSUM_ASSET_NAME: &str = "SHA256SUMS.txt";
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;
const MAX_INSTALLER_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug)]
struct ReleaseInfo {
    version: String,
    tag: String,
    release_url: String,
    asset_name: String,
    asset_url: String,
    asset_size: u64,
    sha256: String,
}

#[derive(Default)]
struct UpdateState {
    checking: bool,
    downloading: bool,
    installing: bool,
    release: Option<ReleaseInfo>,
    downloaded: Option<PathBuf>,
}

#[derive(Debug)]
struct UpdateError {
    code: &'static str,
    message: String,
}

impl UpdateError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

static STATE: LazyLock<Mutex<UpdateState>> = LazyLock::new(|| Mutex::new(UpdateState::default()));

pub(crate) fn supported() -> bool {
    cfg!(windows)
}

pub(crate) fn check_for_updates(inner: Arc<Inner>) {
    if !supported() {
        post_error(
            inner,
            UpdateError::new("unsupported_platform", "自动更新目前只支持 Windows x64"),
        );
        return;
    }

    {
        let mut state = STATE.lock();
        if state.checking || state.downloading || state.installing {
            return;
        }
        state.checking = true;
        state.release = None;
        state.downloaded = None;
    }
    post_status(Arc::clone(&inner), "checking", json!({}));

    let worker_inner = Arc::clone(&inner);
    let spawn = std::thread::Builder::new()
        .name("mediastation-update-check".to_string())
        .spawn(move || {
            let result = fetch_latest_release().and_then(|release| {
                is_newer_release(crate::APP_VERSION_FULL, &release.version)
                    .map(|newer| (release, newer))
            });

            match result {
                Ok((release, true)) => {
                    {
                        let mut state = STATE.lock();
                        state.checking = false;
                        state.release = Some(release.clone());
                    }
                    post_status(
                        worker_inner,
                        "available",
                        json!({
                            "version": release.version,
                            "tag": release.tag,
                            "releaseUrl": release.release_url,
                            "assetName": release.asset_name,
                            "assetBytes": release.asset_size,
                        }),
                    );
                }
                Ok((release, false)) => {
                    STATE.lock().checking = false;
                    post_status(
                        worker_inner,
                        "up_to_date",
                        json!({
                            "currentVersion": crate::APP_VERSION_FULL,
                            "latestVersion": release.version,
                        }),
                    );
                }
                Err(error) => {
                    STATE.lock().checking = false;
                    post_error(worker_inner, error);
                }
            }
        });

    if let Err(error) = spawn {
        STATE.lock().checking = false;
        post_error(
            inner,
            UpdateError::new(
                "worker_start_failed",
                format!("无法启动更新检查线程：{error}"),
            ),
        );
    }
}

pub(crate) fn download_update(inner: Arc<Inner>) {
    let release = {
        let mut state = STATE.lock();
        if state.downloading || state.installing {
            return;
        }
        let Some(release) = state.release.clone() else {
            drop(state);
            post_error(
                inner,
                UpdateError::new("update_not_checked", "请先检查更新"),
            );
            return;
        };
        state.downloading = true;
        release
    };

    post_status(
        Arc::clone(&inner),
        "downloading",
        json!({
            "downloadedBytes": 0,
            "totalBytes": release.asset_size,
            "percent": 0,
        }),
    );

    let worker_inner = Arc::clone(&inner);
    let spawn = std::thread::Builder::new()
        .name("mediastation-update-download".to_string())
        .spawn(move || {
            let result = download_installer(&release, &worker_inner);
            let mut state = STATE.lock();
            state.downloading = false;
            match result {
                Ok(path) => {
                    state.downloaded = Some(path);
                    drop(state);
                    post_status(
                        worker_inner,
                        "ready",
                        json!({
                            "version": release.version,
                            "assetName": release.asset_name,
                        }),
                    );
                }
                Err(error) => {
                    state.downloaded = None;
                    drop(state);
                    post_error(worker_inner, error);
                }
            }
        });

    if let Err(error) = spawn {
        STATE.lock().downloading = false;
        post_error(
            inner,
            UpdateError::new(
                "worker_start_failed",
                format!("无法启动更新下载线程：{error}"),
            ),
        );
    }
}

pub(crate) fn install_update(inner: Arc<Inner>) {
    let (release, path) = {
        let mut state = STATE.lock();
        if state.installing {
            return;
        }
        let Some(release) = state.release.clone() else {
            drop(state);
            post_error(
                inner,
                UpdateError::new("update_not_checked", "请先检查更新"),
            );
            return;
        };
        let Some(path) = state.downloaded.clone() else {
            drop(state);
            post_error(
                inner,
                UpdateError::new("update_not_downloaded", "请先下载更新"),
            );
            return;
        };
        state.installing = true;
        (release, path)
    };

    post_status(Arc::clone(&inner), "verifying", json!({}));
    let worker_inner = Arc::clone(&inner);
    let spawn = std::thread::Builder::new()
        .name("mediastation-update-install".to_string())
        .spawn(move || {
            let result =
                verify_file_sha256(&path, &release.sha256).and_then(|()| launch_installer(&path));
            match result {
                Ok(()) => {
                    post_status(
                        worker_inner,
                        "installing",
                        json!({ "version": release.version }),
                    );
                    jfn_playback::shutdown::jfn_shutdown_initiate();
                }
                Err(error) => {
                    let mut state = STATE.lock();
                    state.installing = false;
                    state.downloaded = None;
                    drop(state);
                    post_error(worker_inner, error);
                }
            }
        });

    if let Err(error) = spawn {
        STATE.lock().installing = false;
        post_error(
            inner,
            UpdateError::new(
                "worker_start_failed",
                format!("无法启动安装程序校验线程：{error}"),
            ),
        );
    }
}

fn fetch_latest_release() -> Result<ReleaseInfo, UpdateError> {
    let agent = github_agent(Duration::from_secs(45));
    let metadata = get_text(&agent, RELEASE_API_URL, MAX_METADATA_BYTES)?;
    let mut release = parse_release_metadata(&metadata)?;
    let checksum_asset_url = checksum_asset_url(&metadata)?;
    let checksums = get_text(&agent, &checksum_asset_url, MAX_CHECKSUM_BYTES)?;
    release.sha256 = checksum_for_asset(&checksums, &release.asset_name)?;
    Ok(release)
}

fn github_agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(5)
        .timeout_global(Some(timeout))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .build();
    ureq::Agent::new_with_config(config)
}

fn github_request(
    agent: &ureq::Agent,
    url: &str,
) -> Result<ureq::http::Response<ureq::Body>, UpdateError> {
    let response = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|error| {
            UpdateError::new(
                "update_request_failed",
                format!("更新服务器请求失败：{error}"),
            )
        })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(UpdateError::new(
            "update_http_error",
            format!("更新服务器返回 HTTP {status}"),
        ));
    }
    Ok(response)
}

fn get_text(agent: &ureq::Agent, url: &str, maximum_bytes: u64) -> Result<String, UpdateError> {
    let mut response = github_request(agent, url)?;
    response
        .body_mut()
        .with_config()
        .limit(maximum_bytes)
        .read_to_string()
        .map_err(|error| {
            UpdateError::new(
                "update_response_invalid",
                format!("更新服务器响应无效：{error}"),
            )
        })
}

fn parse_release_metadata(metadata: &str) -> Result<ReleaseInfo, UpdateError> {
    let payload: Value = serde_json::from_str(metadata).map_err(|error| {
        UpdateError::new(
            "release_metadata_invalid",
            format!("更新版本信息不是有效 JSON：{error}"),
        )
    })?;
    if payload.get("draft").and_then(Value::as_bool) == Some(true)
        || payload.get("prerelease").and_then(Value::as_bool) == Some(true)
    {
        return Err(UpdateError::new(
            "release_not_stable",
            "最新版本不是正式发布版本",
        ));
    }

    let tag = required_string(&payload, "tag_name")?;
    let version = stable_version_from_tag(&tag)?.to_string();
    let release_url = required_string(&payload, "html_url")?;
    validate_release_page_url(&release_url)?;
    let expected_asset_name = format!("MediaStationGo-{version}-windows-x64-setup.exe");
    let assets = payload
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| UpdateError::new("release_assets_missing", "正式版本没有发布文件"))?;
    let asset = assets
        .iter()
        .find(|asset| {
            asset
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name == expected_asset_name)
        })
        .ok_or_else(|| {
            UpdateError::new(
                "installer_asset_missing",
                format!("正式版本缺少 Windows x64 安装器：{expected_asset_name}"),
            )
        })?;
    let asset_url = required_string(asset, "browser_download_url")?;
    validate_asset_url(&asset_url, &tag, &expected_asset_name)?;
    let asset_size = asset
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| UpdateError::new("installer_size_missing", "安装器没有有效的文件大小"))?;
    if asset_size == 0 || asset_size > MAX_INSTALLER_BYTES {
        return Err(UpdateError::new(
            "installer_size_invalid",
            format!("安装器大小不在允许范围内：{asset_size} 字节"),
        ));
    }

    Ok(ReleaseInfo {
        version,
        tag,
        release_url,
        asset_name: expected_asset_name,
        asset_url,
        asset_size,
        sha256: String::new(),
    })
}

fn checksum_asset_url(metadata: &str) -> Result<String, UpdateError> {
    let payload: Value = serde_json::from_str(metadata).map_err(|error| {
        UpdateError::new(
            "release_metadata_invalid",
            format!("更新版本信息不是有效 JSON：{error}"),
        )
    })?;
    let tag = required_string(&payload, "tag_name")?;
    let assets = payload
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| UpdateError::new("release_assets_missing", "正式版本没有发布文件"))?;
    let asset = assets
        .iter()
        .find(|asset| asset.get("name").and_then(Value::as_str) == Some(CHECKSUM_ASSET_NAME))
        .ok_or_else(|| UpdateError::new("checksum_asset_missing", "正式版本缺少 SHA256SUMS.txt"))?;
    let url = required_string(asset, "browser_download_url")?;
    validate_asset_url(&url, &tag, CHECKSUM_ASSET_NAME)?;
    Ok(url)
}

fn required_string(value: &Value, field: &'static str) -> Result<String, UpdateError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| {
            UpdateError::new(
                "release_metadata_missing",
                format!("更新版本信息缺少 {field}"),
            )
        })
}

fn validate_release_page_url(raw: &str) -> Result<(), UpdateError> {
    let url = Url::parse(raw).map_err(|error| {
        UpdateError::new(
            "release_url_invalid",
            format!("正式版本页面地址无效：{error}"),
        )
    })?;
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.path().starts_with(RELEASE_REPOSITORY_PATH)
    {
        return Err(UpdateError::new(
            "release_url_untrusted",
            "正式版本页面不属于受信任的 GitHub 仓库",
        ));
    }
    Ok(())
}

fn validate_asset_url(raw: &str, tag: &str, asset_name: &str) -> Result<(), UpdateError> {
    let url = Url::parse(raw).map_err(|error| {
        UpdateError::new("asset_url_invalid", format!("发布文件地址无效：{error}"))
    })?;
    let expected_path = format!("{RELEASE_REPOSITORY_PATH}download/{tag}/{asset_name}");
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || url.path() != expected_path
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(UpdateError::new(
            "asset_url_untrusted",
            "发布文件不属于受信任的 GitHub Release",
        ));
    }
    Ok(())
}

fn checksum_for_asset(checksums: &str, asset_name: &str) -> Result<String, UpdateError> {
    for line in checksums.lines() {
        let mut fields = line.split_whitespace();
        let Some(hash) = fields.next() else { continue };
        let Some(name) = fields.next() else { continue };
        if name == asset_name
            && fields.next().is_none()
            && hash.len() == 64
            && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Ok(hash.to_ascii_lowercase());
        }
    }
    Err(UpdateError::new(
        "installer_checksum_missing",
        format!("SHA256SUMS.txt 中缺少安装器校验值：{asset_name}"),
    ))
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct VersionTriplet {
    major: u64,
    minor: u64,
    patch: u64,
}

impl std::fmt::Display for VersionTriplet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn stable_version_from_tag(tag: &str) -> Result<VersionTriplet, UpdateError> {
    let raw = tag
        .strip_prefix('v')
        .ok_or_else(|| UpdateError::new("release_version_invalid", "正式版本标签必须以 v 开头"))?;
    parse_version_triplet(raw)
}

fn parse_version_triplet(raw: &str) -> Result<VersionTriplet, UpdateError> {
    let parts = raw.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(UpdateError::new(
            "release_version_invalid",
            format!("版本号不是 major.minor.patch：{raw}"),
        ));
    }
    Ok(VersionTriplet {
        major: parts[0].parse().map_err(|error| {
            UpdateError::new("release_version_invalid", format!("主版本号无效：{error}"))
        })?,
        minor: parts[1].parse().map_err(|error| {
            UpdateError::new("release_version_invalid", format!("次版本号无效：{error}"))
        })?,
        patch: parts[2].parse().map_err(|error| {
            UpdateError::new(
                "release_version_invalid",
                format!("修订版本号无效：{error}"),
            )
        })?,
    })
}

fn is_newer_release(current: &str, latest: &str) -> Result<bool, UpdateError> {
    let current_without_build = current.split('+').next().unwrap_or(current);
    let current_is_prerelease = current_without_build.contains('-');
    let current_base = current_without_build
        .split('-')
        .next()
        .unwrap_or(current_without_build);
    let current_version = parse_version_triplet(current_base)?;
    let latest_version = parse_version_triplet(latest)?;
    Ok(latest_version > current_version
        || (latest_version == current_version && current_is_prerelease))
}

fn download_installer(release: &ReleaseInfo, inner: &Arc<Inner>) -> Result<PathBuf, UpdateError> {
    let update_dir = jfn_paths::cache_dir().join("updates");
    fs::create_dir_all(&update_dir).map_err(|error| {
        UpdateError::new(
            "update_directory_failed",
            format!("无法创建更新目录：{error}"),
        )
    })?;
    let destination = update_dir.join(&release.asset_name);
    if destination.is_file() && verify_file_sha256(&destination, &release.sha256).is_ok() {
        return Ok(destination);
    }

    let temporary = update_dir.join(format!("{}.download", release.asset_name));
    if temporary.exists() {
        fs::remove_file(&temporary).map_err(|error| {
            UpdateError::new(
                "update_temp_cleanup_failed",
                format!("无法清理未完成的更新文件：{error}"),
            )
        })?;
    }

    let result = download_installer_to(&temporary, release, inner);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if destination.exists() {
        fs::remove_file(&destination).map_err(|error| {
            UpdateError::new(
                "update_replace_failed",
                format!("无法替换旧的更新安装器：{error}"),
            )
        })?;
    }
    fs::rename(&temporary, &destination).map_err(|error| {
        UpdateError::new(
            "update_finalize_failed",
            format!("无法保存更新安装器：{error}"),
        )
    })?;
    cleanup_old_installers(&update_dir, &destination);
    Ok(destination)
}

fn download_installer_to(
    destination: &Path,
    release: &ReleaseInfo,
    inner: &Arc<Inner>,
) -> Result<(), UpdateError> {
    let agent = github_agent(Duration::from_secs(20 * 60));
    let response = github_request(&agent, &release.asset_url)?;
    let mut reader = response.into_body().into_reader();
    let mut file = File::create(destination).map_err(|error| {
        UpdateError::new(
            "update_file_create_failed",
            format!("无法创建更新文件：{error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut downloaded = 0_u64;
    let mut last_percent = 0_u64;

    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            UpdateError::new(
                "update_download_failed",
                format!("更新安装器下载失败：{error}"),
            )
        })?;
        if count == 0 {
            break;
        }
        downloaded = downloaded.saturating_add(count as u64);
        if downloaded > MAX_INSTALLER_BYTES || downloaded > release.asset_size {
            return Err(UpdateError::new(
                "installer_size_invalid",
                "下载的安装器超过正式版本声明的大小",
            ));
        }
        file.write_all(&buffer[..count]).map_err(|error| {
            UpdateError::new(
                "update_file_write_failed",
                format!("无法写入更新安装器：{error}"),
            )
        })?;
        hasher.update(&buffer[..count]);

        let percent = downloaded.saturating_mul(100) / release.asset_size;
        if percent > last_percent {
            last_percent = percent;
            post_status(
                Arc::clone(inner),
                "downloading",
                json!({
                    "downloadedBytes": downloaded,
                    "totalBytes": release.asset_size,
                    "percent": percent,
                }),
            );
        }
    }

    if downloaded != release.asset_size {
        return Err(UpdateError::new(
            "installer_size_mismatch",
            format!(
                "安装器大小不匹配：应为 {} 字节，实际为 {downloaded} 字节",
                release.asset_size
            ),
        ));
    }
    file.sync_all().map_err(|error| {
        UpdateError::new(
            "update_file_sync_failed",
            format!("无法完整保存更新安装器：{error}"),
        )
    })?;
    let actual = hex_digest(&hasher.finalize());
    if actual != release.sha256 {
        return Err(UpdateError::new(
            "installer_checksum_mismatch",
            "安装器 SHA-256 校验失败，文件不会被执行",
        ));
    }
    Ok(())
}

fn verify_file_sha256(path: &Path, expected: &str) -> Result<(), UpdateError> {
    let mut file = File::open(path).map_err(|error| {
        UpdateError::new(
            "update_file_missing",
            format!("无法打开已下载的安装器：{error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            UpdateError::new(
                "update_file_read_failed",
                format!("无法校验已下载的安装器：{error}"),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if hex_digest(&hasher.finalize()) != expected {
        return Err(UpdateError::new(
            "installer_checksum_mismatch",
            "安装器 SHA-256 校验失败，文件不会被执行",
        ));
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn cleanup_old_installers(update_dir: &Path, keep: &Path) {
    let Ok(entries) = fs::read_dir(update_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep || !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("MediaStationGo-") && name.ends_with("-windows-x64-setup.exe") {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(windows)]
fn launch_installer(path: &Path) -> Result<(), UpdateError> {
    std::process::Command::new(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            UpdateError::new(
                "installer_launch_failed",
                format!("无法启动更新安装器：{error}"),
            )
        })
}

#[cfg(not(windows))]
fn launch_installer(_path: &Path) -> Result<(), UpdateError> {
    Err(UpdateError::new(
        "unsupported_platform",
        "自动更新目前只支持 Windows x64",
    ))
}

fn post_status(inner: Arc<Inner>, status: &str, payload: Value) {
    let _ = post_renderer_message(
        inner,
        "appUpdateStatus",
        vec![
            RendererValue::String(status.to_string()),
            RendererValue::String(payload.to_string()),
        ],
    );
}

fn post_error(inner: Arc<Inner>, error: UpdateError) {
    jfn_logging::log(
        jfn_logging::CATEGORY_CEF,
        jfn_logging::LEVEL_ERROR,
        &format!(
            "MediaStation update failed: {}: {}",
            error.code, error.message
        ),
    );
    post_status(
        inner,
        "error",
        json!({ "code": error.code, "message": error.message }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_release_beats_matching_development_build() {
        assert!(is_newer_release("0.1.0-dev+79db526", "0.1.0").unwrap());
        assert!(!is_newer_release("0.1.0", "0.1.0").unwrap());
        assert!(!is_newer_release("0.2.0-dev+abcdef0", "0.1.9").unwrap());
    }

    #[test]
    fn checksum_requires_exact_asset_name() {
        let hash = "a".repeat(64);
        let sums =
            format!("{hash}  MediaStationGo-0.1.1-windows-x64-setup.exe\n{hash}  other.exe\n");
        assert_eq!(
            checksum_for_asset(&sums, "MediaStationGo-0.1.1-windows-x64-setup.exe").unwrap(),
            hash
        );
        assert!(checksum_for_asset(&sums, "missing.exe").is_err());
    }

    #[test]
    fn release_metadata_requires_expected_repository_assets() {
        let metadata = json!({
            "draft": false,
            "prerelease": false,
            "tag_name": "v0.1.1",
            "html_url": "https://github.com/timefunnel/MediaStationGo-Windows/releases/tag/v0.1.1",
            "assets": [
                {
                    "name": "MediaStationGo-0.1.1-windows-x64-setup.exe",
                    "browser_download_url": "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/v0.1.1/MediaStationGo-0.1.1-windows-x64-setup.exe",
                    "size": 123456,
                },
                {
                    "name": "SHA256SUMS.txt",
                    "browser_download_url": "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/v0.1.1/SHA256SUMS.txt",
                    "size": 300,
                }
            ]
        })
        .to_string();
        let release = parse_release_metadata(&metadata).unwrap();
        assert_eq!(release.version, "0.1.1");
        assert_eq!(release.asset_size, 123456);
        assert!(checksum_asset_url(&metadata).is_ok());
    }
}
