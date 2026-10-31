//! Custom mpv stream protocol backed by ureq.
//!
//! The `mediastation://` protocol preserves the resolver's UA and proxy mode
//! while providing bounded Range recovery and explicit transport failures.
//! A CDN rejection alone does not establish its upstream cause.

use parking_lot::Mutex;
use std::ffi::{CStr, CString};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ureq::ResponseExt;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};

use crate::sys;

const MPV_ERROR_UNSUPPORTED: i64 = -18;
const MPV_ERROR_GENERIC: i64 = -20;
const MPV_ERROR_LOADING_FAILED: c_int = -13;

const PROTOCOL: &[u8] = b"mediastation";
const RANGE_OPEN_ATTEMPTS: usize = 3;
const READ_RECOVERY_ATTEMPTS: usize = 2;
const RETRY_BASE_DELAY_MS: u64 = 150;
const ERROR_BODY_LIMIT: u64 = 4096;
const ERROR_BODY_TIMEOUT: Duration = Duration::from_millis(100);
const IO_CANCEL_POLL: Duration = Duration::from_millis(100);
const MEDIA_IDLE_TIMEOUT: Duration = Duration::from_secs(15);
const INITIAL_HEAD_LIMIT: usize = 64 * 1024;
static NEXT_STREAM_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
static FAILURE_HANDLER: Mutex<Option<fn(StreamFailure)>> = Mutex::new(None);
// One queued load, consumed on open. Replaced on every native load so canceled
// loads cannot accumulate media bytes. The complete per-load URI binds the
// bytes to the URL, UA, proxy mode, size, and unique session.
static PENDING_INITIAL_HEAD: Mutex<Option<InitialHead>> = Mutex::new(None);
static PENDING_HTTP_STREAM: Mutex<Option<PreparedHttpStream>> = Mutex::new(None);

/// One load's live HTTP response. Credentials and the body stay in memory;
/// the native URI contains only the unique load identifier.
pub struct PreparedHttpStream {
    stream: Stream,
}

impl PreparedHttpStream {
    pub fn uri(&self) -> String {
        format!("mediastation-http://{}", self.stream.session_id)
    }
}

/// The caller's existing resolver owns redirect and credential policy. Each
/// request here disables automatic redirects and replaces all request headers.
pub struct HttpStreamPreparation(Mutex<Stream>);

pub struct HttpStreamResponse {
    pub status_code: u16,
    pub headers: Vec<(String, String)>,
}

impl HttpStreamPreparation {
    pub fn new(url: &str, user_agent: &str, proxy: StreamProxyMode) -> Result<Self, StreamError> {
        let mut stream = Stream::new(
            url,
            user_agent,
            None,
            proxy,
            NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
        )
        .ok_or_else(|| StreamError::Transport("proxy_unavailable".to_string()))?;
        stream.http_headers = Some(Vec::new());
        Ok(Self(Mutex::new(stream)))
    }

    pub fn open<'a>(
        &self,
        url: &str,
        headers: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Result<HttpStreamResponse, StreamError> {
        let mut stream = self.0.lock();
        stream.url = url.to_string();
        stream.link_id = diagnostic_link_id(url);
        stream.http_headers = Some(
            headers
                .filter(|(name, _)| {
                    !name.eq_ignore_ascii_case("range") && !name.eq_ignore_ascii_case("user-agent")
                })
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
        );
        let (response, started) = request_range(&mut stream, 0, "prepare", 1)?;
        let status_code = response.status().as_u16();
        let headers = [
            "location",
            "content-length",
            "content-range",
            "accept-ranges",
            "etag",
            "last-modified",
        ]
        .into_iter()
        .filter_map(|name| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(|value| (name.to_string(), value.to_string()))
        })
        .collect();
        if matches!(status_code, 200 | 206) {
            install_reader(&mut stream, response, started, 0, "prepare", 1)?;
            stream.seekable = status_code == 206;
        } else if status_code >= 400 {
            log_http_failure(
                &stream,
                0,
                "prepare",
                1,
                started.elapsed().as_millis(),
                response,
            );
        }
        Ok(HttpStreamResponse {
            status_code,
            headers,
        })
    }

    pub fn finish(self) -> Result<PreparedHttpStream, StreamError> {
        let stream = self.0.into_inner();
        if stream.reader.is_none() {
            return Err(StreamError::InvalidResponse(
                "media_response_missing".to_string(),
            ));
        }
        Ok(PreparedHttpStream { stream })
    }
}

struct InitialHead {
    uri: String,
    bytes: Arc<[u8]>,
}

pub(crate) fn prepare_initial_head(uri: &str, bytes: Arc<[u8]>) -> Result<(), &'static str> {
    if !bytes.is_empty() && (session_id(uri).is_none() || bytes.len() > INITIAL_HEAD_LIMIT) {
        return Err("initial_head");
    }
    *PENDING_INITIAL_HEAD.lock() = (!bytes.is_empty()).then(|| InitialHead {
        uri: uri.to_string(),
        bytes,
    });
    *PENDING_HTTP_STREAM.lock() = None;
    Ok(())
}

pub(crate) fn prepare_load_stream(
    uri: &str,
    bytes: Arc<[u8]>,
    http: Option<PreparedHttpStream>,
) -> Result<(), &'static str> {
    if let Some(http) = http {
        if uri != http.uri() || !bytes.is_empty() {
            return Err("prepared_http_stream");
        }
        prepare_initial_head(uri, bytes)?;
        *PENDING_HTTP_STREAM.lock() = Some(http);
        Ok(())
    } else {
        if uri.starts_with("mediastation-http://") {
            return Err("prepared_http_stream_missing");
        }
        prepare_initial_head(uri, bytes)
    }
}

fn take_initial_head(pending: &mut Option<InitialHead>, uri: &str) -> Arc<[u8]> {
    if pending.as_ref().is_some_and(|head| head.uri == uri) {
        pending
            .take()
            .map_or_else(|| Arc::from([]), |head| head.bytes)
    } else {
        Arc::from([])
    }
}

pub(crate) fn discard_initial_head(uri: &str) {
    let _ = take_initial_head(&mut PENDING_INITIAL_HEAD.lock(), uri);
    let mut pending = PENDING_HTTP_STREAM.lock();
    if pending.as_ref().is_some_and(|http| http.uri() == uri) {
        *pending = None;
    }
}

pub(crate) fn discard_pending_streams() {
    *PENDING_INITIAL_HEAD.lock() = None;
    *PENDING_HTTP_STREAM.lock() = None;
}

fn take_http_stream(
    pending: &mut Option<PreparedHttpStream>,
    uri: &str,
) -> Option<PreparedHttpStream> {
    if pending.as_ref().is_some_and(|http| http.uri() == uri) {
        pending.take()
    } else {
        None
    }
}

/// The callback may only enqueue work; stream callbacks must never call libmpv.
pub fn set_failure_handler(handler: Option<fn(StreamFailure)>) {
    *FAILURE_HANDLER.lock() = handler;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamError {
    Http { status: u16, message: String },
    Transport(String),
    InvalidResponse(String),
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "http_status_{status}"),
            Self::Transport(cause) | Self::InvalidResponse(cause) => f.write_str(cause),
        }
    }
}

impl std::error::Error for StreamError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamFailure {
    pub session_id: u64,
    pub phase: &'static str,
    pub offset: u64,
    pub error: StreamError,
}

impl StreamFailure {
    pub fn http_status(&self) -> Option<u16> {
        match self.error {
            StreamError::Http { status, .. } => Some(status),
            _ => None,
        }
    }

    pub fn user_message(&self) -> String {
        match &self.error {
            StreamError::Http { status, message } => {
                let message = diagnostic_value(message, "mediastation://");
                if message.is_empty() {
                    format!("读取媒体失败（HTTP {status}）")
                } else {
                    format!("读取媒体失败（HTTP {status}：{message}）")
                }
            }
            StreamError::Transport(_) => "媒体连接中断，读取失败".to_string(),
            StreamError::InvalidResponse(_) => "媒体服务器返回了无效的读取范围".to_string(),
        }
    }
}

/// Correlates a resolved link with stream requests without logging its signed URL.
/// This identifier is local to this binary's diagnostics, not a persistent cache key.
pub fn diagnostic_link_id(url: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    url.hash(&mut hash);
    hash.finish()
}

// ureq's request body timeout would also limit successful, long-lived media reads.
// Bound each wait for new data, not the lifetime of a successful media body.
// Keep the default connector (including TLS/proxy behavior); polling happens
// inside the same read and never issues another request.
#[derive(Clone, Debug, Default)]
struct ErrorBodyDeadline(Arc<Mutex<Option<Instant>>>);

#[derive(Debug)]
struct StreamIo {
    cancelled: AtomicBool,
    idle_timeout: Duration,
}

impl Default for StreamIo {
    fn default() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            idle_timeout: MEDIA_IDLE_TIMEOUT,
        }
    }
}

impl StreamIo {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn check_cancelled(&self) -> Result<(), ureq::Error> {
        if self.is_cancelled() {
            Err(
                std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "stream cancelled")
                    .into(),
            )
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
struct DiagnosticConnector {
    deadline: ErrorBodyDeadline,
    io: Arc<StreamIo>,
}

impl<T: Transport> Connector<T> for DiagnosticConnector {
    type Out = DiagnosticTransport<T>;

    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<T>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| DiagnosticTransport {
            inner,
            deadline: self.deadline.clone(),
            io: self.io.clone(),
        }))
    }
}

#[derive(Debug)]
struct DiagnosticTransport<T> {
    inner: T,
    deadline: ErrorBodyDeadline,
    io: Arc<StreamIo>,
}

