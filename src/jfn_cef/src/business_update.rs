//! GitHub Releases updater used by the About layer.

use parking_lot::Mutex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

use crate::client::{Inner, RendererValue, post_renderer_message};

const GITHUB_RELEASE_API_URL: &str =
    "https://api.github.com/repos/timefunnel/MediaStationGo-Windows/releases/latest";
const RELEASE_REPOSITORY_PATH: &str = "/timefunnel/MediaStationGo-Windows/releases/";
const USER_AGENT: &str = "MediaStationGo-Windows-Updater";
const CHECKSUM_ASSET_NAME: &str = "SHA256SUMS.txt";
const PORTABLE_MARKER_NAME: &str = ".mediastation-portable";
const PORTABLE_UPDATER_NAME: &str = "mediastation-portable-updater.exe";
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 64 * 1024;
const MAX_UPDATE_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackageKind {
    Installer,
    Portable,
}

impl PackageKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Installer => "installer",
            Self::Portable => "portable",
        }
    }

    fn asset_name(self, version: &str) -> String {
        match self {
            Self::Installer => format!("MediaStationGo-{version}-windows-x64-setup.exe"),
            Self::Portable => format!("MediaStationGo-{version}-windows-x64-portable.zip"),
        }
    }
}

#[derive(Clone, Debug)]
struct ReleaseInfo {
    version: String,
    tag: String,
    release_url: String,
    asset_name: String,
    asset_url: String,
    asset_size: u64,
    sha256: String,
    package_kind: PackageKind,
}

#[derive(Clone, Debug)]
struct DownloadSource {
    label: String,
    prefix: Option<String>,
}

impl DownloadSource {
    fn direct() -> Self {
        Self {
            label: "GitHub 直连".to_string(),
            prefix: None,
        }
    }

    fn url(&self, github_url: &str) -> String {
        self.prefix.as_deref().map_or_else(
            || github_url.to_string(),
            |prefix| format!("{prefix}{github_url}"),
        )
    }
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

    let package_kind = match runtime_package_kind() {
        Ok(kind) => kind,
        Err(error) => {
            post_error(inner, error);
            return;
        }
    };

    {
        let mut state = STATE.lock();
        if state.checking || state.downloading || state.installing {
            return;
        }
        state.checking = true;
    }
    post_status(Arc::clone(&inner), "checking", json!({}));

