use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

use crate::api::MediaStationProxyMode;

const MAX_REDIRECTS: usize = 6;
const DEFAULT_SESSION_TTL_MS: u64 = 120_000;
const EXPIRY_SAFETY_MS: u64 = 60_000;
const CACHE_REUSE_SAFETY_MS: u64 = 10_000;

const HEADER_ACCEPT_RANGES: &str = "accept-ranges";
const HEADER_CONTENT_LENGTH: &str = "content-length";
const HEADER_CONTENT_RANGE: &str = "content-range";
const HEADER_EMBY_AUTHORIZATION: &str = "x-emby-authorization";
const HEADER_EMBY_TOKEN: &str = "x-emby-token";
const HEADER_ETAG: &str = "etag";
const HEADER_LAST_MODIFIED: &str = "last-modified";
const HEADER_LOCATION: &str = "location";
const HEADER_RANGE: &str = "range";
const HEADER_USER_AGENT: &str = "user-agent";

#[derive(Clone, Default, PartialEq, Eq)]
pub struct HeaderMap(BTreeMap<String, String>);

impl HeaderMap {
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0
            .insert(name.into().to_ascii_lowercase(), value.into());
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(&name.to_ascii_lowercase()).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    pub fn to_mpv_http_header_fields(&self) -> Result<String, HeaderEncodingError> {
        let mut fields = Vec::with_capacity(self.0.len());
        for (name, value) in &self.0 {
            if !is_valid_header_name(name) {
                return Err(HeaderEncodingError::InvalidName { name: name.clone() });
            }
            if value.contains(['\r', '\n', '\0']) {
                return Err(HeaderEncodingError::InvalidValue { name: name.clone() });
            }
            fields.push(format!("{name}: {}", value.replace(',', "\\,")));
        }
        Ok(fields.join(","))
    }
}

impl fmt::Debug for HeaderMap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = formatter.debug_map();
        for (name, value) in &self.0 {
            if is_sensitive_header(name) {
                map.entry(name, &"<redacted>");
            } else {
                map.entry(name, value);
            }
        }
        map.finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderEncodingError {
    InvalidName { name: String },
    InvalidValue { name: String },
}

impl fmt::Display for HeaderEncodingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name } => write!(formatter, "invalid HTTP header name: {name:?}"),
            Self::InvalidValue { name } => {
                write!(formatter, "HTTP header {name:?} contains a prohibited byte")
            }
        }
    }
}

impl std::error::Error for HeaderEncodingError {}

#[derive(Clone, PartialEq, Eq)]
pub struct ServerAuth {
    token: String,
    authorization: String,
}

impl ServerAuth {
    pub fn new(
        token: impl Into<String>,
        authorization: impl Into<String>,
    ) -> Result<Self, PlaybackSessionError> {
        let token = token.into();
        let authorization = authorization.into();
        if token.trim().is_empty() {
            return Err(PlaybackSessionError::InvalidInput {
                field: "token",
                reason: "must not be empty".to_string(),
            });
        }
        if authorization.trim().is_empty() {
            return Err(PlaybackSessionError::InvalidInput {
                field: "authorization",
                reason: "must not be empty".to_string(),
            });
        }
        Ok(Self {
            token,
            authorization,
        })
    }

    fn apply(&self, headers: &mut HeaderMap) {
        headers.insert(HEADER_EMBY_TOKEN, self.token.clone());
        headers.insert(HEADER_EMBY_AUTHORIZATION, self.authorization.clone());
    }
}