impl<T: Transport> Transport for DiagnosticTransport<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.io.check_cancelled()?;
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let started = Instant::now();
        loop {
            self.io.check_cancelled()?;
            let elapsed = started.elapsed();
            let mut remaining = self.io.idle_timeout.saturating_sub(elapsed);
            let mut reason = ureq::Timeout::RecvBody;
            let original_remaining = (*timeout.after).saturating_sub(elapsed);
            if original_remaining <= remaining {
                remaining = original_remaining;
                reason = timeout.reason;
            }
            if let Some(deadline) = *self.deadline.0.lock() {
                remaining = remaining.min(deadline.saturating_duration_since(Instant::now()));
            }
            if remaining.is_zero() {
                return Err(ureq::Error::Timeout(reason));
            }
            let result = self.inner.await_input(NextTimeout {
                after: remaining.min(IO_CANCEL_POLL).into(),
                reason,
            });
            self.io.check_cancelled()?;
            match result {
                // Only the short I/O polling timeout is consumed here. EOF and
                // real transport errors return to the existing recovery path.
                Err(ureq::Error::Timeout(_)) => {}
                result => return result,
            }
        }
    }

    fn is_open(&mut self) -> bool {
        !self.io.is_cancelled() && self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamProxyMode {
    Direct,
    System,
}

impl StreamProxyMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::System => "system",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

fn stream_agent(
    proxy_mode: StreamProxyMode,
    deadline: ErrorBodyDeadline,
    io: Arc<StreamIo>,
) -> Option<ureq::Agent> {
    let proxy = match proxy_mode {
        StreamProxyMode::Direct => None,
        StreamProxyMode::System => Some(ureq::Proxy::try_from_env()?),
    };
    Some(ureq::Agent::with_parts(
        ureq::Agent::config_builder()
            .proxy(proxy)
            // Inspect error responses before preserving the existing failure/retry path.
            .http_status_as_error(false)
            // These stages precede body polling, including TLS establishment
            // during the initial send. Never put a lifetime limit on the body.
            .timeout_resolve(Some(Duration::from_secs(5)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_send_request(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .build(),
        DefaultConnector::default().chain(DiagnosticConnector { deadline, io }),
        DefaultResolver::default(),
    ))
}

/// Serializes read/seek state. Cancellation uses the independent atomic control
/// so it never waits for this mutex while a read owns it.
struct Stream {
    agent: ureq::Agent,
    io: Arc<StreamIo>,
    url: String,
    user_agent: String,
    http_headers: Option<Vec<(String, String)>>,
    seekable: bool,
    content_length: Option<u64>,
    position: u64,
    reader: Option<ureq::BodyReader<'static>>,
    error_body_deadline: ErrorBodyDeadline,
    id: u64,
    link_id: u64,
    request_seq: u64,
    reader_request_seq: u64,
    created_at: Instant,
    reader_opened_at: Option<Instant>,
    first_read_pending: bool,
    session_id: u64,
    failure: Option<StreamFailure>,
    pending_seek: bool,
    initial_head: Arc<[u8]>,
    cached_bytes_read: u64,
    cache_read_logged: bool,
}

impl Stream {
    fn new(
        url: &str,
        user_agent: &str,
        content_length: Option<u64>,
        proxy_mode: StreamProxyMode,
        session_id: u64,
    ) -> Option<Self> {
        let error_body_deadline = ErrorBodyDeadline::default();
        let io = Arc::new(StreamIo::default());
        let agent = stream_agent(proxy_mode, error_body_deadline.clone(), io.clone())?;
        let stream = Self {
            agent,
            io,
            url: url.to_string(),
            user_agent: user_agent.to_string(),
            http_headers: None,
            seekable: true,
            content_length,
            position: 0,
            reader: None,
            error_body_deadline,
            id: NEXT_STREAM_ID.fetch_add(1, Ordering::Relaxed),
            link_id: diagnostic_link_id(url),
            request_seq: 0,
            reader_request_seq: 0,
            created_at: Instant::now(),
            reader_opened_at: None,
            first_read_pending: false,
            session_id,
            failure: None,
            pending_seek: false,
            initial_head: Arc::from([]),
            cached_bytes_read: 0,
            cache_read_logged: false,
        };
        tracing::info!(
            target: "mpv",
            "mediastation stream opened: {} proxy_mode={} proxy_configured={} content_length={:?} user_agent={:?}",
            stream.diagnostic_context(), proxy_mode.as_str(), stream.agent.config().proxy().is_some(),
            content_length, user_agent,
        );
        Some(stream)
    }

    fn diagnostic_context(&self) -> String {
        format!(
            "stream_id={} session_id={} link_id={:016x} request_seq={}",
            self.id, self.session_id, self.link_id, self.request_seq,
        )
    }

    fn redact_http_headers(&self, value: &str) -> String {
        let mut value = value.to_string();
        if let Some(headers) = &self.http_headers {
            for (_, secret) in headers {
                if !secret.is_empty() {
                    value = value.replace(secret, "[redacted]");
                }
            }
        }
        value
    }

    fn fail(&mut self, phase: &'static str, offset: u64, error: StreamError) {
        if self.failure.is_some() || self.cancelled() {
            return;
        }
        self.reader = None;
        let failure = StreamFailure {
            session_id: self.session_id,
            phase,
            offset,
            error,
        };
        tracing::warn!(target: "mpv", "mediastation stream terminal failure: {} phase={} offset={} cause={}",
            self.diagnostic_context(), phase, offset, failure.error);
        self.failure = Some(failure.clone());
        let handler = *FAILURE_HANDLER.lock();
        if let Some(handler) = handler {
            handler(failure);
        }
    }

    fn cancelled(&mut self) -> bool {
        if self.io.is_cancelled() {
            self.reader = None;
            true
        } else {
            false
        }
    }
}

struct StreamCookie {
    inner: Mutex<Stream>,
    io: Arc<StreamIo>,
}

fn stream_from_cookie<'a>(cookie: *mut c_void) -> &'a StreamCookie {
    unsafe { &*(cookie as *const StreamCookie) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadAttempt {
    Bytes(usize),
    Eof,
    Recoverable(&'static str),
}

fn io_error_kind(error: &std::io::Error) -> &'static str {
    if matches!(
        error
            .get_ref()
            .and_then(|error| error.downcast_ref::<ureq::Error>()),
        Some(ureq::Error::Timeout(_))
    ) {
        return "timeout";
    }
    match error.kind() {
        std::io::ErrorKind::ConnectionAborted => "connection_aborted",
        std::io::ErrorKind::ConnectionReset => "connection_reset",
        std::io::ErrorKind::BrokenPipe => "broken_pipe",
        std::io::ErrorKind::NotConnected => "not_connected",
        std::io::ErrorKind::TimedOut => "timed_out",
        std::io::ErrorKind::UnexpectedEof => "unexpected_eof",
        std::io::ErrorKind::WouldBlock => "would_block",
        _ => "io_error",
    }
}

fn ureq_error_kind(error: &ureq::Error) -> String {
    match error {
        ureq::Error::StatusCode(status) => format!("http_status_{status}"),
        ureq::Error::Io(error) => format!("io_{}", io_error_kind(error)),
        ureq::Error::Timeout(_) => "timeout".to_string(),
        ureq::Error::HostNotFound => "host_not_found".to_string(),
        ureq::Error::ConnectionFailed => "connection_failed".to_string(),
        ureq::Error::Protocol(_) => "protocol_error".to_string(),
        ureq::Error::Tls(_) => "tls_error".to_string(),
        _ => "request_error".to_string(),
    }
}

fn content_range_start(value: &str) -> Option<u64> {
    let range = value.strip_prefix("bytes ")?.split_once('/')?.0;
    range.split_once('-')?.0.parse().ok()
}

/// Only selected error fields reach the log. Never dump bodies, cookies, or signed URLs.
fn diagnostic_value(value: &str, url: &str) -> String {
    let mut value = value.replace(url, "[redacted_url]");
    if let Some((_, query)) = url.split_once('?') {
        for parameter in query.split('&') {
            if let Some((_, secret)) = parameter.split_once('=')
                && secret.len() >= 4
            {
                value = value.replace(secret, "[redacted]");
            }
        }
    }
    value
        .split_whitespace()
        .map(|word| {
            if word.contains("://") || word.contains('=') {
                "[redacted]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(256)
        .collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CdnError {
    format: &'static str,
    code: String,
    message: String,
    request_id: String,
    ec: String,
}

fn cdn_error(body: &[u8], url: &str) -> CdnError {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        let field = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| value.get(name))
                .and_then(|value| match value {
                    serde_json::Value::String(text) => Some(text.clone()),
                    serde_json::Value::Number(number) => Some(number.to_string()),
                    _ => None,
                })
                .map(|text| diagnostic_value(&text, url))
                .unwrap_or_default()
        };
        return CdnError {
            format: "json",
            code: field(&["code", "Code"]),
            message: field(&["message", "Message"]),
            request_id: field(&["request_id", "RequestId"]),
            ec: field(&["ec", "EC"]),
        };
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return CdnError {
            format: "unrecognized",
            ..CdnError::default()
        };
    };
    if !text.trim_start().starts_with('<') || !text.contains("<Error>") {
        return CdnError {
            format: "unrecognized",
            ..CdnError::default()
        };
    }
    let field = |name| {
        text.split_once(&format!("<{name}>"))
            .and_then(|(_, rest)| rest.split_once(&format!("</{name}>")))
            .map(|(value, _)| diagnostic_value(value, url))
            .unwrap_or_default()
    };
    CdnError {
        format: "xml",
        code: field("Code"),
        message: field("Message"),
        request_id: field("RequestId"),
        ec: field("EC"),
    }
}

fn read_error_body(mut reader: impl Read, deadline: Instant) -> (Vec<u8>, String) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    let state = loop {
        let remaining = ERROR_BODY_LIMIT as usize - bytes.len();
        if remaining == 0 {
            break "limit_reached";
        }
        if Instant::now() >= deadline {
            break "time_budget_exhausted";
        }
        let capacity = buffer.len().min(remaining);
        match reader.read(&mut buffer[..capacity]) {
            Ok(0) => break "complete",
            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            Err(_) if Instant::now() >= deadline => break "time_budget_exhausted",
            Err(error) => break io_error_kind(&error),
        }
    };
    (bytes, state.to_string())
}

fn log_http_failure(
    stream: &Stream,
    offset: u64,
    phase: &str,
    attempt: usize,
    header_ms: u128,
    response: ureq::http::Response<ureq::Body>,
) -> CdnError {
    let status = response.status().as_u16();
    let host = response.get_uri().host().unwrap_or("").to_string();
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| diagnostic_value(&stream.redact_http_headers(value), &stream.url))
            .unwrap_or_default()
    };
    let server = header("Server");
    let server_date = header("Date");
    let content_type = header("Content-Type");
    let request_id = header("x-oss-request-id");
    let retry_after = header("Retry-After");
    let body_started = Instant::now();
    let deadline = body_started + ERROR_BODY_TIMEOUT;
    *stream.error_body_deadline.0.lock() = Some(deadline);
    let (body, body_state) = read_error_body(response.into_body().into_reader(), deadline);
    *stream.error_body_deadline.0.lock() = None;
    let body_ms = body_started.elapsed().as_millis();
    let redacted_body = stream
        .http_headers
        .as_ref()
        .map(|_| stream.redact_http_headers(&String::from_utf8_lossy(&body)));
    let error = cdn_error(
        redacted_body
            .as_ref()
            .map_or(body.as_slice(), |body| body.as_bytes()),
        &stream.url,
    );
    tracing::warn!(
        target: "mpv",
        "mediastation stream HTTP failure: {} phase={} attempt={} status={} host={} proxy_configured={} range=bytes={}- previous_offset={} old_reader_open={} header_ms={} body_ms={} user_agent={:?} server={:?} server_date={:?} content_type={:?} retry_after={:?} header_request_id={:?} body_state={} body_bytes={} error_format={} cdn_code={:?} cdn_message={:?} cdn_request_id={:?} cdn_ec={:?}",
        stream.diagnostic_context(), phase, attempt, status, host, stream.agent.config().proxy().is_some(),
        offset, stream.position, stream.reader.is_some(),
        header_ms, body_ms, stream.user_agent, server, server_date, content_type, retry_after, request_id,
        body_state, body.len(), error.format, error.code, error.message, error.request_id, error.ec,
    );
    error
}

fn open_reader_at(
    stream: &mut Stream,
    offset: u64,
    phase: &str,
    attempt: usize,
) -> Result<(), StreamError> {
    let (response, started) = request_range(stream, offset, phase, attempt)?;
    install_reader(stream, response, started, offset, phase, attempt)
}

fn request_range(
    stream: &mut Stream,
    offset: u64,
    phase: &str,
    attempt: usize,
) -> Result<(ureq::http::Response<ureq::Body>, Instant), StreamError> {
    if stream.cancelled() {
        return Err(StreamError::Transport("cancelled".to_string()));
    }
    if let Some(failure) = &stream.failure {
        return Err(failure.error.clone());
    }
    stream.request_seq += 1;
    let range = format!("bytes={offset}-");
    let started = Instant::now();
    tracing::info!(
        target: "mpv",
        "mediastation stream range request: {} phase={} attempt={} range={} previous_offset={} old_reader_open={} old_reader_request_seq={} stream_age_ms={} user_agent={:?}",
        stream.diagnostic_context(), phase, attempt, range, stream.position,
        stream.reader.is_some(), stream.reader_request_seq, stream.created_at.elapsed().as_millis(), stream.user_agent,
    );
    // Drop the abandoned body before opening its replacement, including retries.
    // A failed replacement must never leave reads attached to the old position.
    stream.reader = None;
    stream.first_read_pending = false;
    let mut request = stream
        .agent
        .get(stream.url.as_str())
        .header("User-Agent", stream.user_agent.as_str())
        .header("Range", range.as_str());
    if let Some(headers) = &stream.http_headers {
        request = request
            .config()
            .max_redirects(0)
            .max_redirects_will_error(false)
            .build()
            .header("Accept-Encoding", "identity");
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
    }
    let response = request
        .call()
        .map_err(|error| {
            if stream.io.is_cancelled() {
                return StreamError::Transport("cancelled".to_string());
            }
            let cause = ureq_error_kind(&error);
            tracing::warn!(
                target: "mpv",
                "mediastation stream range transport failed: {} phase={} attempt={} range={} elapsed_ms={} cause={}",
                stream.diagnostic_context(), phase, attempt, range, started.elapsed().as_millis(), cause,
            );
            StreamError::Transport(cause)
        })?;
    if stream.cancelled() {
        return Err(StreamError::Transport("cancelled".to_string()));
    }
    Ok((response, started))
}

fn install_reader(
    stream: &mut Stream,
    response: ureq::http::Response<ureq::Body>,
    started: Instant,
    offset: u64,
    phase: &str,
    attempt: usize,
) -> Result<(), StreamError> {
    let range = format!("bytes={offset}-");
    let status = response.status().as_u16();
    let header_ms = started.elapsed().as_millis();
    let content_range = response
        .headers()
        .get("Content-Range")
        .and_then(|value| value.to_str().ok());
    tracing::info!(
        target: "mpv",
        "mediastation stream range response: {} phase={} attempt={} status={} host={} range={} header_ms={} content_range={:?} content_length={:?}",
        stream.diagnostic_context(), phase, attempt, status, response.get_uri().host().unwrap_or_default(),
        range, header_ms, content_range.map(|value| diagnostic_value(value, &stream.url)), response.body().content_length(),
    );
    if response.status().is_client_error() || response.status().is_server_error() {
        let error = log_http_failure(stream, offset, phase, attempt, header_ms, response);
        return Err(StreamError::Http {
            status,
            message: error.message,
        });
    }

    let validation = (|| {
        if status == 206 {
            let content_range = content_range.ok_or_else(|| "content_range_missing".to_string())?;
            let actual_offset = content_range_start(content_range)
                .ok_or_else(|| "content_range_invalid".to_string())?;
            if actual_offset != offset {
                return Err(format!(
                    "content_range_offset_mismatch_expected_{offset}_actual_{actual_offset}"
                ));
            }
        } else if offset != 0 || status != 200 {
            return Err(format!("unexpected_http_status_{status}"));
        }
        Ok(())
    })();
    validation.inspect_err(|cause| {
        tracing::warn!(
            target: "mpv",
            "mediastation stream range rejected: {} phase={} attempt={} status={} range={} cause={}",
            stream.diagnostic_context(), phase, attempt, status, range, cause,
        );
    }).map_err(StreamError::InvalidResponse)?;

    if stream.http_headers.is_some() {
        let invalid = |cause: &str| StreamError::InvalidResponse(cause.to_string());
        if response
            .headers()
            .get("Content-Encoding")
            .is_some_and(|encoding| encoding != "identity")
        {
            return Err(invalid("encoded_media_response"));
        }
        let length = if status == 206 {
            let (range, total) = content_range
                .and_then(|value| value.strip_prefix("bytes "))
                .and_then(|value| value.split_once('/'))
                .ok_or_else(|| invalid("content_range_invalid"))?;
            let (start, end) = range
                .split_once('-')
                .ok_or_else(|| invalid("content_range_invalid"))?;
            let start = start
                .parse::<u64>()
                .map_err(|_| invalid("content_range_invalid"))?;
            let end = end
                .parse::<u64>()
                .map_err(|_| invalid("content_range_invalid"))?;
            let total = if total == "*" {
                None
            } else {
                Some(
                    total
                        .parse::<u64>()
                        .map_err(|_| invalid("content_range_invalid"))?,
                )
            };
            if end < start
                || total.is_some_and(|total| end >= total)
                || response.body().content_length().is_some_and(|length| {
                    Some(length) != end.checked_sub(start).and_then(|v| v.checked_add(1))
                })
            {
                return Err(invalid("content_range_length_mismatch"));
            }
            total
        } else {
            response.body().content_length()
        };
        if stream.content_length.is_some() && length != stream.content_length {
            return Err(invalid("content_length_changed"));
        }
        stream.content_length = length;
    }

    stream.reader = Some(response.into_body().into_reader());
    stream.position = offset;
    stream.reader_request_seq = stream.request_seq;
    stream.reader_opened_at = Some(started);
    stream.first_read_pending = true;
    Ok(())
}

fn read_once(stream: &mut Stream, out: &mut [u8]) -> ReadAttempt {
    if stream.cancelled() {
        return ReadAttempt::Recoverable("cancelled");
    }
    if out.is_empty() {
        return ReadAttempt::Eof;
    }
    let Some(reader) = stream.reader.as_mut() else {
        return ReadAttempt::Recoverable("reader_missing");
    };
    let result = reader.read(out);
    if stream.cancelled() {
        return ReadAttempt::Recoverable("cancelled");
    }
    match result {
        Ok(0)
            if stream
                .content_length
                .is_some_and(|length| stream.position < length) =>
        {
            ReadAttempt::Recoverable("premature_eof")
        }
        Ok(0) => ReadAttempt::Eof,
        Ok(n) => {
            if stream.first_read_pending {
                tracing::info!(
                    target: "mpv",
                    "mediastation stream first bytes: {} reader_request_seq={} offset={} bytes={} request_elapsed_ms={}",
                    stream.diagnostic_context(), stream.reader_request_seq, stream.position, n,
                    stream.reader_opened_at.map(|started| started.elapsed().as_millis()).unwrap_or_default(),
                );
                stream.first_read_pending = false;
            }
            stream.position += n as u64;
            ReadAttempt::Bytes(n)
        }
        Err(error) => ReadAttempt::Recoverable(io_error_kind(&error)),
    }
}

fn read_with_recovery(stream: &mut Stream, out: &mut [u8]) -> i64 {
    if stream.failure.is_some() || stream.cancelled() {
        return -1;
    }
    if out.is_empty() {
        return 0;
    }
    if stream.pending_seek && stream.position < stream.initial_head.len() as u64 {
        let offset = stream.position as usize;
        let count = out.len().min(stream.initial_head.len() - offset);
        out[..count].copy_from_slice(&stream.initial_head[offset..offset + count]);
        if !stream.cache_read_logged {
            tracing::info!(target: "mpv",
                "mediastation stream initial head reused: {} offset={} available_bytes={} stream_age_ms={}",
                stream.diagnostic_context(), offset, stream.initial_head.len() - offset,
                stream.created_at.elapsed().as_millis());
            stream.cache_read_logged = true;
        }
        stream.position += count as u64;
        stream.cached_bytes_read += count as u64;
        // Keep the seek pending. If parsing continues beyond the prefix, its
        // first network read must open exactly at the new logical position.
        return count as i64;
    }
    if stream.pending_seek
        && stream.content_length == Some(stream.position)
        && stream.position == stream.initial_head.len() as u64
    {
        return 0;
    }
    if stream.pending_seek {
        stream.pending_seek = false;
        if open_seek_reader(stream) < 0 {
            return -1;
        }
    }
    let mut cause = match read_once(stream, out) {
        ReadAttempt::Bytes(n) => return n as i64,
        ReadAttempt::Eof => return 0,
        ReadAttempt::Recoverable(cause) => StreamError::Transport(cause.to_string()),
    };
    let offset = stream.position;

    for attempt in 1..=READ_RECOVERY_ATTEMPTS {
        if stream.cancelled() {
            return -1;
        }
        tracing::warn!(
            target: "mpv",
            "mediastation stream read interrupted; retrying range: {} offset={} attempt={}/{} cause={}",
            stream.diagnostic_context(),
            offset,
            attempt,
            READ_RECOVERY_ATTEMPTS,
            cause,
        );
        std::thread::sleep(std::time::Duration::from_millis(
            RETRY_BASE_DELAY_MS * attempt as u64,
        ));
        if stream.cancelled() {
            return -1;
        }
        match open_reader_at(stream, offset, "read_recovery", attempt) {
            Ok(()) => match read_once(stream, out) {
                ReadAttempt::Bytes(n) => {
                    tracing::info!(
                        target: "mpv",
                        "mediastation stream read recovered: {} offset={} attempt={}",
                        stream.diagnostic_context(),
                        offset,
                        attempt,
                    );
                    return n as i64;
                }
                ReadAttempt::Eof => return 0,
                ReadAttempt::Recoverable(next_cause) => {
                    cause = StreamError::Transport(next_cause.to_string())
                }
            },
            Err(error) => cause = error,
        }
    }

    if stream.cancelled() {
        return -1;
    }
    tracing::warn!(
        target: "mpv",
        "mediastation stream read failed after bounded recovery: {} offset={} attempts={} cause={}",
        stream.diagnostic_context(),
        offset,
        READ_RECOVERY_ATTEMPTS,
        cause,
    );
    stream.fail("read_recovery", offset, cause);
    -1
}

unsafe extern "C" fn read_cb(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64 {
    let stream = stream_from_cookie(cookie);
    let mut inner = stream.inner.lock();
    let out = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, nbytes as usize) };
    read_with_recovery(&mut inner, out)
}

unsafe extern "C" fn seek_cb(cookie: *mut c_void, offset: i64) -> i64 {
    let stream = stream_from_cookie(cookie);
    let mut inner = stream.inner.lock();
    seek_to(&mut inner, offset)
}

fn seek_to(inner: &mut Stream, offset: i64) -> i64 {
    if inner.failure.is_some() || inner.cancelled() {
        return MPV_ERROR_GENERIC;
    }
    if offset < 0 {
        return MPV_ERROR_UNSUPPORTED;
    }
    if offset as u64 == inner.position && inner.reader.is_some() {
        return offset;
    }
    tracing::info!(
        target: "mpv",
        "mediastation stream seek scheduled: {} offset={} previous_offset={} old_reader_open={} replaced_pending_seek={}",
        inner.diagnostic_context(), offset, inner.position, inner.reader.is_some(), inner.pending_seek,
    );
    // Seeking changes the logical cursor. Only a subsequent read needs HTTP
    // validation, so unused intermediate seeks never open CDN connections.
    // Release the previous reader now, even if this seek is later superseded.
    inner.reader = None;
    inner.reader_opened_at = None;
    inner.first_read_pending = false;
    inner.position = offset as u64;
    inner.pending_seek = true;
    inner.cache_read_logged = false;
    offset
}

fn open_seek_reader(inner: &mut Stream) -> i64 {
    let offset = inner.position;
    let mut last_error = None;
    for attempt in 0..RANGE_OPEN_ATTEMPTS {
        if inner.cancelled() {
            return MPV_ERROR_GENERIC;
        }
        match open_reader_at(inner, offset, "seek", attempt + 1) {
            Ok(()) => return offset as i64,
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < RANGE_OPEN_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(
                        RETRY_BASE_DELAY_MS * (attempt + 1) as u64,
                    ));
                }
            }
        }
    }
    if inner.cancelled() {
        return MPV_ERROR_GENERIC;
    }
    tracing::warn!(
        target: "mpv",
        "mediastation stream seek failed after retries: {} offset={} attempts={} cause={}",
        inner.diagnostic_context(),
        offset,
        RANGE_OPEN_ATTEMPTS,
        last_error.as_ref().map(ToString::to_string).unwrap_or_default(),
    );
    if let Some(error) = last_error {
        inner.fail("seek", offset, error);
    }
    MPV_ERROR_GENERIC
}