    let worker_inner = Arc::clone(&inner);
    let spawn = std::thread::Builder::new()
        .name("mediastation-update-check".to_string())
        .spawn(move || {
            let result = fetch_latest_release(package_kind).and_then(|release| {
                is_newer_release(crate::APP_VERSION_FULL, &release.version)
                    .map(|newer| (release, newer))
            });

            match result {
                Ok((release, true)) => {
                    {
                        let mut state = STATE.lock();
                        state.checking = false;
                        state.release = Some(release.clone());
                        state.downloaded = None;
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
                            "packageKind": release.package_kind.as_str(),
                        }),
                    );
                }
                Ok((release, false)) => {
                    let mut state = STATE.lock();
                    state.checking = false;
                    state.release = None;
                    state.downloaded = None;
                    drop(state);
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
            "downloadedBytes": retained_download_bytes(&release),
            "totalBytes": release.asset_size,
            "percent": retained_download_bytes(&release).saturating_mul(100) / release.asset_size,
        }),
    );

    let worker_inner = Arc::clone(&inner);
    let spawn = std::thread::Builder::new()
        .name("mediastation-update-download".to_string())
        .spawn(move || {
            let result = download_update_package(&release, &worker_inner);
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
                            "packageKind": release.package_kind.as_str(),
                        }),
                    );
                }
                Err(error) => {
                    state.downloaded = None;
                    drop(state);
                    post_download_error(worker_inner, error, &release);
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
            let result = verify_file_sha256(&path, &release.sha256)
                .and_then(|()| launch_update_package(&path, &release));
            match result {
                Ok(()) => {
                    post_status(
                        worker_inner,
                        "installing",
                        json!({
                            "version": release.version,
                            "packageKind": release.package_kind.as_str(),
                        }),
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

fn fetch_latest_release(package_kind: PackageKind) -> Result<ReleaseInfo, UpdateError> {
    fetch_release(GITHUB_RELEASE_API_URL, package_kind)
}

fn fetch_release(
    metadata_url: &str,
    package_kind: PackageKind,
) -> Result<ReleaseInfo, UpdateError> {
    let agent = github_agent(Duration::from_secs(45));
    let metadata = get_text(&agent, metadata_url, MAX_METADATA_BYTES)?;
    let mut release = parse_release_metadata(&metadata, package_kind)?;
    let checksum_asset_url = checksum_asset_url(&metadata)?;
    // The package rotates through configured download sources, while the
    // checksum stays on GitHub so a source cannot replace both package and hash.
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

fn parse_release_metadata(
    metadata: &str,
    package_kind: PackageKind,
) -> Result<ReleaseInfo, UpdateError> {
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
    let expected_asset_name = package_kind.asset_name(&version);
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
                "update_asset_missing",
                format!("正式版本缺少当前运行模式的 Windows x64 更新包：{expected_asset_name}"),
            )
        })?;
    let asset_url = required_string(asset, "browser_download_url")?;
    validate_asset_url(&asset_url, &tag, &expected_asset_name)?;
    let asset_size = asset
        .get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| UpdateError::new("update_size_missing", "更新包没有有效的文件大小"))?;
    if asset_size == 0 || asset_size > MAX_UPDATE_PACKAGE_BYTES {
        return Err(UpdateError::new(
            "update_size_invalid",
            format!("更新包大小不在允许范围内：{asset_size} 字节"),
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
        package_kind,
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
    let github_path = format!("{RELEASE_REPOSITORY_PATH}download/{tag}/{asset_name}");
    let trusted_github = url.host_str() == Some("github.com") && url.path() == github_path;
    if url.scheme() != "https"
        || !trusted_github
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
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

fn download_sources() -> Vec<DownloadSource> {
    let mode = jfn_config::update_download_source_mode();
    let custom = jfn_config::update_download_sources();
    let (cached, expires_at) = jfn_config::cached_update_download_sources();
    let cache_valid = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => !cached.is_empty() && elapsed.as_secs() < expires_at,
        Err(error) => {
            jfn_logging::log(
                jfn_logging::CATEGORY_CEF,
                jfn_logging::LEVEL_WARN,
                &format!("Update download policy cache clock failed: {error}"),
            );
            false
        }
    };
    if !matches!(mode.as_str(), "custom" | "builtin" | "server") {
        jfn_logging::log(
            jfn_logging::CATEGORY_CEF,
            jfn_logging::LEVEL_ERROR,
            &format!("Unknown update download source mode {mode:?}; using direct download only"),
        );
    }
    download_sources_from(&mode, &custom, &cached, cache_valid)
}

fn download_sources_from(
    mode: &str,
    custom: &str,
    cached: &str,
    cache_valid: bool,
) -> Vec<DownloadSource> {
    let mut sources = Vec::new();
    match mode {
        "custom" => append_download_sources(&mut sources, custom),
        "builtin" => {
            append_download_sources(&mut sources, jfn_config::DEFAULT_UPDATE_DOWNLOAD_SOURCES)
        }
        "server" => {
            if cache_valid {
                append_download_sources(&mut sources, cached);
            }
            append_download_sources(&mut sources, jfn_config::DEFAULT_UPDATE_DOWNLOAD_SOURCES);
        }
        _ => {}
    }
    sources.push(DownloadSource::direct());
    sources
}

fn append_download_sources(sources: &mut Vec<DownloadSource>, raw_sources: &str) {
    for raw in raw_sources.lines() {
        let value = raw.trim();
        if value.is_empty() {
            continue;
        }
        if value == "direct" {
            continue;
        }
        let Some(source) = parse_download_source(value) else {
            log_invalid_download_source(value, "必须是无认证、无参数且以 / 结尾的 HTTPS 前缀");
            continue;
        };
        if sources
            .iter()
            .any(|existing: &DownloadSource| existing.prefix == source.prefix)
        {
            continue;
        }
        sources.push(source);
    }
}

fn parse_download_source(value: &str) -> Option<DownloadSource> {
    if !value.ends_with('/') {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(DownloadSource {
        label: url.host_str().unwrap_or(value).to_string(),
        prefix: Some(value.to_string()),
    })
}

fn log_invalid_download_source(value: &str, reason: &str) {
    jfn_logging::log(
        jfn_logging::CATEGORY_CEF,
        jfn_logging::LEVEL_WARN,
        &format!("Ignoring invalid update download source {value:?}: {reason}"),
    );
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
        "update_checksum_missing",
        format!("SHA256SUMS.txt 中缺少更新包校验值：{asset_name}"),
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

fn download_update_package(
    release: &ReleaseInfo,
    inner: &Arc<Inner>,
) -> Result<PathBuf, UpdateError> {
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
    if temporary.is_file()
        && fs::metadata(&temporary)
            .map_err(|error| {
                UpdateError::new(
                    "update_temp_metadata_failed",
                    format!("无法读取未完成更新文件：{error}"),
                )
            })?
            .len()
            > release.asset_size
    {
        fs::remove_file(&temporary).map_err(|error| {
            UpdateError::new(
                "update_temp_cleanup_failed",
                format!("无法清理超过正式版本大小的未完成更新文件：{error}"),
            )
        })?;
    }

    if temporary.exists() && !temporary.is_file() {
        return Err(UpdateError::new(
            "update_temp_invalid",
            "未完成更新文件路径不是普通文件",
        ));
    }

    if temporary.is_file()
        && fs::metadata(&temporary)
            .map_err(|error| {
                UpdateError::new(
                    "update_temp_metadata_failed",
                    format!("无法读取未完成更新文件：{error}"),
                )
            })?
            .len()
            == release.asset_size
    {
        if verify_file_sha256(&temporary, &release.sha256).is_ok() {
            if destination.exists() {
                fs::remove_file(&destination).map_err(|error| {
                    UpdateError::new(
                        "update_replace_failed",
                        format!("无法替换旧的更新包：{error}"),
                    )
                })?;
            }
            fs::rename(&temporary, &destination).map_err(|error| {
                UpdateError::new("update_finalize_failed", format!("无法保存更新包：{error}"))
            })?;
            cleanup_old_update_packages(&update_dir, &destination);
            return Ok(destination);
        }
        fs::remove_file(&temporary).map_err(|error| {
            UpdateError::new(
                "update_temp_cleanup_failed",
                format!("无法清理校验失败的未完成更新文件：{error}"),
            )
        })?;
    }

    let mut failures = Vec::new();
    for source in download_sources() {
        let retained = retained_download_bytes(release);
        post_status(
            Arc::clone(inner),
            "downloading",
            json!({
                "downloadedBytes": retained,
                "totalBytes": release.asset_size,
                "percent": retained.saturating_mul(100) / release.asset_size,
                "source": source.label.clone(),
            }),
        );
        match download_update_package_to(&temporary, release, &source, inner) {
            Ok(()) => {
                if destination.exists() {
                    fs::remove_file(&destination).map_err(|error| {
                        UpdateError::new(
                            "update_replace_failed",
                            format!("无法替换旧的更新包：{error}"),
                        )
                    })?;
                }
                fs::rename(&temporary, &destination).map_err(|error| {
                    UpdateError::new("update_finalize_failed", format!("无法保存更新包：{error}"))
                })?;
                cleanup_old_update_packages(&update_dir, &destination);
                return Ok(destination);
            }
            Err(error) => {
                let message = format!("{}：{}", source.label, error.message);
                jfn_logging::log(
                    jfn_logging::CATEGORY_CEF,
                    jfn_logging::LEVEL_WARN,
                    &format!("MediaStation update source failed: {message}"),
                );
                if !is_retryable_download_error(error.code) {
                    return Err(UpdateError::new(error.code, message));
                }
                if matches!(
                    error.code,
                    "update_checksum_mismatch" | "update_size_invalid"
                ) {
                    fs::remove_file(&temporary).map_err(|cleanup_error| {
                        UpdateError::new(
                            "update_temp_cleanup_failed",
                            format!("下载源返回了无效内容，且无法清理临时文件：{cleanup_error}"),
                        )
                    })?;
                }
                failures.push(message);
            }
        }
    }
    Err(UpdateError::new(
        "update_download_failed_all",
        format!("所有更新下载源均失败：{}", failures.join("；")),
    ))
}

fn download_update_package_to(
    destination: &Path,
    release: &ReleaseInfo,
    source: &DownloadSource,
    inner: &Arc<Inner>,
) -> Result<(), UpdateError> {
    let agent = github_agent(Duration::from_secs(20 * 60));
    let downloaded = retained_download_bytes(release);
    let asset_url = source.url(&release.asset_url);
    let response = download_response(&agent, &asset_url, downloaded, release.asset_size)?;
    let mut reader = response.into_body().into_reader();
    let mut file = if downloaded == 0 {
        File::create(destination).map_err(|error| {
            UpdateError::new(
                "update_file_create_failed",
                format!("无法创建更新文件：{error}"),
            )
        })?
    } else {
        OpenOptions::new()
            .append(true)
            .open(destination)
            .map_err(|error| {
                UpdateError::new(
                    "update_file_open_failed",
                    format!("无法打开未完成更新文件：{error}"),
                )
            })?
    };
    let mut hasher = hash_file(destination)?;
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut downloaded = downloaded;
    let mut last_percent = downloaded.saturating_mul(100) / release.asset_size;

    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            UpdateError::new(
                "update_download_failed",
                format!("更新包下载失败，已保留 {downloaded} 字节，可继续下载：{error}"),
            )
        })?;
        if count == 0 {
            break;
        }
        downloaded = downloaded.saturating_add(count as u64);
        if downloaded > MAX_UPDATE_PACKAGE_BYTES || downloaded > release.asset_size {
            return Err(UpdateError::new(
                "update_size_invalid",
                "下载的更新包超过正式版本声明的大小",
            ));
        }
        file.write_all(&buffer[..count]).map_err(|error| {
            UpdateError::new(
                "update_file_write_failed",
                format!("无法写入更新包：{error}"),
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
                    "source": source.label.clone(),
                }),
            );
        }
    }

    if downloaded != release.asset_size {
        return Err(UpdateError::new(
            "update_size_mismatch",
            format!(
                "更新包大小不匹配：应为 {} 字节，实际为 {downloaded} 字节",
                release.asset_size
            ),
        ));
    }
    file.sync_all().map_err(|error| {
        UpdateError::new(
            "update_file_sync_failed",
            format!("无法完整保存更新包：{error}"),
        )
    })?;
    let actual = hex_digest(&hasher.finalize());
    if actual != release.sha256 {
        return Err(UpdateError::new(
            "update_checksum_mismatch",
            "更新包 SHA-256 校验失败，文件不会被应用",
        ));
    }
    Ok(())
}

fn is_retryable_download_error(code: &'static str) -> bool {
    matches!(
        code,
        "update_request_failed"
            | "update_http_error"
            | "update_resume_unsupported"
            | "update_resume_invalid"
            | "update_download_failed"
            | "update_size_invalid"
            | "update_size_mismatch"
            | "update_checksum_mismatch"
    )
}

fn retained_download_bytes(release: &ReleaseInfo) -> u64 {
    let path = jfn_paths::cache_dir()
        .join("updates")
        .join(format!("{}.download", release.asset_name));
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() < release.asset_size => metadata.len(),
        _ => 0,
    }
}

fn download_response(
    agent: &ureq::Agent,
    url: &str,
    offset: u64,
    expected_size: u64,
) -> Result<ureq::http::Response<ureq::Body>, UpdateError> {
    let mut request = agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/octet-stream");
    if offset > 0 {
        request = request.header("Range", format!("bytes={offset}-"));
    }
    let response = request.call().map_err(|error| {
        UpdateError::new(
            "update_request_failed",
            format!("更新包下载请求失败：{error}"),
        )
    })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(UpdateError::new(
            "update_http_error",
            format!("更新包服务器返回 HTTP {status}"),
        ));
    }
    if offset > 0 {
        if status != 206 {
            return Err(UpdateError::new(
                "update_resume_unsupported",
                "下载代理未返回断点续传响应，已保留当前进度",
            ));
        }
        let content_range = response
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok());
        validate_content_range(content_range, offset, expected_size)?;
    }
    Ok(response)
}