impl fmt::Debug for ServerAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerAuth")
            .field("token", &"<redacted>")
            .field("authorization", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub struct ResolveInput {
    pub user_id: String,
    pub media_id: String,
    pub source_url: Url,
    pub server_base_url: Url,
    pub user_agent: String,
    pub auth: ServerAuth,
    pub proxy_mode: MediaStationProxyMode,
}

impl ResolveInput {
    pub fn validate(&self) -> Result<(), PlaybackSessionError> {
        validate_non_empty("user_id", &self.user_id)?;
        validate_non_empty("media_id", &self.media_id)?;
        validate_non_empty("user_agent", &self.user_agent)?;
        validate_http_url("source_url", &self.source_url)?;
        validate_http_url("server_base_url", &self.server_base_url)?;
        if !same_origin(&self.source_url, &self.server_base_url) {
            return Err(PlaybackSessionError::SourceOriginMismatch {
                source_origin: origin_label(&self.source_url),
                server_origin: origin_label(&self.server_base_url),
            });
        }
        Ok(())
    }
}

impl fmt::Debug for ResolveInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolveInput")
            .field("user_id", &self.user_id)
            .field("media_id", &self.media_id)
            .field("source_url", &redacted_url(&self.source_url))
            .field("server_base_url", &redacted_url(&self.server_base_url))
            .field("user_agent", &self.user_agent)
            .field("auth", &self.auth)
            .field("proxy_mode", &self.proxy_mode)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryMode {
    Server,
    DirectCdn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionExpirySource {
    QueryTimestamp,
    DefaultTtl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackSessionMetrics {
    pub redirect_count: usize,
    pub resolve_ms: u64,
    pub status_code: u16,
    pub target_host: String,
    pub delivery_mode: DeliveryMode,
    pub accepts_ranges: bool,
    pub expiry_source: SessionExpirySource,
    pub reused: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PlaybackSession {
    pub user_id: String,
    pub media_id: String,
    pub source_url: Url,
    pub resolved_url: Url,
    pub request_headers: HeaderMap,
    pub content_length: Option<u64>,
    pub content_version: Option<String>,
    pub expires_at_epoch_ms: u64,
    pub metrics: PlaybackSessionMetrics,
}

impl PlaybackSession {
    pub fn is_reusable_at(&self, now_epoch_ms: u64) -> bool {
        self.expires_at_epoch_ms
            .saturating_sub(CACHE_REUSE_SAFETY_MS)
            > now_epoch_ms
    }

    fn as_reused(&self) -> Self {
        let mut reused = self.clone();
        reused.metrics.resolve_ms = 0;
        reused.metrics.reused = true;
        reused
    }
}

impl fmt::Debug for PlaybackSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaybackSession")
            .field("user_id", &self.user_id)
            .field("media_id", &self.media_id)
            .field("source_url", &redacted_url(&self.source_url))
            .field("resolved_url", &redacted_url(&self.resolved_url))
            .field("request_headers", &self.request_headers)
            .field("content_length", &self.content_length)
            .field("content_version", &self.content_version)
            .field("expires_at_epoch_ms", &self.expires_at_epoch_ms)
            .field("metrics", &self.metrics)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    pub url: Url,
    pub headers: HeaderMap,
    pub proxy_mode: MediaStationProxyMode,
}

impl fmt::Debug for ProbeRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProbeRequest")
            .field("url", &redacted_url(&self.url))
            .field("headers", &self.headers)
            .field("proxy_mode", &self.proxy_mode)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeResponse {
    pub status_code: u16,
    pub headers: HeaderMap,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportError {
    pub message: String,
}

impl TransportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

pub trait ProbeTransport: Send + Sync {
    fn probe(&self, request: &ProbeRequest) -> Result<ProbeResponse, TransportError>;
}

#[derive(Clone)]
pub struct UreqTransport {
    direct_agent: ureq::Agent,
    system_proxy_agent: Arc<Mutex<Option<CachedSystemProxyAgent>>>,
}

struct CachedSystemProxyAgent {
    proxy_url: String,
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new() -> Self {
        Self {
            direct_agent: build_probe_agent(None),
            system_proxy_agent: Arc::new(Mutex::new(None)),
        }
    }

    fn agent_for_mode(
        &self,
        proxy_mode: MediaStationProxyMode,
    ) -> Result<ureq::Agent, TransportError> {
        match proxy_mode {
            MediaStationProxyMode::Direct => Ok(self.direct_agent.clone()),
            MediaStationProxyMode::System => {
                let proxy = ureq::Proxy::try_from_env()
                    .ok_or_else(|| TransportError::new("system HTTP proxy is unavailable"))?;
                let proxy_url = proxy.uri().to_string();
                let mut cached = self.system_proxy_agent.lock();
                if let Some(cached) = cached.as_ref()
                    && cached.proxy_url == proxy_url
                {
                    return Ok(cached.agent.clone());
                }
                let agent = build_probe_agent(Some(proxy));
                *cached = Some(CachedSystemProxyAgent {
                    proxy_url,
                    agent: agent.clone(),
                });
                Ok(agent)
            }
        }
    }
}

fn build_probe_agent(proxy: Option<ureq::Proxy>) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(25)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(15)))
        .proxy(proxy)
        .build();
    ureq::Agent::new_with_config(config)
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ProbeTransport for UreqTransport {
    fn probe(&self, request: &ProbeRequest) -> Result<ProbeResponse, TransportError> {
        let agent = self.agent_for_mode(request.proxy_mode)?;
        let mut builder = agent.get(request.url.as_str());
        for (name, value) in request.headers.iter() {
            builder = builder.header(name, value);
        }
        let mut response = builder
            .call()
            .map_err(|error| TransportError::new(error.to_string()))?;
        let status_code = response.status().as_u16();
        let mut headers = HeaderMap::default();
        for name in [
            HEADER_ACCEPT_RANGES,
            HEADER_CONTENT_LENGTH,
            HEADER_CONTENT_RANGE,
            HEADER_ETAG,
            HEADER_LAST_MODIFIED,
            HEADER_LOCATION,
        ] {
            if let Some(value) = response.headers().get(name) {
                let value = value.to_str().map_err(|error| {
                    TransportError::new(format!("response header {name} is invalid: {error}"))
                })?;
                headers.insert(name, value);
            }
        }

        if !is_redirect(status_code) {
            let mut probe_byte = [0_u8; 1];
            response
                .body_mut()
                .as_reader()
                .read(&mut probe_byte)
                .map_err(|error| TransportError::new(format!("probe body read failed: {error}")))?;
        }
        Ok(ProbeResponse {
            status_code,
            headers,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackSessionError {
    InvalidInput {
        field: &'static str,
        reason: String,
    },
    UnsupportedScheme {
        field: &'static str,
        scheme: String,
    },
    EmbeddedCredentials {
        field: &'static str,
    },
    SourceOriginMismatch {
        source_origin: String,
        server_origin: String,
    },
    Transport {
        url: String,
        message: String,
    },
    RedirectMissingLocation {
        url: String,
        status_code: u16,
    },
    RedirectLocationInvalid {
        url: String,
        location: String,
        message: String,
    },
    RedirectLimitExceeded {
        max_redirects: usize,
    },
    RedirectSecurityDowngrade {
        from: String,
        to: String,
    },
    UnexpectedStatus {
        url: String,
        status_code: u16,
    },
    RangeUnsupported {
        url: String,
        status_code: u16,
        accept_ranges: Option<String>,
    },
    InvalidResponseHeader {
        name: &'static str,
        value: String,
    },
}

impl fmt::Display for PlaybackSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field, reason } => write!(formatter, "invalid {field}: {reason}"),
            Self::UnsupportedScheme { field, scheme } => {
                write!(formatter, "unsupported {field} scheme: {scheme}")
            }
            Self::EmbeddedCredentials { field } => {
                write!(formatter, "embedded credentials are not allowed in {field}")
            }
            Self::SourceOriginMismatch {
                source_origin,
                server_origin,
            } => write!(
                formatter,
                "playback source origin {source_origin} does not match server origin {server_origin}"
            ),
            Self::Transport { url, message } => {
                write!(formatter, "playback probe failed for {url}: {message}")
            }
            Self::RedirectMissingLocation { url, status_code } => write!(
                formatter,
                "redirect response {status_code} from {url} has no Location header"
            ),
            Self::RedirectLocationInvalid {
                url,
                location,
                message,
            } => write!(
                formatter,
                "invalid redirect Location {location:?} from {url}: {message}"
            ),
            Self::RedirectLimitExceeded { max_redirects } => {
                write!(
                    formatter,
                    "playback redirect limit exceeded: {max_redirects}"
                )
            }
            Self::RedirectSecurityDowngrade { from, to } => {
                write!(
                    formatter,
                    "HTTPS redirect downgrade rejected: {from} -> {to}"
                )
            }
            Self::UnexpectedStatus { url, status_code } => {
                write!(
                    formatter,
                    "unexpected playback probe status {status_code} from {url}"
                )
            }
            Self::RangeUnsupported {
                url,
                status_code,
                accept_ranges,
            } => write!(
                formatter,
                "direct CDN does not support byte ranges: url={url} status={status_code} Accept-Ranges={accept_ranges:?}"
            ),
            Self::InvalidResponseHeader { name, value } => {
                write!(formatter, "invalid response header {name}: {value:?}")
            }
        }
    }
}

impl std::error::Error for PlaybackSessionError {}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SessionKey {
    user_id: String,
    media_id: String,
}

#[derive(Clone)]
struct CachedSession {
    session: PlaybackSession,
    auth: ServerAuth,
    server_base_url: Url,
    proxy_mode: MediaStationProxyMode,
}

pub struct PlaybackSessionResolver<T: ProbeTransport> {
    transport: T,
    sessions: Mutex<HashMap<SessionKey, CachedSession>>,
    resolve_locks: Mutex<HashMap<SessionKey, Arc<Mutex<()>>>>,
}

impl<T: ProbeTransport> PlaybackSessionResolver<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            sessions: Mutex::new(HashMap::new()),
            resolve_locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn resolve(&self, input: &ResolveInput) -> Result<PlaybackSession, PlaybackSessionError> {
        self.resolve_at(input, unix_epoch_ms())
    }

    pub fn resolve_at(
        &self,
        input: &ResolveInput,
        now_epoch_ms: u64,
    ) -> Result<PlaybackSession, PlaybackSessionError> {
        input.validate()?;
        let key = SessionKey {
            user_id: input.user_id.clone(),
            media_id: input.media_id.clone(),
        };
        let resolve_lock = {
            let mut locks = self.resolve_locks.lock();
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = resolve_lock.lock();

        if let Some(cached) = self.sessions.lock().get(&key)
            && cached.session.source_url == input.source_url
            && cached.server_base_url == input.server_base_url
            && cached.auth == input.auth
            && cached.proxy_mode == input.proxy_mode
            && cached.session.is_reusable_at(now_epoch_ms)
        {
            return Ok(cached.session.as_reused());
        }

        let session = self.resolve_network(input, now_epoch_ms)?;
        self.sessions.lock().insert(
            key,
            CachedSession {
                session: session.clone(),
                auth: input.auth.clone(),
                server_base_url: input.server_base_url.clone(),
                proxy_mode: input.proxy_mode,
            },
        );
        Ok(session)
    }

    pub fn invalidate(&self, user_id: &str, media_id: &str) {
        let key = SessionKey {
            user_id: user_id.to_string(),
            media_id: media_id.to_string(),
        };
        self.sessions.lock().remove(&key);
        self.resolve_locks.lock().remove(&key);
    }

    fn resolve_network(
        &self,
        input: &ResolveInput,
        now_epoch_ms: u64,
    ) -> Result<PlaybackSession, PlaybackSessionError> {
        let started = Instant::now();
        let mut current = input.source_url.clone();
        let mut redirect_count = 0;

        loop {
            let mut headers = HeaderMap::default();
            headers.insert(HEADER_USER_AGENT, input.user_agent.clone());
            headers.insert(HEADER_RANGE, "bytes=0-0");
            if same_origin(&current, &input.server_base_url) {
                input.auth.apply(&mut headers);
            }
            let response = self
                .transport
                .probe(&ProbeRequest {
                    url: current.clone(),
                    headers,
                    proxy_mode: input.proxy_mode,
                })
                .map_err(|error| PlaybackSessionError::Transport {
                    url: redacted_url(&current),
                    message: error.to_string(),
                })?;

            if is_redirect(response.status_code) {
                if redirect_count == MAX_REDIRECTS {
                    return Err(PlaybackSessionError::RedirectLimitExceeded {
                        max_redirects: MAX_REDIRECTS,
                    });
                }
                let location = response.headers.get(HEADER_LOCATION).ok_or_else(|| {
                    PlaybackSessionError::RedirectMissingLocation {
                        url: redacted_url(&current),
                        status_code: response.status_code,
                    }
                })?;
                let next = current.join(location).map_err(|error| {
                    PlaybackSessionError::RedirectLocationInvalid {
                        url: redacted_url(&current),
                        location: location.to_string(),
                        message: error.to_string(),
                    }
                })?;
                validate_http_url("redirect_url", &next)?;
                if current.scheme() == "https" && next.scheme() == "http" {
                    return Err(PlaybackSessionError::RedirectSecurityDowngrade {
                        from: redacted_url(&current),
                        to: redacted_url(&next),
                    });
                }
                current = next;
                redirect_count += 1;
                continue;
            }

            if !(200..300).contains(&response.status_code) {
                return Err(PlaybackSessionError::UnexpectedStatus {
                    url: redacted_url(&current),
                    status_code: response.status_code,
                });
            }

            let delivery_mode = if same_origin(&current, &input.server_base_url) {
                DeliveryMode::Server
            } else {
                DeliveryMode::DirectCdn
            };
            let accepts_ranges = response.status_code == 206
                || response
                    .headers
                    .get(HEADER_ACCEPT_RANGES)
                    .is_some_and(|value| value.eq_ignore_ascii_case("bytes"));
            if delivery_mode == DeliveryMode::DirectCdn && !accepts_ranges {
                return Err(PlaybackSessionError::RangeUnsupported {
                    url: redacted_url(&current),
                    status_code: response.status_code,
                    accept_ranges: response
                        .headers
                        .get(HEADER_ACCEPT_RANGES)
                        .map(str::to_string),
                });
            }

            let mut request_headers = HeaderMap::default();
            request_headers.insert(HEADER_USER_AGENT, input.user_agent.clone());
            if delivery_mode == DeliveryMode::Server {
                input.auth.apply(&mut request_headers);
            }
            let (expires_at_epoch_ms, expiry_source) = direct_link_expiry(&current, now_epoch_ms);
            let content_length = parse_content_length(&response.headers)?;
            let content_version = response
                .headers
                .get(HEADER_ETAG)
                .or_else(|| response.headers.get(HEADER_LAST_MODIFIED))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let resolve_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            return Ok(PlaybackSession {
                user_id: input.user_id.clone(),
                media_id: input.media_id.clone(),
                source_url: input.source_url.clone(),
                resolved_url: current.clone(),
                request_headers,
                content_length,
                content_version,
                expires_at_epoch_ms,
                metrics: PlaybackSessionMetrics {
                    redirect_count,
                    resolve_ms,
                    status_code: response.status_code,
                    target_host: current.host_str().unwrap_or_default().to_string(),
                    delivery_mode,
                    accepts_ranges,
                    expiry_source,
                    reused: false,
                },
            });
        }
    }
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), PlaybackSessionError> {
    if value.trim().is_empty() {
        return Err(PlaybackSessionError::InvalidInput {
            field,
            reason: "must not be empty".to_string(),
        });
    }
    Ok(())
}

fn validate_http_url(field: &'static str, url: &Url) -> Result<(), PlaybackSessionError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PlaybackSessionError::UnsupportedScheme {
            field,
            scheme: url.scheme().to_string(),
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PlaybackSessionError::EmbeddedCredentials { field });
    }
    if url.host_str().is_none() {
        return Err(PlaybackSessionError::InvalidInput {
            field,
            reason: "host is required".to_string(),
        });
    }
    Ok(())
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn origin_label(url: &Url) -> String {
    let host = url.host_str().unwrap_or("<missing-host>");
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

fn redacted_url(url: &Url) -> String {
    let mut redacted = url.clone();
    if redacted.query().is_some() {
        redacted.set_query(Some("<redacted>"));
    }
    redacted.set_fragment(None);
    redacted.to_string()
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | HEADER_EMBY_AUTHORIZATION | HEADER_EMBY_TOKEN
    )
}

fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn is_redirect(status_code: u16) -> bool {
    matches!(status_code, 301 | 302 | 303 | 307 | 308)
}

fn parse_content_length(headers: &HeaderMap) -> Result<Option<u64>, PlaybackSessionError> {
    if let Some(content_range) = headers.get(HEADER_CONTENT_RANGE) {
        let total = content_range
            .rsplit_once('/')
            .map(|(_, total)| total.trim())
            .ok_or_else(|| PlaybackSessionError::InvalidResponseHeader {
                name: HEADER_CONTENT_RANGE,
                value: content_range.to_string(),
            })?;
        if total == "*" {
            return Ok(None);
        }
        return total.parse::<u64>().map(Some).map_err(|_| {
            PlaybackSessionError::InvalidResponseHeader {
                name: HEADER_CONTENT_RANGE,
                value: content_range.to_string(),
            }
        });
    }
    headers
        .get(HEADER_CONTENT_LENGTH)
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| PlaybackSessionError::InvalidResponseHeader {
                    name: HEADER_CONTENT_LENGTH,
                    value: value.to_string(),
                })
        })
        .transpose()
}