unsafe extern "C" fn size_cb(cookie: *mut c_void) -> i64 {
    let stream = stream_from_cookie(cookie);
    let inner = stream.inner.lock();
    match inner.content_length {
        Some(size) => size as i64,
        None => MPV_ERROR_UNSUPPORTED,
    }
}

unsafe extern "C" fn close_cb(cookie: *mut c_void) {
    if cookie.is_null() {
        return;
    }
    // Reclaim the leaked Box and record the actual stream lifetime.
    let cookie = unsafe { Box::from_raw(cookie as *mut StreamCookie) };
    let inner = cookie.inner.lock();
    tracing::info!(
        target: "mpv",
        "mediastation stream closed: {} position={} reader_open={} lifetime_ms={} initial_head_bytes={} cached_bytes_read={} cancelled={}",
        inner.diagnostic_context(), inner.position, inner.reader.is_some(), inner.created_at.elapsed().as_millis(),
        inner.initial_head.len(), inner.cached_bytes_read, inner.io.is_cancelled(),
    );
}

unsafe extern "C" fn cancel_cb(cookie: *mut c_void) {
    // mpv calls this on a different thread while read_cb may own inner.
    // Never acquire that mutex or call back into libmpv from here.
    stream_from_cookie(cookie)
        .io
        .cancelled
        .store(true, Ordering::Release);
}