fn validate_content_range(
    value: Option<&str>,
    expected_offset: u64,
    expected_size: u64,
) -> Result<(), UpdateError> {
    let value = value.ok_or_else(|| {
        UpdateError::new("update_resume_invalid", "断点续传响应缺少 Content-Range")
    })?;
    let bytes = value.strip_prefix("bytes ").ok_or_else(|| {
        UpdateError::new("update_resume_invalid", "断点续传响应的 Content-Range 无效")
    })?;
    let (range, total) = bytes.split_once('/').ok_or_else(|| {
        UpdateError::new("update_resume_invalid", "断点续传响应的 Content-Range 无效")
    })?;
    let (start, end) = range.split_once('-').ok_or_else(|| {
        UpdateError::new("update_resume_invalid", "断点续传响应的 Content-Range 无效")
    })?;
    let start = start
        .parse::<u64>()
        .map_err(|_| UpdateError::new("update_resume_invalid", "断点续传响应的起始位置无效"))?;
    let end = end
        .parse::<u64>()
        .map_err(|_| UpdateError::new("update_resume_invalid", "断点续传响应的结束位置无效"))?;
    let total = total
        .parse::<u64>()
        .map_err(|_| UpdateError::new("update_resume_invalid", "断点续传响应的总大小无效"))?;
    if start != expected_offset || end < start || end >= expected_size || total != expected_size {
        return Err(UpdateError::new(
            "update_resume_invalid",
            "断点续传响应与正式版本大小不一致，已保留当前进度",
        ));
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<Sha256, UpdateError> {
    let mut file = File::open(path).map_err(|error| {
        UpdateError::new(
            "update_file_read_failed",
            format!("无法读取未完成更新文件：{error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            UpdateError::new(
                "update_file_read_failed",
                format!("无法读取未完成更新文件：{error}"),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher)
}

fn verify_file_sha256(path: &Path, expected: &str) -> Result<(), UpdateError> {
    let mut file = File::open(path).map_err(|error| {
        UpdateError::new(
            "update_file_missing",
            format!("无法打开已下载的更新包：{error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| {
            UpdateError::new(
                "update_file_read_failed",
                format!("无法校验已下载的更新包：{error}"),
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if hex_digest(&hasher.finalize()) != expected {
        return Err(UpdateError::new(
            "update_checksum_mismatch",
            "更新包 SHA-256 校验失败，文件不会被应用",
        ));
    }
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn cleanup_old_update_packages(update_dir: &Path, keep: &Path) {
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
        if name.starts_with("MediaStationGo-")
            && (name.ends_with("-windows-x64-setup.exe")
                || name.ends_with("-windows-x64-portable.zip"))
        {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(windows)]
fn runtime_package_kind() -> Result<PackageKind, UpdateError> {
    let executable = std::env::current_exe().map_err(|error| {
        UpdateError::new(
            "executable_path_failed",
            format!("无法确认当前应用目录：{error}"),
        )
    })?;
    let install_dir = executable.parent().ok_or_else(|| {
        UpdateError::new("executable_path_failed", "当前应用程序没有有效的安装目录")
    })?;
    Ok(if install_dir.join(PORTABLE_MARKER_NAME).is_file() {
        PackageKind::Portable
    } else {
        PackageKind::Installer
    })
}

#[cfg(not(windows))]
fn runtime_package_kind() -> Result<PackageKind, UpdateError> {
    Err(UpdateError::new(
        "unsupported_platform",
        "自动更新目前只支持 Windows x64",
    ))
}

#[cfg(windows)]
fn launch_update_package(path: &Path, release: &ReleaseInfo) -> Result<(), UpdateError> {
    match release.package_kind {
        PackageKind::Installer => launch_installer(path),
        PackageKind::Portable => launch_portable_updater(path, release),
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

#[cfg(windows)]
fn launch_portable_updater(path: &Path, release: &ReleaseInfo) -> Result<(), UpdateError> {
    let executable = std::env::current_exe().map_err(|error| {
        UpdateError::new(
            "executable_path_failed",
            format!("无法确认当前便携版目录：{error}"),
        )
    })?;
    let install_dir = executable.parent().ok_or_else(|| {
        UpdateError::new("executable_path_failed", "当前便携版没有有效的应用目录")
    })?;
    if !install_dir.join(PORTABLE_MARKER_NAME).is_file() {
        return Err(UpdateError::new(
            "portable_marker_missing",
            "当前应用目录缺少便携版标记，无法安全覆盖更新",
        ));
    }

    let source = install_dir.join(PORTABLE_UPDATER_NAME);
    if !source.is_file() {
        return Err(UpdateError::new(
            "portable_updater_missing",
            format!("当前便携版缺少更新助手：{PORTABLE_UPDATER_NAME}"),
        ));
    }
    let update_dir = path.parent().ok_or_else(|| {
        UpdateError::new("update_directory_failed", "下载的更新包没有有效的缓存目录")
    })?;
    let helper = update_dir.join(format!(
        "mediastation-portable-updater-{}.exe",
        release.version
    ));
    if helper.exists() {
        fs::remove_file(&helper).map_err(|error| {
            UpdateError::new(
                "portable_updater_prepare_failed",
                format!("无法替换缓存中的便携版更新助手：{error}"),
            )
        })?;
    }
    fs::copy(&source, &helper).map_err(|error| {
        UpdateError::new(
            "portable_updater_prepare_failed",
            format!("无法准备便携版更新助手：{error}"),
        )
    })?;

    let log_file = update_dir.join("portable-update.log");
    std::process::Command::new(&helper)
        .current_dir(install_dir)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .arg("--archive")
        .arg(path)
        .arg("--install-dir")
        .arg(install_dir)
        .arg("--expected-version")
        .arg(&release.version)
        .arg("--expected-sha256")
        .arg(&release.sha256)
        .arg("--log-file")
        .arg(log_file)
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            UpdateError::new(
                "portable_updater_launch_failed",
                format!("无法启动便携版更新助手：{error}"),
            )
        })
}

#[cfg(not(windows))]
fn launch_update_package(_path: &Path, _release: &ReleaseInfo) -> Result<(), UpdateError> {
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

fn post_download_error(inner: Arc<Inner>, error: UpdateError, release: &ReleaseInfo) {
    jfn_logging::log(
        jfn_logging::CATEGORY_CEF,
        jfn_logging::LEVEL_ERROR,
        &format!(
            "MediaStation update download failed: {}: {}",
            error.code, error.message
        ),
    );
    let downloaded = retained_download_bytes(release);
    post_status(
        inner,
        "error",
        json!({
            "code": error.code,
            "message": error.message,
            "version": release.version,
            "downloadedBytes": downloaded,
            "totalBytes": release.asset_size,
            "percent": downloaded.saturating_mul(100) / release.asset_size,
            "canResume": true,
        }),
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
        let release = parse_release_metadata(&metadata, PackageKind::Installer).unwrap();
        assert_eq!(release.version, "0.1.1");
        assert_eq!(release.asset_size, 123456);
        assert_eq!(release.package_kind, PackageKind::Installer);
        assert!(checksum_asset_url(&metadata).is_ok());
    }

    #[test]
    fn release_metadata_selects_portable_asset_for_portable_runtime() {
        let metadata = json!({
            "draft": false,
            "prerelease": false,
            "tag_name": "v0.1.4",
            "html_url": "https://github.com/timefunnel/MediaStationGo-Windows/releases/tag/v0.1.4",
            "assets": [
                {
                    "name": "MediaStationGo-0.1.4-windows-x64-setup.exe",
                    "browser_download_url": "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/v0.1.4/MediaStationGo-0.1.4-windows-x64-setup.exe",
                    "size": 200000000,
                },
                {
                    "name": "MediaStationGo-0.1.4-windows-x64-portable.zip",
                    "browser_download_url": "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/v0.1.4/MediaStationGo-0.1.4-windows-x64-portable.zip",
                    "size": 210000000,
                },
                {
                    "name": "SHA256SUMS.txt",
                    "browser_download_url": "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/v0.1.4/SHA256SUMS.txt",
                    "size": 300,
                }
            ]
        })
        .to_string();

        let release = parse_release_metadata(&metadata, PackageKind::Portable).unwrap();
        assert_eq!(
            release.asset_name,
            "MediaStationGo-0.1.4-windows-x64-portable.zip"
        );
        assert_eq!(release.asset_size, 210000000);
        assert_eq!(release.package_kind, PackageKind::Portable);
    }

    #[test]
    fn release_metadata_rejects_mirror_assets() {
        let metadata = json!({
            "draft": false,
            "prerelease": false,
            "tag_name": "v0.1.2",
            "html_url": "https://github.com/timefunnel/MediaStationGo-Windows/releases/tag/v0.1.2",
            "assets": [
                {
                    "name": "MediaStationGo-0.1.2-windows-x64-setup.exe",
                    "browser_download_url": "https://cdn.timefunnel.top/mediastation/updates/windows/x64/v0.1.2/MediaStationGo-0.1.2-windows-x64-setup.exe",
                    "size": 218412564,
                },
                {
                    "name": "MediaStationGo-0.1.2-windows-x64-portable.zip",
                    "browser_download_url": "https://cdn.timefunnel.top/mediastation/updates/windows/x64/v0.1.2/MediaStationGo-0.1.2-windows-x64-portable.zip",
                    "size": 329412564,
                },
                {
                    "name": "SHA256SUMS.txt",
                    "browser_download_url": "https://cdn.timefunnel.top/mediastation/updates/windows/x64/v0.1.2/SHA256SUMS.txt",
                    "size": 334,
                }
            ]
        })
        .to_string();

        assert!(parse_release_metadata(&metadata, PackageKind::Installer).is_err());
        assert!(parse_release_metadata(&metadata, PackageKind::Portable).is_err());
        assert!(checksum_asset_url(&metadata).is_err());
    }

    #[test]
    fn asset_validation_accepts_only_exact_github_release_path() {
        let tag = "v0.1.2";
        let name = "MediaStationGo-0.1.2-windows-x64-setup.exe";
        assert!(
            validate_asset_url(
                "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/v0.1.2/MediaStationGo-0.1.2-windows-x64-setup.exe",
                tag,
                name,
            )
            .is_ok()
        );
        assert!(
            validate_asset_url(
                "https://cdn.timefunnel.top/mediastation/updates/windows/x64/v0.1.2/MediaStationGo-0.1.2-windows-x64-setup.exe",
                tag,
                name,
            )
            .is_err()
        );
        assert!(
            validate_asset_url(
                "https://example.com/mediastation/updates/windows/x64/v0.1.2/MediaStationGo-0.1.2-windows-x64-setup.exe",
                tag,
                name,
            )
            .is_err()
        );
        assert!(
            validate_asset_url(
                "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/v0.1.2/other.exe",
                tag,
                name,
            )
            .is_err()
        );
        assert!(
            validate_asset_url(
                "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/v0.1.2/MediaStationGo-0.1.2-windows-x64-setup.exe?source=other",
                tag,
                name,
            )
            .is_err()
        );
    }

    #[test]
    fn download_sources_validate_prefixes_and_keep_direct_fallback() {
        let github_url = "https://github.com/timefunnel/MediaStationGo-Windows/releases/download/v0.1.2/MediaStationGo-0.1.2-windows-x64-setup.exe";
        let prefix = "https://ghfast.top/";
        let source = DownloadSource {
            label: "ghfast.top".to_string(),
            prefix: Some(prefix.to_string()),
        };
        assert_eq!(source.url(github_url), format!("{prefix}{github_url}"));
        assert_eq!(DownloadSource::direct().url(github_url), github_url);
    }

    #[test]
    fn configured_download_sources_reject_unsafe_prefixes() {
        assert!(parse_download_source("http://example.com/").is_none());
        assert!(parse_download_source("https://example.com").is_none());
        assert!(parse_download_source("https://example.com/path").is_none());
        assert!(parse_download_source("https://user@example.com/").is_none());
        assert!(parse_download_source("https://example.com/?token=secret").is_none());
        assert_eq!(
            parse_download_source("https://example.com/").unwrap().label,
            "example.com"
        );
    }

    #[test]
    fn server_download_sources_precede_builtins_and_end_with_direct() {
        let sources = download_sources_from(
            "server",
            "",
            "https://priority.example/\ndirect\nhttps://ghfast.top/",
            true,
        );
        let prefixes = sources
            .iter()
            .map(|source| source.prefix.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(prefixes[0], Some("https://priority.example/"));
        assert_eq!(prefixes[1], Some("https://ghfast.top/"));
        assert_eq!(prefixes.last(), Some(&None));
        assert_eq!(prefixes.iter().filter(|prefix| prefix.is_none()).count(), 1);
        assert_eq!(
            prefixes
                .iter()
                .filter(|prefix| **prefix == Some("https://ghfast.top/"))
                .count(),
            1
        );
    }

    #[test]
    fn expired_server_policy_uses_builtins_before_direct() {
        let sources = download_sources_from("server", "", "https://expired.example/", false);
        assert!(
            !sources
                .iter()
                .any(|source| { source.prefix.as_deref() == Some("https://expired.example/") })
        );
        assert_eq!(
            sources.first().and_then(|source| source.prefix.as_deref()),
            Some("https://gh-proxy.com/")
        );
        assert!(sources.last().is_some_and(|source| source.prefix.is_none()));
    }

    #[test]
    fn resume_response_requires_the_exact_partial_range() {
        assert!(validate_content_range(Some("bytes 1024-2047/4096"), 1024, 4096).is_ok());
        assert!(validate_content_range(Some("bytes 0-2047/4096"), 1024, 4096).is_err());
        assert!(validate_content_range(Some("bytes 1024-4096/4096"), 1024, 4096).is_err());
        assert!(validate_content_range(None, 1024, 4096).is_err());
    }
}