fn direct_link_expiry(url: &Url, now_epoch_ms: u64) -> (u64, SessionExpirySource) {
    if let Some(raw) = url
        .query_pairs()
        .find_map(|(name, value)| (name == "t").then(|| value.into_owned()))
        && let Ok(parsed) = raw.parse::<u64>()
    {
        let epoch_ms = if raw.len() >= 13 {
            parsed
        } else {
            parsed.saturating_mul(1_000)
        };
        if epoch_ms > now_epoch_ms.saturating_add(EXPIRY_SAFETY_MS) {
            return (
                epoch_ms.saturating_sub(EXPIRY_SAFETY_MS),
                SessionExpirySource::QueryTimestamp,
            );
        }
    }
    (
        now_epoch_ms.saturating_add(DEFAULT_SESSION_TTL_MS),
        SessionExpirySource::DefaultTtl,
    )
}

fn unix_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::thread;

    const NOW_MS: u64 = 1_800_000_000_000;

    #[derive(Clone, Default)]
    struct FakeTransport {
        state: Arc<Mutex<FakeTransportState>>,
    }

    #[derive(Default)]
    struct FakeTransportState {
        responses: VecDeque<Result<ProbeResponse, TransportError>>,
        requests: Vec<ProbeRequest>,
    }

    impl FakeTransport {
        fn with_responses(responses: Vec<ProbeResponse>) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeTransportState {
                    responses: responses.into_iter().map(Ok).collect(),
                    requests: Vec::new(),
                })),
            }
        }

        fn requests(&self) -> Vec<ProbeRequest> {
            self.state.lock().requests.clone()
        }
    }

    impl ProbeTransport for FakeTransport {
        fn probe(&self, request: &ProbeRequest) -> Result<ProbeResponse, TransportError> {
            let mut state = self.state.lock();
            state.requests.push(request.clone());
            state
                .responses
                .pop_front()
                .unwrap_or_else(|| Err(TransportError::new("no fake response configured")))
        }
    }

    fn headers(values: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::default();
        for (name, value) in values {
            headers.insert(*name, *value);
        }
        headers
    }

    fn response(status_code: u16, values: &[(&str, &str)]) -> ProbeResponse {
        ProbeResponse {
            status_code,
            headers: headers(values),
        }
    }

    fn input(source_url: &str) -> ResolveInput {
        ResolveInput {
            user_id: "user-1".to_string(),
            media_id: "media-1".to_string(),
            source_url: Url::parse(source_url).expect("source URL should parse"),
            server_base_url: Url::parse("https://media.example").expect("base URL should parse"),
            user_agent: "MediaStationWindows/0.1".to_string(),
            auth: ServerAuth::new("secret-token", "MediaBrowser Token=secret-token")
                .expect("auth should be valid"),
            proxy_mode: MediaStationProxyMode::Direct,
        }
    }

    fn accept_request(listener: TcpListener, response: String) -> String {
        let (mut stream, _) = listener.accept().expect("test server should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout should apply");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream
                .read(&mut buffer)
                .expect("request should be readable");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        stream
            .write_all(response.as_bytes())
            .expect("response should be writable");
        String::from_utf8(request).expect("HTTP request should be UTF-8")
    }

    #[test]
    fn direct_cdn_redirect_strips_private_headers() {
        let transport = FakeTransport::with_responses(vec![
            response(302, &[(HEADER_LOCATION, "https://cdn.example/video.mkv")]),
            response(
                206,
                &[
                    (HEADER_CONTENT_RANGE, "bytes 0-0/12345"),
                    (HEADER_ACCEPT_RANGES, "bytes"),
                    (HEADER_ETAG, "v1"),
                ],
            ),
        ]);
        let resolver = PlaybackSessionResolver::new(transport.clone());

        let session = resolver
            .resolve_at(&input("https://media.example/Videos/1/stream"), NOW_MS)
            .expect("session should resolve");

        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].headers.get(HEADER_EMBY_TOKEN),
            Some("secret-token")
        );
        assert_eq!(requests[0].headers.get(HEADER_RANGE), Some("bytes=0-0"));
        assert_eq!(requests[1].headers.get(HEADER_EMBY_TOKEN), None);
        assert_eq!(requests[1].headers.get(HEADER_EMBY_AUTHORIZATION), None);
        assert_eq!(session.request_headers.get(HEADER_EMBY_TOKEN), None);
        assert_eq!(session.content_length, Some(12_345));
        assert_eq!(session.content_version.as_deref(), Some("v1"));
        assert_eq!(session.metrics.delivery_mode, DeliveryMode::DirectCdn);
        assert_eq!(session.metrics.redirect_count, 1);
    }

    #[test]
    fn ureq_transport_keeps_credentials_off_the_cdn_request() {
        let server = TcpListener::bind("127.0.0.1:0").expect("server listener should bind");
        let cdn = TcpListener::bind("127.0.0.1:0").expect("CDN listener should bind");
        let server_address = server.local_addr().expect("server address should resolve");
        let cdn_address = cdn.local_addr().expect("CDN address should resolve");
        let cdn_response = concat!(
            "HTTP/1.1 206 Partial Content\r\n",
            "Content-Range: bytes 0-0/42\r\n",
            "Accept-Ranges: bytes\r\n",
            "Content-Length: 1\r\n",
            "Connection: close\r\n",
            "\r\n",
            "x"
        )
        .to_string();
        let cdn_thread = thread::spawn(move || accept_request(cdn, cdn_response));
        let server_response = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{cdn_address}/video.mkv\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let server_thread = thread::spawn(move || accept_request(server, server_response));
        let source_url = format!("http://{server_address}/stream");
        let mut request = input(&source_url);
        request.server_base_url =
            Url::parse(&format!("http://{server_address}")).expect("base URL should parse");
        let resolver = PlaybackSessionResolver::new(UreqTransport::new());

        let session = resolver
            .resolve_at(&request, NOW_MS)
            .expect("real HTTP redirect should resolve");
        let server_request = server_thread.join().expect("server thread should finish");
        let cdn_request = cdn_thread.join().expect("CDN thread should finish");
        let server_request = server_request.to_ascii_lowercase();
        let cdn_request = cdn_request.to_ascii_lowercase();

        assert!(server_request.contains("x-emby-token: secret-token"));
        assert!(server_request.contains("range: bytes=0-0"));
        assert!(!cdn_request.contains("x-emby-token"));
        assert!(!cdn_request.contains("x-emby-authorization"));
        assert!(cdn_request.contains("range: bytes=0-0"));
        assert_eq!(session.metrics.delivery_mode, DeliveryMode::DirectCdn);
        assert_eq!(session.content_length, Some(42));
    }

    #[test]
    fn relative_redirect_keeps_auth_on_same_origin() {
        let transport = FakeTransport::with_responses(vec![
            response(302, &[(HEADER_LOCATION, "/resolved/video.mkv")]),
            response(206, &[(HEADER_CONTENT_RANGE, "bytes 0-0/42")]),
        ]);
        let resolver = PlaybackSessionResolver::new(transport.clone());

        let session = resolver
            .resolve_at(&input("https://media.example/Videos/1/stream"), NOW_MS)
            .expect("session should resolve");

        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].headers.get(HEADER_EMBY_TOKEN),
            Some("secret-token")
        );
        assert_eq!(
            session.request_headers.get(HEADER_EMBY_TOKEN),
            Some("secret-token")
        );
        assert_eq!(session.metrics.delivery_mode, DeliveryMode::Server);
    }

    #[test]
    fn direct_cdn_without_range_support_is_an_error() {
        let transport = FakeTransport::with_responses(vec![
            response(302, &[(HEADER_LOCATION, "https://cdn.example/video.mkv")]),
            response(200, &[(HEADER_CONTENT_LENGTH, "12345")]),
        ]);
        let resolver = PlaybackSessionResolver::new(transport);

        let error = resolver
            .resolve_at(&input("https://media.example/Videos/1/stream"), NOW_MS)
            .expect_err("range rejection must be explicit");

        assert!(matches!(
            error,
            PlaybackSessionError::RangeUnsupported { .. }
        ));
    }

    #[test]
    fn cross_origin_source_is_rejected_before_network() {
        let transport = FakeTransport::default();
        let resolver = PlaybackSessionResolver::new(transport.clone());

        let error = resolver
            .resolve_at(&input("https://cdn.example/video.mkv"), NOW_MS)
            .expect_err("cross-origin source must be rejected");

        assert!(matches!(
            error,
            PlaybackSessionError::SourceOriginMismatch { .. }
        ));
        assert!(transport.requests().is_empty());
    }

    #[test]
    fn missing_redirect_location_is_an_error() {
        let transport = FakeTransport::with_responses(vec![response(302, &[])]);
        let resolver = PlaybackSessionResolver::new(transport);

        let error = resolver
            .resolve_at(&input("https://media.example/Videos/1/stream"), NOW_MS)
            .expect_err("missing Location must fail");

        assert!(matches!(
            error,
            PlaybackSessionError::RedirectMissingLocation { .. }
        ));
    }

    #[test]
    fn https_downgrade_is_rejected() {
        let transport = FakeTransport::with_responses(vec![response(
            302,
            &[(HEADER_LOCATION, "http://cdn.example/video.mkv")],
        )]);
        let resolver = PlaybackSessionResolver::new(transport);

        let error = resolver
            .resolve_at(&input("https://media.example/Videos/1/stream"), NOW_MS)
            .expect_err("HTTPS downgrade must fail");

        assert!(matches!(
            error,
            PlaybackSessionError::RedirectSecurityDowngrade { .. }
        ));
    }

    #[test]
    fn cached_session_is_user_scoped_and_reused() {
        let transport = FakeTransport::with_responses(vec![response(
            206,
            &[(HEADER_CONTENT_RANGE, "bytes 0-0/123")],
        )]);
        let resolver = PlaybackSessionResolver::new(transport.clone());
        let request = input("https://media.example/Videos/1/stream");

        let first = resolver
            .resolve_at(&request, NOW_MS)
            .expect("first resolve should work");
        let second = resolver
            .resolve_at(&request, NOW_MS + 1_000)
            .expect("cached resolve should work");

        assert!(!first.metrics.reused);
        assert!(second.metrics.reused);
        assert_eq!(second.metrics.resolve_ms, 0);
        assert_eq!(transport.requests().len(), 1);
    }

    #[test]
    fn signed_link_expiry_uses_safety_margin() {
        let expiry_seconds = (NOW_MS + 300_000) / 1_000;
        let location = format!("https://cdn.example/video.mkv?t={expiry_seconds}");
        let transport = FakeTransport::with_responses(vec![
            response(302, &[(HEADER_LOCATION, &location)]),
            response(206, &[(HEADER_CONTENT_RANGE, "bytes 0-0/123")]),
        ]);
        let resolver = PlaybackSessionResolver::new(transport);

        let session = resolver
            .resolve_at(&input("https://media.example/Videos/1/stream"), NOW_MS)
            .expect("session should resolve");

        assert_eq!(session.expires_at_epoch_ms, NOW_MS + 240_000);
        assert_eq!(
            session.metrics.expiry_source,
            SessionExpirySource::QueryTimestamp
        );
    }

    #[test]
    fn malformed_content_range_is_an_error() {
        let transport = FakeTransport::with_responses(vec![response(
            206,
            &[(HEADER_CONTENT_RANGE, "not-a-range")],
        )]);
        let resolver = PlaybackSessionResolver::new(transport);

        let error = resolver
            .resolve_at(&input("https://media.example/Videos/1/stream"), NOW_MS)
            .expect_err("invalid Content-Range must fail");

        assert!(matches!(
            error,
            PlaybackSessionError::InvalidResponseHeader {
                name: HEADER_CONTENT_RANGE,
                ..
            }
        ));
    }

    #[test]
    fn debug_output_redacts_tokens_and_signed_queries() {
        let request = input("https://media.example/Videos/1/stream?api_key=secret");
        let debug = format!("{request:?}");

        assert!(!debug.contains("secret-token"));
        assert!(!debug.contains("api_key=secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn mpv_header_fields_escape_commas_in_values() {
        let fields = headers(&[
            (HEADER_USER_AGENT, "MediaStationWindows/0.1"),
            (
                HEADER_EMBY_AUTHORIZATION,
                "MediaBrowser Client=\"MediaStation Windows\", Token=\"secret\"",
            ),
        ])
        .to_mpv_http_header_fields()
        .expect("headers should encode");

        assert_eq!(
            fields,
            "user-agent: MediaStationWindows/0.1,x-emby-authorization: MediaBrowser Client=\"MediaStation Windows\"\\, Token=\"secret\""
        );
    }

    #[test]
    fn mpv_header_fields_reject_header_injection() {
        let mut values = HeaderMap::default();
        values.insert(HEADER_USER_AGENT, "valid\r\nx-emby-token: leaked");

        let error = values
            .to_mpv_http_header_fields()
            .expect_err("newlines must be rejected");

        assert_eq!(
            error,
            HeaderEncodingError::InvalidValue {
                name: HEADER_USER_AGENT.to_string()
            }
        );
    }
}