unsafe extern "C" fn open_cb(
    _user_data: *mut c_void,
    uri: *mut c_char,
    info: *mut sys::mpv_stream_cb_info,
) -> c_int {
    if uri.is_null() || info.is_null() {
        return MPV_ERROR_LOADING_FAILED;
    }
    let uri = unsafe { CStr::from_ptr(uri) }.to_bytes();
    let uri = String::from_utf8_lossy(uri).into_owned();
    if uri.starts_with("mediastation-http://") {
        let mut pending = PENDING_HTTP_STREAM.lock();
        let Some(http) = take_http_stream(&mut pending, &uri) else {
            tracing::warn!(target: "mpv", "mediastation HTTP response unavailable for native load");
            return MPV_ERROR_LOADING_FAILED;
        };
        drop(pending);
        tracing::info!(target: "mpv", "mediastation HTTP response handed to native stream: {} response_age_ms={}",
            http.stream.diagnostic_context(), http.stream.reader_opened_at.map(|time| time.elapsed().as_millis()).unwrap_or_default());
        return unsafe { install_cookie(http.stream, info) };
    }
    // The final field identifies a single load, even when its link is reused.
    let rest = uri.strip_prefix("mediastation://").unwrap_or(&uri);
    let mut parts = rest.split('|');
    let Some(url) = parts.next() else {
        return MPV_ERROR_LOADING_FAILED;
    };
    let user_agent = parts.next().unwrap_or("MediaStationGoWindows/0.1.0-dev");
    let content_length = parts.next().and_then(|v| v.parse::<u64>().ok());
    let Some(proxy_mode) = parts
        .next()
        .map(StreamProxyMode::from_str)
        .unwrap_or(Some(StreamProxyMode::Direct))
    else {
        return MPV_ERROR_LOADING_FAILED;
    };
    let Some(session_id) = parts
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|id| *id != 0)
    else {
        return MPV_ERROR_LOADING_FAILED;
    };
    let Some(mut stream) = Stream::new(url, user_agent, content_length, proxy_mode, session_id)
    else {
        tracing::warn!(target: "mpv", "mediastation stream open failed: cause=proxy_unavailable proxy_mode={}", proxy_mode.as_str());
        return MPV_ERROR_LOADING_FAILED;
    };
    let initial_head = take_initial_head(&mut PENDING_INITIAL_HEAD.lock(), &uri);
    if content_length.is_some_and(|length| initial_head.len() as u64 > length) {
        tracing::warn!(target: "mpv", "mediastation stream initial head exceeds content length");
        return MPV_ERROR_LOADING_FAILED;
    }
    stream.initial_head = initial_head;
    unsafe { install_cookie(stream, info) }
}

unsafe fn install_cookie(stream: Stream, info: *mut sys::mpv_stream_cb_info) -> c_int {
    let seekable = stream.seekable;
    let cookie = StreamCookie {
        io: stream.io.clone(),
        inner: Mutex::new(stream),
    };
    let cookie_ptr = Box::into_raw(Box::new(cookie)) as *mut c_void;
    let info = unsafe { &mut *info };
    info.cookie = cookie_ptr;
    info.read_fn = Some(read_cb);
    info.seek_fn = seekable.then_some(seek_cb);
    info.size_fn = Some(size_cb);
    info.close_fn = Some(close_cb);
    info.cancel_fn = Some(cancel_cb);
    0
}

/// Register the `mediastation://` protocol on the given mpv handle.
///
/// Must be called after `mpv_create` and before the handle is used to load
/// media. Safe to call once; a second registration on the same handle is
/// rejected by libmpv and ignored here.
///
/// # Safety
/// A non-null handle must point to a live mpv instance and remain valid for
/// this call. Registered callbacks remain valid until that instance terminates.
pub unsafe fn register_protocol(handle: *mut sys::mpv_handle) {
    if handle.is_null() {
        return;
    }
    for protocol in [PROTOCOL, b"mediastation-http"] {
        let protocol = CString::new(protocol).unwrap_or_default();
        let status = unsafe {
            sys::mpv_stream_cb_add_ro(handle, protocol.as_ptr(), ptr::null_mut(), Some(open_cb))
        };
        if status != 0 {
            tracing::warn!(target: "mpv", "mediastation stream protocol registration failed: {status}");
        } else {
            tracing::info!(target: "mpv", "mediastation stream protocol registered");
        }
    }
}

/// Build a `mediastation://` URI carrying the target URL, user agent, and
/// optional content length, and server-selected proxy mode.
pub fn build_uri(
    url: &str,
    user_agent: &str,
    content_length: Option<u64>,
    proxy_mode: StreamProxyMode,
) -> String {
    build_uri_with_session(
        url,
        user_agent,
        content_length,
        proxy_mode,
        NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
    )
}

pub fn session_id(uri: &str) -> Option<u64> {
    if let Some(id) = uri.strip_prefix("mediastation-http://") {
        return id.parse().ok().filter(|id| *id != 0);
    }
    uri.strip_prefix("mediastation://")?
        .split('|')
        .nth(4)?
        .parse()
        .ok()
        .filter(|id| *id != 0)
}

fn build_uri_with_session(
    url: &str,
    user_agent: &str,
    content_length: Option<u64>,
    proxy_mode: StreamProxyMode,
    session_id: u64,
) -> String {
    let content_length = content_length
        .map(|value| value.to_string())
        .unwrap_or_default();
    format!(
        "mediastation://{url}|{user_agent}|{content_length}|{}|{session_id}",
        proxy_mode.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestResponse {
        status: u16,
        reason: &'static str,
        content_range: Option<String>,
        declared_length: usize,
        body: Vec<u8>,
    }

    fn response(
        status: u16,
        reason: &'static str,
        content_range: Option<&str>,
        declared_length: usize,
        body: &[u8],
    ) -> TestResponse {
        TestResponse {
            status,
            reason,
            content_range: content_range.map(str::to_string),
            declared_length,
            body: body.to_vec(),
        }
    }

    fn spawn_range_server(
        responses: Vec<TestResponse>,
    ) -> std::io::Result<(
        String,
        std::thread::JoinHandle<std::io::Result<Vec<String>>>,
    )> {
        spawn_range_server_with_delay(responses, Duration::ZERO)
    }

    fn spawn_range_server_with_delay(
        responses: Vec<TestResponse>,
        delay: Duration,
    ) -> std::io::Result<(
        String,
        std::thread::JoinHandle<std::io::Result<Vec<String>>>,
    )> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::{Shutdown, TcpListener};

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let handle = std::thread::spawn(move || {
            let mut ranges = Vec::with_capacity(responses.len());
            for response in responses {
                let (mut stream, _) = listener.accept()?;
                let mut reader = BufReader::new(stream.try_clone()?);
                let mut range = String::new();
                loop {
                    let mut line = String::new();
                    let bytes = reader.read_line(&mut line)?;
                    if bytes == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("Range")
                    {
                        range = value.trim().to_string();
                    }
                }
                ranges.push(range);
                std::thread::sleep(delay);
                let content_range = response
                    .content_range
                    .map_or(String::new(), |value| format!("Content-Range: {value}\r\n"));
                let headers = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
                    response.status, response.reason, response.declared_length, content_range,
                );
                stream.write_all(headers.as_bytes())?;
                stream.write_all(&response.body)?;
                stream.flush()?;
                let _ = stream.shutdown(Shutdown::Both);
            }
            Ok(ranges)
        });
        Ok((format!("http://{address}/video"), handle))
    }

    fn test_stream(url: &str, content_length: u64) -> std::io::Result<Stream> {
        Stream::new(
            url,
            "MediaStationGoWindows/test",
            Some(content_length),
            StreamProxyMode::Direct,
            NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
        )
        .ok_or_else(|| std::io::Error::other("direct stream agent unavailable"))
    }

    fn join_ranges(
        handle: std::thread::JoinHandle<std::io::Result<Vec<String>>>,
    ) -> std::io::Result<Vec<String>> {
        match handle.join() {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other("range server thread panicked")),
        }
    }

    fn spawn_delayed_body_server(
        status: u16,
        delay: Duration,
    ) -> std::io::Result<(String, std::thread::JoinHandle<std::io::Result<()>>)> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let handle = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut reader = BufReader::new(socket.try_clone()?);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line)? == 0 || line == "\r\n" {
                    break;
                }
            }
            write!(
                socket,
                "HTTP/1.1 {status} Test\r\nContent-Length: 2\r\nContent-Range: bytes 0-1/2\r\nConnection: close\r\n\r\n"
            )?;
            socket.flush()?;
            std::thread::sleep(delay);
            match socket.write_all(b"{}") {
                // An error-body timeout deliberately closes this connection.
                Err(error)
                    if status == 403
                        && matches!(
                            error.kind(),
                            std::io::ErrorKind::BrokenPipe
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                {
                    Ok(())
                }
                result => result,
            }
        });
        Ok((format!("http://{address}/video"), handle))
    }

    fn join_delayed_server(
        handle: std::thread::JoinHandle<std::io::Result<()>>,
    ) -> std::io::Result<()> {
        handle
            .join()
            .map_err(|_| std::io::Error::other("delayed server thread panicked"))?
    }

    // The peer stays connected until explicitly released, reproducing a stalled
    // CDN without relying on EOF to unblock the client's read.
    fn stalled_peer_cancellation(send_body: bool) -> std::io::Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let url = format!("http://{}/video", listener.local_addr()?);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut request = BufReader::new(socket.try_clone()?);
            loop {
                let mut line = String::new();
                if request.read_line(&mut line)? == 0 || line == "\r\n" {
                    break;
                }
            }
            if send_body {
                socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-1/2\r\nContent-Length: 2\r\nConnection: close\r\n\r\na")?;
                socket.flush()?;
            }
            ready_tx.send(()).map_err(std::io::Error::other)?;
            release_rx
                .recv_timeout(Duration::from_secs(4))
                .map_err(std::io::Error::other)?;
            Ok(())
        });
        let stream = test_stream(&url, 2)?;
        let cookie = Arc::new(StreamCookie {
            io: stream.io.clone(),
            inner: Mutex::new(stream),
        });
        let (done_tx, done_rx) = mpsc::channel();
        let reader_cookie = cookie.clone();
        let worker = std::thread::spawn(move || {
            let cookie_ptr = Arc::as_ptr(&reader_cookie) as *mut c_void;
            let mut byte = [0_u8];
            if send_body {
                assert_eq!(
                    unsafe { read_cb(cookie_ptr, byte.as_mut_ptr().cast(), 1) },
                    1
                );
                assert_eq!(byte, *b"a");
            }
            let result = unsafe { read_cb(cookie_ptr, byte.as_mut_ptr().cast(), 1) };
            done_tx.send(result).unwrap();
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(std::io::Error::other)?;
        std::thread::sleep(Duration::from_millis(120));
        assert!(
            cookie.inner.is_locked(),
            "read must be in flight while canceled"
        );
        let started = Instant::now();
        unsafe { cancel_cb(Arc::as_ptr(&cookie) as *mut c_void) };
        let completed = done_rx.recv_timeout(Duration::from_secs(1));
        let elapsed = started.elapsed();
        // Release and join even when the old implementation fails the deadline.
        release_tx.send(()).map_err(std::io::Error::other)?;
        server
            .join()
            .map_err(|_| std::io::Error::other("stalled peer panicked"))??;
        worker
            .join()
            .map_err(|_| std::io::Error::other("read worker panicked"))?;
        assert_eq!(
            completed,
            Ok(-1),
            "cancellation did not unblock read: {elapsed:?}"
        );
        let mut stream = cookie.inner.lock();
        assert!(
            stream.failure.is_none(),
            "user cancellation is not a stream failure"
        );
        assert_eq!(
            stream.request_seq, 1,
            "cancellation must not retry the Range"
        );
        let mut byte = [0_u8];
        assert_eq!(read_with_recovery(&mut stream, &mut byte), -1);
        assert_eq!(seek_to(&mut stream, 0), MPV_ERROR_GENERIC);
        assert_eq!(stream.request_seq, 1);
        eprintln!("stalled peer canceled: body={send_body} elapsed={elapsed:?}");
        Ok(())
    }

    #[test]
    fn cancellation_interrupts_a_stalled_media_body_without_recovery() -> std::io::Result<()> {
        stalled_peer_cancellation(true)
    }

    #[test]
    fn cancellation_interrupts_waiting_for_range_headers_without_recovery() -> std::io::Result<()> {
        stalled_peer_cancellation(false)
    }

    fn test_stream_with_idle_timeout(
        url: &str,
        length: u64,
        idle: Duration,
    ) -> std::io::Result<Stream> {
        let mut stream = test_stream(url, length)?;
        stream.io = Arc::new(StreamIo {
            cancelled: AtomicBool::new(false),
            idle_timeout: idle,
        });
        stream.agent = stream_agent(
            StreamProxyMode::Direct,
            stream.error_body_deadline.clone(),
            stream.io.clone(),
        )
        .ok_or_else(|| std::io::Error::other("direct stream agent unavailable"))?;
        Ok(stream)
    }

    #[test]
    fn stalled_successful_body_returns_an_explicit_idle_timeout() -> std::io::Result<()> {
        let (url, server) = spawn_delayed_body_server(206, Duration::from_millis(500))?;
        let mut stream = test_stream_with_idle_timeout(&url, 2, Duration::from_millis(150))?;
        open_reader_at(&mut stream, 0, "seek", 1).map_err(std::io::Error::other)?;
        let mut bytes = [0_u8; 2];
        let result = read_once(&mut stream, &mut bytes);
        join_delayed_server(server)?;
        assert_eq!(result, ReadAttempt::Recoverable("timeout"));
        assert_eq!(stream.position, 0);
        Ok(())
    }

    #[test]
    fn idle_timeouts_exhaust_existing_recovery_and_remain_a_terminal_error() -> std::io::Result<()>
    {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let url = format!("http://{}/video", listener.local_addr()?);
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            for _ in 0..=READ_RECOVERY_ATTEMPTS {
                let (mut socket, _) = listener.accept()?;
                socket.set_read_timeout(Some(Duration::from_secs(2)))?;
                let mut request = BufReader::new(socket.try_clone()?);
                loop {
                    let mut line = String::new();
                    if request.read_line(&mut line)? == 0 || line == "\r\n" {
                        break;
                    }
                }
                socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-1/2\r\nContent-Length: 2\r\nConnection: close\r\n\r\n")?;
                socket.flush()?;
                let mut byte = [0_u8];
                // The client must time out and close; the peer never sends EOF.
                assert_eq!(socket.read(&mut byte)?, 0);
            }
            Ok(())
        });
        let mut stream = test_stream_with_idle_timeout(&url, 2, Duration::from_millis(150))?;
        assert_eq!(seek_to(&mut stream, 0), 0);
        let mut byte = [0_u8];
        assert_eq!(read_with_recovery(&mut stream, &mut byte), -1);
        join_delayed_server(server)?;
        let failure = stream
            .failure
            .clone()
            .expect("idle timeout must be reported");
        assert_eq!(failure.error, StreamError::Transport("timeout".to_string()));
        assert_eq!(failure.phase, "read_recovery");
        assert_eq!(stream.request_seq, (READ_RECOVERY_ATTEMPTS + 1) as u64);
        assert_eq!(read_with_recovery(&mut stream, &mut byte), -1);
        assert_eq!(seek_to(&mut stream, 0), MPV_ERROR_GENERIC);
        assert_eq!(stream.failure, Some(failure));
        assert_eq!(stream.request_seq, (READ_RECOVERY_ATTEMPTS + 1) as u64);
        Ok(())
    }

    #[test]
    fn successful_body_can_outlive_the_idle_timeout_while_data_keeps_arriving()
    -> std::io::Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let url = format!("http://{}/video", listener.local_addr()?);
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut socket, _) = listener.accept()?;
            socket.set_read_timeout(Some(Duration::from_secs(2)))?;
            socket.set_nodelay(true)?;
            let mut request = BufReader::new(socket.try_clone()?);
            loop {
                let mut line = String::new();
                if request.read_line(&mut line)? == 0 || line == "\r\n" {
                    break;
                }
            }
            socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-5/6\r\nContent-Length: 6\r\nConnection: close\r\n\r\n")?;
            for byte in b"abcdef" {
                std::thread::sleep(Duration::from_millis(150));
                socket.write_all(&[*byte])?;
            }
            Ok(())
        });
        let idle = Duration::from_millis(400);
        let mut stream = test_stream_with_idle_timeout(&url, 6, idle)?;
        let started = Instant::now();
        let mut actual = Vec::new();
        assert_eq!(seek_to(&mut stream, 0), 0);
        loop {
            let mut byte = [0_u8];
            let count = read_with_recovery(&mut stream, &mut byte);
            if count <= 0 {
                assert_eq!(count, 0, "progressing body failed: {:?}", stream.failure);
                break;
            }
            actual.push(byte[0]);
        }
        join_delayed_server(server)?;
        assert_eq!(actual, b"abcdef");
        assert!(started.elapsed() > idle);
        assert_eq!(stream.request_seq, 1, "polling must not reopen the Range");
        Ok(())
    }

    #[test]
    fn cancellation_prevents_reading_the_cached_head_or_starting_a_new_range() -> std::io::Result<()>
    {
        let mut stream = test_stream("http://127.0.0.1:1/unused", 6)?;
        stream.initial_head = Arc::from(b"abcdef".as_slice());
        assert_eq!(seek_to(&mut stream, 0), 0);
        let cookie = Box::new(StreamCookie {
            io: stream.io.clone(),
            inner: Mutex::new(stream),
        });
        unsafe { cancel_cb((&*cookie as *const StreamCookie).cast_mut().cast()) };
        let mut stream = cookie.inner.lock();
        let mut bytes = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), -1);
        assert_eq!(seek_to(&mut stream, 0), MPV_ERROR_GENERIC);
        assert_eq!(stream.cached_bytes_read, 0);
        assert_eq!(stream.request_seq, 0);
        assert!(stream.failure.is_none());
        Ok(())
    }

    #[test]
    fn local_https_polling_keeps_tls_usable_and_interrupts_a_stalled_body()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;

        // Public test-only identity, trusted only by this loopback client.
        let pem = include_bytes!("../tests/fixtures/loopback-cert.pem");
        let cert = ureq::tls::Certificate::from_pem(pem)?;
        let key =
            ureq::tls::PrivateKey::from_pem(include_bytes!("../tests/fixtures/loopback-key.pem"))?;
        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert.der().to_vec())],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                key.der().to_vec(),
            )),
        )?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let base = format!("https://{}", listener.local_addr()?);
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            for path in ["/progress", "/stall"] {
                let (socket, _) = listener.accept()?;
                socket.set_read_timeout(Some(Duration::from_secs(3)))?;
                socket.set_write_timeout(Some(Duration::from_secs(3)))?;
                socket.set_nodelay(true)?;
                let connection = rustls::ServerConnection::new(Arc::new(server_config.clone()))
                    .map_err(std::io::Error::other)?;
                let mut peer = rustls::StreamOwned::new(connection, socket);
                let mut request = BufReader::new(&mut peer);
                let mut first_line = String::new();
                request.read_line(&mut first_line)?;
                assert!(first_line.starts_with(&format!("GET {path} ")));
                loop {
                    let mut line = String::new();
                    if request.read_line(&mut line)? == 0 || line == "\r\n" {
                        break;
                    }
                }
                peer.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-5/6\r\nContent-Length: 6\r\nConnection: close\r\n\r\n")?;
                peer.flush()?;
                if path == "/progress" {
                    for byte in b"abcdef" {
                        std::thread::sleep(Duration::from_millis(180));
                        peer.write_all(&[*byte])?;
                        peer.flush()?;
                    }
                    peer.conn.send_close_notify();
                    peer.flush()?;
                } else {
                    peer.write_all(b"a")?;
                    peer.flush()?;
                    let mut byte = [0_u8];
                    match peer.read(&mut byte) {
                        Ok(0) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::UnexpectedEof
                                    | std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::ConnectionAborted
                            ) => {}
                        result => {
                            return Err(std::io::Error::other(format!(
                                "TLS client did not close canceled stream: {result:?}"
                            )));
                        }
                    }
                }
            }
            Ok(())
        });
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::Specific(Arc::new(vec![cert])))
            .build();
        let make_stream = |path: &str| -> std::io::Result<Stream> {
            let mut stream = test_stream_with_idle_timeout(
                &format!("{base}/{path}"),
                6,
                Duration::from_millis(600),
            )?;
            stream.agent = ureq::Agent::with_parts(
                ureq::Agent::config_builder()
                    .proxy(None)
                    .tls_config(tls.clone())
                    .build(),
                DefaultConnector::default().chain(DiagnosticConnector {
                    deadline: stream.error_body_deadline.clone(),
                    io: stream.io.clone(),
                }),
                DefaultResolver::default(),
            );
            Ok(stream)
        };
        let mut progress = make_stream("progress")?;
        let started = Instant::now();
        assert_eq!(seek_to(&mut progress, 0), 0);
        let mut actual = Vec::new();
        loop {
            let mut byte = [0_u8];
            let n = read_with_recovery(&mut progress, &mut byte);
            if n <= 0 {
                assert_eq!(n, 0, "TLS body failed: {:?}", progress.failure);
                break;
            }
            actual.push(byte[0]);
        }
        assert_eq!(actual, b"abcdef");
        assert!(started.elapsed() > progress.io.idle_timeout);
        assert_eq!(progress.request_seq, 1);

        let mut stalled = make_stream("stall")?;
        assert_eq!(seek_to(&mut stalled, 0), 0);
        let mut byte = [0_u8];
        assert_eq!(read_with_recovery(&mut stalled, &mut byte), 1);
        let io = stalled.io.clone();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let n = read_with_recovery(&mut stalled, &mut byte);
            tx.send((n, stalled.failure.is_none(), stalled.request_seq))
                .unwrap();
        });
        std::thread::sleep(Duration::from_millis(150));
        let started = Instant::now();
        io.cancelled.store(true, Ordering::Release);
        let result = rx.recv_timeout(Duration::from_secs(1));
        worker.join().map_err(|_| "TLS read worker panicked")?;
        join_delayed_server(server)?;
        assert_eq!(result?, (-1, true, 1));
        eprintln!(
            "TLS polling: progressing body intact; stalled body canceled in {:?}",
            started.elapsed()
        );
        Ok(())
    }

    #[test]
    fn slow_error_body_does_not_block_playback_until_the_body_arrives() -> std::io::Result<()> {
        let (url, server) = spawn_delayed_body_server(403, Duration::from_millis(500))?;
        let mut stream = test_stream(&url, 2)?;
        let started = Instant::now();
        let result = open_reader_at(&mut stream, 0, "seek", 1);
        let elapsed = started.elapsed();
        assert_eq!(
            result,
            Err(StreamError::Http {
                status: 403,
                message: String::new()
            })
        );
        assert!(
            elapsed < Duration::from_millis(400),
            "error diagnostics took {elapsed:?}"
        );
        assert!(stream.error_body_deadline.0.lock().is_none());
        join_delayed_server(server)
    }

    #[test]
    fn successful_media_body_is_not_subject_to_the_diagnostic_deadline() -> std::io::Result<()> {
        let (url, server) = spawn_delayed_body_server(206, Duration::from_millis(250))?;
        let mut stream = test_stream(&url, 2)?;
        open_reader_at(&mut stream, 0, "seek", 1).map_err(std::io::Error::other)?;
        let mut body = [0_u8; 2];
        assert_eq!(read_once(&mut stream, &mut body), ReadAttempt::Bytes(2));
        assert_eq!(body, *b"{}");
        join_delayed_server(server)
    }

    #[test]
    fn uri_carries_explicit_proxy_mode_and_preserves_empty_content_length() {
        assert_eq!(
            build_uri_with_session(
                "https://cdn.example/video.mkv",
                "MediaStationGoWindows/test",
                None,
                StreamProxyMode::System,
                41,
            ),
            "mediastation://https://cdn.example/video.mkv|MediaStationGoWindows/test||system|41"
        );
        assert_eq!(
            build_uri_with_session(
                "https://cdn.example/video.mkv",
                "MediaStationGoWindows/test",
                Some(42),
                StreamProxyMode::Direct,
                42,
            ),
            "mediastation://https://cdn.example/video.mkv|MediaStationGoWindows/test|42|direct|42"
        );
    }

    #[test]
    fn cdn_json_error_keeps_reason_and_request_id_without_other_fields() {
        let error = cdn_error(
            br#"{"message":"no cookie value","request_id":"69FB0C751FABFC3936BA5A7E","status":403,"cookie":"SEID=private"}"#,
            "https://cdn.example/video?token=private",
        );
        assert_eq!(error.format, "json");
        assert_eq!(error.message, "no cookie value");
        assert_eq!(error.request_id, "69FB0C751FABFC3936BA5A7E");
        assert!(!format!("{error:?}").contains("SEID"));
    }

    #[test]
    fn cdn_xml_error_keeps_oss_fields_and_redacts_signed_values() {
        let error = cdn_error(
            b"<?xml version=\"1.0\"?><Error><Code>AccessDenied</Code><Message>Denied https://cdn.example/video?k=private signature private Cookie=secret</Message><RequestId>request-123</RequestId><EC>0003-00000001</EC><StringToSign>credential</StringToSign></Error>",
            "https://cdn.example/video?k=private",
        );
        assert_eq!(error.format, "xml");
        assert_eq!(error.code, "AccessDenied");
        assert_eq!(error.request_id, "request-123");
        assert_eq!(error.ec, "0003-00000001");
        let diagnostic = format!("{error:?}");
        for secret in ["https://", "private", "secret", "credential"] {
            assert!(!diagnostic.contains(secret));
        }
    }

    #[test]
    fn cdn_error_does_not_dump_unknown_or_malformed_bodies() {
        for body in [
            b"<html>Cookie=secret</html>".as_slice(),
            b"{\"message\":\"secret",
        ] {
            let error = cdn_error(body, "https://cdn.example/video");
            assert_eq!(error.format, "unrecognized");
            assert!(error.message.is_empty());
        }
    }

    #[test]
    fn diagnostic_body_read_is_limited_and_marks_partial_failure() {
        let mut reader = std::io::Cursor::new(vec![b'x'; ERROR_BODY_LIMIT as usize + 100]);
        let (body, state) = read_error_body(&mut reader, Instant::now() + ERROR_BODY_TIMEOUT);
        assert_eq!(body.len() as u64, ERROR_BODY_LIMIT);
        assert_eq!(reader.position(), ERROR_BODY_LIMIT);
        assert_eq!(state, "limit_reached");

        struct ResetReader;
        impl Read for ResetReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::ConnectionReset.into())
            }
        }
        let (body, state) = read_error_body(
            std::io::Cursor::new(b"{}").chain(ResetReader),
            Instant::now() + ERROR_BODY_TIMEOUT,
        );
        assert_eq!(body, b"{}");
        assert_eq!(state, "connection_reset");
    }

    #[test]
    fn http_403_drops_the_previous_reader() -> std::io::Result<()> {
        const ERROR: &[u8] =
            br#"{"message":"no cookie value","request_id":"request-123","status":403}"#;
        let (url, server) = spawn_range_server(vec![
            response(206, "Partial Content", Some("bytes 0-5/6"), 6, b"abcdef"),
            response(403, "Forbidden", None, ERROR.len(), ERROR),
        ])?;
        let mut stream = test_stream(&url, 6)?;
        open_reader_at(&mut stream, 0, "seek", 1).map_err(std::io::Error::other)?;
        let mut first = [0_u8; 2];
        assert_eq!(read_once(&mut stream, &mut first), ReadAttempt::Bytes(2));
        assert_eq!(
            open_reader_at(&mut stream, 4, "seek", 1),
            Err(StreamError::Http {
                status: 403,
                message: "no cookie value".to_string()
            })
        );
        assert_eq!(stream.position, 2);
        assert!(stream.reader.is_none());
        assert_eq!(join_ranges(server)?, vec!["bytes=0-", "bytes=4-"]);
        Ok(())
    }

    #[test]
    fn read_recovers_after_truncated_body_with_same_url() -> std::io::Result<()> {
        let (url, server) = spawn_range_server(vec![
            response(206, "Partial Content", Some("bytes 0-5/6"), 6, b"abc"),
            response(206, "Partial Content", Some("bytes 3-5/6"), 3, b"def"),
        ])?;
        let mut stream = test_stream(&url, 6)?;
        open_reader_at(&mut stream, 0, "seek", 1).map_err(std::io::Error::other)?;

        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0_u8; 2];
            let count = read_with_recovery(&mut stream, &mut buffer);
            if count < 0 {
                return Err(std::io::Error::other("stream recovery failed"));
            }
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count as usize]);
        }

        assert_eq!(bytes, b"abcdef");
        assert_eq!(
            join_ranges(server)?,
            vec!["bytes=0-".to_string(), "bytes=3-".to_string()]
        );
        Ok(())
    }

    #[test]
    fn read_recovers_after_premature_eof_with_same_url() -> std::io::Result<()> {
        let (url, server) = spawn_range_server(vec![
            response(206, "Partial Content", Some("bytes 0-2/6"), 3, b"abc"),
            response(206, "Partial Content", Some("bytes 3-5/6"), 3, b"def"),
        ])?;
        let mut stream = test_stream(&url, 6)?;
        open_reader_at(&mut stream, 0, "seek", 1).map_err(std::io::Error::other)?;

        let mut bytes = Vec::new();
        loop {
            let mut buffer = [0_u8; 2];
            let count = read_with_recovery(&mut stream, &mut buffer);
            if count < 0 {
                return Err(std::io::Error::other("stream recovery failed"));
            }
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count as usize]);
        }

        assert_eq!(bytes, b"abcdef");
        assert_eq!(
            join_ranges(server)?,
            vec!["bytes=0-".to_string(), "bytes=3-".to_string()]
        );
        Ok(())
    }

    #[test]
    fn read_recovery_stops_after_bounded_range_failures() -> std::io::Result<()> {
        let (url, server) = spawn_range_server(vec![
            response(206, "Partial Content", Some("bytes 0-5/6"), 6, b"abc"),
            response(503, "Service Unavailable", None, 0, b""),
            response(503, "Service Unavailable", None, 0, b""),
        ])?;
        let mut stream = test_stream(&url, 6)?;
        open_reader_at(&mut stream, 0, "seek", 1).map_err(std::io::Error::other)?;

        let mut first = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut first), 2);
        let mut second = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut second), 1);
        let mut third = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut third), -1);
        assert_eq!(stream.position, 3);
        assert!(stream.reader.is_none());
        assert_eq!(
            stream.failure.as_ref().and_then(StreamFailure::http_status),
            Some(503)
        );
        let requests = stream.request_seq;
        assert_eq!(read_with_recovery(&mut stream, &mut third), -1);
        assert_eq!(stream.request_seq, requests);
        assert_eq!(
            join_ranges(server)?,
            vec![
                "bytes=0-".to_string(),
                "bytes=3-".to_string(),
                "bytes=3-".to_string()
            ]
        );
        Ok(())
    }

    #[test]
    fn successive_seeks_only_request_the_final_offset_on_read() -> std::io::Result<()> {
        let (url, server) = spawn_range_server(vec![response(
            206,
            "Partial Content",
            Some("bytes 4-5/6"),
            2,
            b"ef",
        )])?;
        let mut stream = test_stream(&url, 6)?;
        assert_eq!(seek_to(&mut stream, 4), 4);
        assert_eq!(stream.request_seq, 0, "seek must not issue speculative I/O");
        assert_eq!(seek_to(&mut stream, 2), 2);
        assert_eq!(seek_to(&mut stream, 4), 4);
        let mut bytes = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 2);
        assert_eq!(&bytes, b"ef");
        assert_eq!(join_ranges(server)?, vec!["bytes=4-"]);
        Ok(())
    }

    #[test]
    fn startup_range_sequence_avoids_an_unread_request() -> std::io::Result<()> {
        fn run(deferred: bool) -> std::io::Result<(Vec<String>, Duration)> {
            let mut responses = vec![
                response(206, "Partial Content", Some("bytes 0-1/128"), 2, b"ab"),
                response(206, "Partial Content", Some("bytes 126-127/128"), 2, b"yz"),
                response(206, "Partial Content", Some("bytes 8-9/128"), 2, b"hi"),
            ];
            if !deferred {
                responses.push(response(
                    206,
                    "Partial Content",
                    Some("bytes 80-81/128"),
                    2,
                    b"xx",
                ));
            }
            responses.push(response(
                206,
                "Partial Content",
                Some("bytes 72-73/128"),
                2,
                b"ok",
            ));
            let (url, server) =
                spawn_range_server_with_delay(responses, Duration::from_millis(80))?;
            let mut stream = test_stream(&url, 128)?;
            let started = Instant::now();
            for (offset, expected) in [
                (0, Some(b"ab")),
                (126, Some(b"yz")),
                (8, Some(b"hi")),
                (80, None),
                (72, Some(b"ok")),
            ] {
                if deferred {
                    assert_eq!(seek_to(&mut stream, offset), offset);
                } else {
                    // Reference the previous eager-open behavior on the same transport.
                    open_reader_at(&mut stream, offset as u64, "seek", 1)
                        .map_err(std::io::Error::other)?;
                }
                if let Some(expected) = expected {
                    let mut bytes = [0_u8; 2];
                    assert_eq!(read_with_recovery(&mut stream, &mut bytes), 2);
                    assert_eq!(&bytes, expected);
                }
            }
            let elapsed = started.elapsed();
            Ok((join_ranges(server)?, elapsed))
        }
        let (eager, eager_elapsed) = run(false)?;
        let (deferred, deferred_elapsed) = run(true)?;
        assert_eq!(
            eager,
            vec![
                "bytes=0-",
                "bytes=126-",
                "bytes=8-",
                "bytes=80-",
                "bytes=72-"
            ]
        );
        assert_eq!(
            deferred,
            vec!["bytes=0-", "bytes=126-", "bytes=8-", "bytes=72-"]
        );
        eprintln!(
            "loopback startup Range sequence (80ms/response): eager={eager_elapsed:?} (5 requests), deferred={deferred_elapsed:?} (4 requests)"
        );
        Ok(())
    }

    #[test]
    fn failed_seek_read_is_terminal_and_preserves_http_error_without_more_requests()
    -> std::io::Result<()> {
        const ERROR: &[u8] = br#"{"message":"115 pmt 3-2","status":403}"#;
        let (url, server) = spawn_range_server(vec![
            response(206, "Partial Content", Some("bytes 0-5/6"), 6, b"abcdef"),
            response(403, "Forbidden", None, ERROR.len(), ERROR),
            response(403, "Forbidden", None, ERROR.len(), ERROR),
            response(403, "Forbidden", None, ERROR.len(), ERROR),
        ])?;
        let mut stream = test_stream(&url, 6)?;
        assert_eq!(seek_to(&mut stream, 0), 0);
        let mut bytes = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 2);
        assert_eq!(seek_to(&mut stream, 4), 4);
        assert!(stream.reader.is_none());
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), -1);
        assert_eq!(stream.position, 4);
        assert!(stream.reader.is_none());
        let failure = stream
            .failure
            .clone()
            .ok_or_else(|| std::io::Error::other("missing terminal failure"))?;
        assert_eq!(failure.http_status(), Some(403));
        assert_eq!(failure.phase, "seek");
        assert_eq!(failure.offset, 4);
        assert!(failure.user_message().contains("115 pmt 3-2"));
        for _ in 0..3 {
            assert_eq!(read_with_recovery(&mut stream, &mut bytes), -1);
            assert_eq!(seek_to(&mut stream, 0), MPV_ERROR_GENERIC);
        }
        assert_eq!(stream.failure, Some(failure));
        assert_eq!(stream.request_seq, 4);
        assert_eq!(
            join_ranges(server)?,
            vec!["bytes=0-", "bytes=4-", "bytes=4-", "bytes=4-"]
        );
        Ok(())
    }

    #[test]
    fn transient_seek_failure_recovers_to_requested_offset() -> std::io::Result<()> {
        let (url, server) = spawn_range_server(vec![
            response(206, "Partial Content", Some("bytes 0-5/6"), 6, b"abcdef"),
            response(503, "Service Unavailable", None, 0, b""),
            response(206, "Partial Content", Some("bytes 4-5/6"), 2, b"ef"),
        ])?;
        let mut stream = test_stream(&url, 6)?;
        assert_eq!(seek_to(&mut stream, 0), 0);
        let mut bytes = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 2);
        assert_eq!(&bytes, b"ab");
        assert_eq!(seek_to(&mut stream, 4), 4);
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 2);
        assert_eq!(&bytes, b"ef");
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 0);
        assert!(stream.failure.is_none());
        assert_eq!(
            join_ranges(server)?,
            vec!["bytes=0-", "bytes=4-", "bytes=4-"]
        );
        Ok(())
    }

    #[test]
    fn replayed_link_has_a_distinct_session() {
        let a = build_uri(
            "https://cdn.example/video",
            "test",
            None,
            StreamProxyMode::Direct,
        );
        let b = build_uri(
            "https://cdn.example/video",
            "test",
            None,
            StreamProxyMode::Direct,
        );
        assert!(session_id(&a).is_some());
        assert_ne!(session_id(&a), session_id(&b));
        assert_eq!(session_id("https://cdn.example/video"), None);
        assert_eq!(session_id("mediastation://url|ua||direct|0"), None);
    }

    #[test]
    fn replacement_range_arrives_after_the_previous_tcp_connection_closes() -> std::io::Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let url = format!("http://{}/video", listener.local_addr()?);
        let server = std::thread::spawn(move || -> std::io::Result<bool> {
            let (mut first, _) = listener.accept()?;
            first.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut request = BufReader::new(first.try_clone()?);
            loop {
                let mut line = String::new();
                if request.read_line(&mut line)? == 0 || line == "\r\n" {
                    break;
                }
            }
            write!(
                first,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-1048575/1048576\r\nContent-Length: 1048576\r\n\r\nabcdefgh"
            )?;
            first.flush()?;
            let mut byte = [0_u8; 1];
            let closed = match first.read(&mut byte) {
                Ok(0) => true,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    true
                }
                _ => false,
            };
            // A timeout would expose the original lifecycle bug, but still
            // finish the second response so the test can report the assertion.
            let (mut second, _) = listener.accept()?;
            second.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut request = BufReader::new(second.try_clone()?);
            loop {
                let mut line = String::new();
                if request.read_line(&mut line)? == 0 || line == "\r\n" {
                    break;
                }
            }
            second.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 10-10/1048576\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx")?;
            Ok(closed)
        });
        let mut stream = test_stream(&url, 1048576)?;
        open_reader_at(&mut stream, 0, "seek", 1).map_err(std::io::Error::other)?;
        let mut bytes = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 2);
        assert_eq!(seek_to(&mut stream, 10), 10);
        assert!(stream.reader.is_none());
        assert_eq!(stream.request_seq, 1);
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 1);
        assert_eq!(bytes[0], b'x');
        let closed = server
            .join()
            .map_err(|_| std::io::Error::other("connection lifecycle server panicked"))??;
        assert!(
            closed,
            "previous TCP connection remained open while requesting its replacement"
        );
        Ok(())
    }

    #[test]
    fn seeking_to_current_reader_position_keeps_the_valid_connection() -> std::io::Result<()> {
        let (url, server) = spawn_range_server(vec![response(
            206,
            "Partial Content",
            Some("bytes 0-5/6"),
            6,
            b"abcdef",
        )])?;
        let mut stream = test_stream(&url, 6)?;
        assert_eq!(seek_to(&mut stream, 0), 0);
        let mut bytes = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 2);
        assert_eq!(&bytes, b"ab");
        assert_eq!(seek_to(&mut stream, 2), 2);
        assert!(stream.reader.is_some());
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 2);
        assert_eq!(&bytes, b"cd");
        assert_eq!(stream.request_seq, 1);
        assert_eq!(join_ranges(server)?, vec!["bytes=0-"]);
        Ok(())
    }

    #[test]
    fn retained_http_error_redacts_server_credentials() -> Result<(), Box<dyn std::error::Error>> {
        let body = br#"{"message":"denied test-token test-auth"}"#;
        let (url, server) =
            spawn_range_server(vec![response(403, "Forbidden", None, body.len(), body)])?;
        let mut stream = test_stream(&url, 6)?;
        stream.http_headers = Some(vec![
            ("x-emby-token".to_string(), "test-token".to_string()),
            ("x-emby-authorization".to_string(), "test-auth".to_string()),
        ]);
        let error = open_reader_at(&mut stream, 0, "read", 1).expect_err("HTTP rejection");
        let StreamError::Http { status, message } = error else {
            return Err("expected HTTP error".into());
        };
        assert_eq!(status, 403);
        assert_eq!(message, "denied [redacted] [redacted]");
        assert_eq!(join_ranges(server)?, vec!["bytes=0-"]);
        Ok(())
    }

    #[test]
    fn prepared_http_handoff_requires_its_unique_load_and_is_consumed_once() -> std::io::Result<()>
    {
        let http = PreparedHttpStream {
            stream: test_stream("http://127.0.0.1/media", 6)?,
        };
        let uri = http.uri();
        let mut pending = Some(http);
        assert!(take_http_stream(&mut pending, "mediastation-http://0").is_none());
        assert!(take_http_stream(&mut pending, "mediastation://other").is_none());
        assert!(pending.is_some());
        assert!(take_http_stream(&mut pending, &uri).is_some());
        assert!(take_http_stream(&mut pending, &uri).is_none());
        Ok(())
    }

    #[test]
    fn prepared_response_feeds_native_callbacks_without_reopening()
    -> Result<(), Box<dyn std::error::Error>> {
        for status in [200, 206] {
            let (url, server) = spawn_range_server(vec![response(
                status,
                "OK",
                (status == 206).then_some("bytes 0-5/6"),
                6,
                b"abcdef",
            )])?;
            let preparation = HttpStreamPreparation::new(&url, "test-UA", StreamProxyMode::Direct)?;
            preparation.open(&url, [("x-emby-token", "test-token")].into_iter())?;
            let http = preparation.finish()?;
            assert!(session_id(&http.uri()).is_some());
            assert!(!http.uri().contains("test-token"));
            let mut info: sys::mpv_stream_cb_info = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { install_cookie(http.stream, &mut info) }, 0);
            assert_eq!(unsafe { size_cb(info.cookie) }, 6);
            assert_eq!(info.seek_fn.is_some(), status == 206);
            if status == 206 {
                assert_eq!(unsafe { seek_cb(info.cookie, 0) }, 0);
            }
            let mut bytes = [0_u8; 6];
            assert_eq!(
                unsafe { read_cb(info.cookie, bytes.as_mut_ptr().cast(), 6) },
                6
            );
            assert_eq!(&bytes, b"abcdef");
            assert_eq!(stream_from_cookie(info.cookie).inner.lock().request_seq, 1);
            unsafe {
                close_cb(info.cookie);
            }
            assert_eq!(join_ranges(server)?, vec!["bytes=0-"]);
        }
        Ok(())
    }

    #[test]
    fn prepared_response_rejects_bad_ranges_and_http_errors_without_a_second_request()
    -> Result<(), Box<dyn std::error::Error>> {
        for (status, range) in [
            (206, Some("bytes 1-6/7")),
            (206, Some("bytes 0-9/10")),
            (403, None),
        ] {
            let (url, server) =
                spawn_range_server(vec![response(status, "Test", range, 6, b"abcdef")])?;
            let preparation = HttpStreamPreparation::new(&url, "test-UA", StreamProxyMode::Direct)?;
            let result = preparation.open(&url, std::iter::empty());
            if status == 206 {
                assert!(matches!(result, Err(StreamError::InvalidResponse(_))));
            } else {
                assert_eq!(result?.status_code, 403);
            }
            assert!(preparation.finish().is_err());
            assert_eq!(join_ranges(server)?, vec!["bytes=0-"]);
        }
        Ok(())
    }

    #[test]
    fn initial_head_handoff_requires_the_complete_load_uri_and_is_consumed_once() {
        let uri = build_uri(
            "https://cdn.example/file",
            "UA",
            Some(6),
            StreamProxyMode::Direct,
        );
        let mut pending = Some(InitialHead {
            uri: uri.clone(),
            bytes: Arc::from(*b"abc"),
        });
        for changed in [
            uri.replace("/file", "/other"),
            uri.replace("|UA|", "|other|"),
            uri.replace("|6|", "|7|"),
            uri.replace("|direct|", "|system|"),
            build_uri(
                "https://cdn.example/file",
                "UA",
                Some(6),
                StreamProxyMode::Direct,
            ),
        ] {
            assert!(take_initial_head(&mut pending, &changed).is_empty());
            assert!(pending.is_some());
        }
        assert_eq!(take_initial_head(&mut pending, &uri).as_ref(), b"abc");
        assert!(take_initial_head(&mut pending, &uri).is_empty());
    }

    #[test]
    fn reads_crossing_the_cached_head_continue_at_the_exact_network_offset() -> std::io::Result<()>
    {
        let (url, server) = spawn_range_server(vec![response(
            206,
            "Partial Content",
            Some("bytes 3-5/6"),
            3,
            b"def",
        )])?;
        let mut stream = test_stream(&url, 6)?;
        stream.initial_head = Arc::from(*b"abc");
        assert_eq!(seek_to(&mut stream, 0), 0);
        let mut bytes = [0_u8; 4];
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 3);
        assert_eq!(&bytes[..3], b"abc");
        assert_eq!(stream.request_seq, 0);
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 3);
        assert_eq!(&bytes[..3], b"def");
        assert_eq!(seek_to(&mut stream, 1), 1);
        assert!(stream.reader.is_none());
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 2);
        assert_eq!(&bytes[..2], b"bc");
        assert_eq!(stream.cached_bytes_read, 5);
        assert_eq!(join_ranges(server)?, vec!["bytes=3-"]);
        Ok(())
    }

    #[test]
    fn an_entirely_cached_small_file_needs_no_network_even_at_eof() -> std::io::Result<()> {
        let mut stream = test_stream("http://127.0.0.1:1/file", 3)?;
        stream.initial_head = Arc::from(*b"abc");
        assert_eq!(seek_to(&mut stream, 0), 0);
        let mut bytes = [0_u8; 8];
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 3);
        assert_eq!(&bytes[..3], b"abc");
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 0);
        assert_eq!(seek_to(&mut stream, 3), 3);
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 0);
        assert_eq!(stream.request_seq, 0);
        Ok(())
    }

    #[test]
    fn a_failed_network_read_cannot_be_hidden_by_the_cached_head() -> std::io::Result<()> {
        let (url, server) = spawn_range_server(vec![
            response(403, "Forbidden", None, 0, b""),
            response(403, "Forbidden", None, 0, b""),
            response(403, "Forbidden", None, 0, b""),
        ])?;
        let mut stream = test_stream(&url, 6)?;
        stream.initial_head = Arc::from(*b"abc");
        assert_eq!(seek_to(&mut stream, 0), 0);
        let mut bytes = [0_u8; 8];
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), 3);
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), -1);
        assert_eq!(
            stream.failure.as_ref().and_then(StreamFailure::http_status),
            Some(403)
        );
        assert_eq!(seek_to(&mut stream, 0), MPV_ERROR_GENERIC);
        assert_eq!(read_with_recovery(&mut stream, &mut bytes), -1);
        assert_eq!(
            join_ranges(server)?,
            vec!["bytes=3-", "bytes=3-", "bytes=3-"]
        );
        Ok(())
    }

    #[test]
    fn initial_head_reuse_removes_both_head_requests_in_the_recorded_startup_sequence()
    -> std::io::Result<()> {
        const TOTAL: u64 = 1_317_086_319;
        const HEAD: &[u8] = &[b'h'; INITIAL_HEAD_LIMIT];
        const TAIL: &[u8] = &[b't'; 383_718];
        fn read_count(stream: &mut Stream, count: usize, byte: u8) {
            let mut remaining = count;
            let mut out = vec![0; count];
            while remaining > 0 {
                let n = read_with_recovery(stream, &mut out[..remaining]);
                assert!(n > 0, "unexpected EOF/error at {}", stream.position);
                assert!(out[..n as usize].iter().all(|value| *value == byte));
                remaining -= n as usize;
            }
        }
        fn run(cached: bool) -> std::io::Result<(Vec<String>, Duration)> {
            let mut responses = Vec::new();
            if !cached {
                responses.push(response(
                    206,
                    "Partial Content",
                    Some("bytes 0-65535/1317086319"),
                    HEAD.len(),
                    HEAD,
                ));
            }
            responses.push(response(
                206,
                "Partial Content",
                Some("bytes 1316702601-1317086318/1317086319"),
                TAIL.len(),
                TAIL,
            ));
            if !cached {
                responses.push(response(
                    206,
                    "Partial Content",
                    Some("bytes 9177-53791/1317086319"),
                    44615,
                    &HEAD[9177..53792],
                ));
            }
            responses.push(response(
                206,
                "Partial Content",
                Some("bytes 1035457134-1035457137/1317086319"),
                4,
                b"ssss",
            ));
            let (url, server) =
                spawn_range_server_with_delay(responses, Duration::from_millis(80))?;
            let mut stream = test_stream(&url, TOTAL)?;
            if cached {
                stream.initial_head = Arc::from(HEAD);
            }
            let started = Instant::now();
            for (offset, count, byte) in [
                (0, 11850, b'h'),
                (1316702601, 383718, b't'),
                (9177, 44615, b'h'),
                (1035457134, 4, b's'),
            ] {
                assert_eq!(seek_to(&mut stream, offset), offset);
                read_count(&mut stream, count, byte);
            }
            let elapsed = started.elapsed();
            assert!(stream.failure.is_none());
            if cached {
                assert_eq!(stream.cached_bytes_read, 56465);
            }
            Ok((join_ranges(server)?, elapsed))
        }
        let (before, before_elapsed) = run(false)?;
        let (after, after_elapsed) = run(true)?;
        assert_eq!(
            before,
            vec![
                "bytes=0-",
                "bytes=1316702601-",
                "bytes=9177-",
                "bytes=1035457134-"
            ]
        );
        assert_eq!(after, vec!["bytes=1316702601-", "bytes=1035457134-"]);
        eprintln!(
            "recorded startup sequence, loopback 80ms/response, prefix already obtained: before={before_elapsed:?} (4 requests), after={after_elapsed:?} (2 requests)"
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires installed libmpv and MPV_STREAM_TEST_FIXTURE; loopback HTTP only"]
    fn native_http_response_handoff_preserves_start_and_resume_with_one_request()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{Command, Event, Handle, PropertyValue};
        let bytes = std::fs::read(std::env::var("MPV_STREAM_TEST_FIXTURE")?)?;
        let full_range = format!("bytes 0-{}/{}", bytes.len() - 1, bytes.len());
        for (start, cache) in [(0.0, "auto"), (0.4, "auto"), (0.4, "no"), (0.4, "yes")] {
            for handoff in [false, true] {
                let mut responses = Vec::new();
                if !handoff {
                    responses.push(response(
                        206,
                        "Partial Content",
                        Some(&format!("bytes 0-0/{}", bytes.len())),
                        1,
                        &bytes[..1],
                    ));
                }
                responses.push(response(
                    206,
                    "Partial Content",
                    Some(&full_range),
                    bytes.len(),
                    &bytes,
                ));
                let (url, server) =
                    spawn_range_server_with_delay(responses, Duration::from_millis(80))?;
                let handle = Handle::create()?;
                for (name, value) in [
                    ("config", "no"),
                    ("vo", "null"),
                    ("ao", "null"),
                    ("terminal", "no"),
                    ("hwdec", "no"),
                    ("cache", cache),
                    ("demuxer-readahead-secs", "0"),
                ] {
                    handle.set_option_string(name, value)?;
                }
                unsafe {
                    register_protocol(handle.raw());
                }
                handle.initialize()?;
                handle.observe_property_node(1, "video-frame-info")?;
                let started = Instant::now();
                let path = if handoff {
                    let preparation =
                        HttpStreamPreparation::new(&url, "test-UA", StreamProxyMode::Direct)?;
                    preparation.open(&url, std::iter::empty())?;
                    let http = preparation.finish()?;
                    let uri = http.uri();
                    prepare_load_stream(&uri, Arc::from([]), Some(http))?;
                    CString::new(uri)?
                } else {
                    let agent: ureq::Agent =
                        ureq::Agent::config_builder().proxy(None).build().into();
                    let mut probe = agent.get(&url).header("Range", "bytes=0-0").call()?;
                    probe.body_mut().as_reader().read_exact(&mut [0_u8; 1])?;
                    drop(probe);
                    CString::new(url)?
                };
                let mut options = vec![(
                    "start".to_string(),
                    if start == 0.0 {
                        "none".to_string()
                    } else {
                        start.to_string()
                    },
                )];
                if handoff
                    && let Some(option) =
                        unsafe { crate::api::retained_http_cache_option(handle.raw()) }?
                {
                    options.push(option);
                }
                let mut load = crate::command::LoadFileCommand::new(&path, &options)?;
                crate::error::check(unsafe {
                    sys::mpv_command_node_async(handle.raw(), 0, load.as_mut_ptr())
                })?;
                let (mut loaded, mut restarted, mut frame) = (false, false, false);
                while started.elapsed() < Duration::from_secs(5) {
                    match handle.wait_event(0.01) {
                        Event::FileLoaded => loaded = true,
                        Event::PlaybackRestart => restarted = true,
                        Event::PropertyChange { id: 1, value, .. }
                            if value != PropertyValue::None =>
                        {
                            frame = true
                        }
                        Event::EndFile(reason) => {
                            return Err(format!("HTTP fixture failed: {reason:?}").into());
                        }
                        _ => {}
                    }
                    if loaded && restarted && frame {
                        break;
                    }
                }
                assert!(
                    loaded && restarted && frame,
                    "start={start} handoff={handoff}"
                );
                let elapsed = started.elapsed();
                assert_eq!(
                    handle.get_property_string("options/cache")?,
                    if handoff && cache == "auto" {
                        "yes"
                    } else {
                        cache
                    }
                );
                let position = handle.get_property::<f64>("time-pos")?;
                assert!((position - start).abs() < 0.2, "resume position {position}");
                handle.command(&Command::new(["stop"])?)?;
                let ranges = join_ranges(server)?;
                assert_eq!(
                    ranges,
                    if handoff {
                        vec!["bytes=0-"]
                    } else {
                        vec!["bytes=0-0", "bytes=0-"]
                    }
                );
                eprintln!(
                    "loopback native HTTP: handoff={handoff} cache={cache} start={start} position={position} first_frame_ms={} requests={}",
                    elapsed.as_millis(),
                    ranges.len()
                );
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires installed libmpv and a local MP4 fixture between 64 and 128 KiB"]
    fn native_stream_consumes_the_handed_off_prefix_and_resumes_the_local_video()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{Command, Event, Handle, PropertyValue};
        let bytes = std::fs::read(std::env::var("MPV_STREAM_TEST_FIXTURE")?)?;
        assert!((INITIAL_HEAD_LIMIT + 1..=2 * INITIAL_HEAD_LIMIT).contains(&bytes.len()));
        let remaining_range = format!(
            "bytes {}-{}/{}",
            INITIAL_HEAD_LIMIT,
            bytes.len() - 1,
            bytes.len()
        );
        let (url, server) = spawn_range_server(vec![response(
            206,
            "Partial Content",
            Some(&remaining_range),
            bytes.len() - INITIAL_HEAD_LIMIT,
            &bytes[INITIAL_HEAD_LIMIT..],
        )])?;
        let uri = build_uri(
            &url,
            "MediaStationGoWindows/test",
            Some(bytes.len() as u64),
            StreamProxyMode::Direct,
        );
        prepare_initial_head(&uri, Arc::from(&bytes[..INITIAL_HEAD_LIMIT]))?;
        let handle = Handle::create()?;
        for (name, value) in [
            ("config", "no"),
            ("vo", "null"),
            ("ao", "null"),
            ("terminal", "no"),
            ("hwdec", "no"),
            ("cache", "no"),
            ("demuxer-readahead-secs", "0"),
        ] {
            handle.set_option_string(name, value)?;
        }
        unsafe {
            register_protocol(handle.raw());
        }
        handle.initialize()?;
        handle.observe_property_node(1, "video-frame-info")?;
        let path = CString::new(uri)?;
        let mut load = crate::command::LoadFileCommand::new(
            &path,
            &[("start".to_string(), "0.4".to_string())],
        )?;
        crate::error::check(unsafe {
            sys::mpv_command_node_async(handle.raw(), 0, load.as_mut_ptr())
        })?;
        let started = Instant::now();
        let (mut loaded, mut restarted, mut frame) = (false, false, false);
        while started.elapsed() < Duration::from_secs(5) {
            match handle.wait_event(0.01) {
                Event::FileLoaded => loaded = true,
                Event::PlaybackRestart => restarted = true,
                Event::PropertyChange { id: 1, value, .. } if value != PropertyValue::None => {
                    frame = true
                }
                Event::EndFile(reason) => {
                    return Err(format!("local cached playback failed: {reason:?}").into());
                }
                _ => {}
            }
            if loaded && restarted && frame {
                break;
            }
        }
        assert!(loaded && restarted && frame);
        let position = handle.get_property::<f64>("time-pos")?;
        assert!((position - 0.4).abs() < 0.2, "resume position {position}");
        assert!(
            PENDING_INITIAL_HEAD.lock().is_none(),
            "native open did not consume its header"
        );
        handle.command(&Command::new(["stop"])?)?;
        assert_eq!(
            join_ranges(server)?,
            vec![format!("bytes={INITIAL_HEAD_LIMIT}-")]
        );
        eprintln!(
            "native cached MP4 reached a video frame at {position}s with one Range after the 64KiB prefix"
        );
        Ok(())
    }
}
