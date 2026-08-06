//! Custom mpv stream protocol backed by ureq.
//!
//! The 115 cloud CDN behind MediaStationGo returns HTTP 403 to ffmpeg's
//! OpenSSL TLS fingerprint, while browser-class TLS stacks (curl, ureq,
//! OkHttp) are allowed. Registering a `mediastation://` protocol here lets
//! playback read the media through ureq instead of ffmpeg's HTTP, so CDN
//! rate limits that target the ffmpeg TLS fingerprint are avoided.

use parking_lot::Mutex;
use std::ffi::{CStr, CString};
use std::io::Read;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

use crate::sys;

const MPV_ERROR_UNSUPPORTED: i64 = -18;
const MPV_ERROR_LOADING_FAILED: c_int = -13;

const PROTOCOL: &[u8] = b"mediastation";
const RANGE_OPEN_ATTEMPTS: usize = 3;
const READ_RECOVERY_ATTEMPTS: usize = 2;
const RETRY_BASE_DELAY_MS: u64 = 150;

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

fn stream_agent(proxy_mode: StreamProxyMode) -> Option<ureq::Agent> {
    let proxy = match proxy_mode {
        StreamProxyMode::Direct => None,
        StreamProxyMode::System => Some(ureq::Proxy::try_from_env()?),
    };
    Some(ureq::Agent::new_with_config(
        ureq::Agent::config_builder().proxy(proxy).build(),
    ))
}

/// A single open media stream. Guarded by a mutex because mpv may call the
/// cancel callback from a different thread than read/seek.
struct Stream {
    agent: ureq::Agent,
    url: String,
    user_agent: String,
    content_length: Option<u64>,
    position: u64,
    reader: Option<ureq::BodyReader<'static>>,
}

struct StreamCookie {
    inner: Mutex<Stream>,
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

fn open_reader_at(stream: &mut Stream, offset: u64) -> Result<(), String> {
    let range = format!("bytes={offset}-");
    let response = stream
        .agent
        .get(stream.url.as_str())
        .header("User-Agent", stream.user_agent.as_str())
        .header("Range", range.as_str())
        .call()
        .map_err(|error| ureq_error_kind(&error))?;
    let status = response.status().as_u16();

    if status == 206 {
        let content_range = response
            .headers()
            .get("Content-Range")
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| "content_range_missing".to_string())?;
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

    stream.reader = Some(response.into_body().into_reader());
    stream.position = offset;
    Ok(())
}

fn read_once(stream: &mut Stream, out: &mut [u8]) -> ReadAttempt {
    if out.is_empty() {
        return ReadAttempt::Eof;
    }
    let Some(reader) = stream.reader.as_mut() else {
        return ReadAttempt::Recoverable("reader_missing");
    };
    match reader.read(out) {
        Ok(0)
            if stream
                .content_length
                .is_some_and(|length| stream.position < length) =>
        {
            ReadAttempt::Recoverable("premature_eof")
        }
        Ok(0) => ReadAttempt::Eof,
        Ok(n) => {
            stream.position += n as u64;
            ReadAttempt::Bytes(n)
        }
        Err(error) => ReadAttempt::Recoverable(io_error_kind(&error)),
    }
}

fn read_with_recovery(stream: &mut Stream, out: &mut [u8]) -> i64 {
    let mut cause = match read_once(stream, out) {
        ReadAttempt::Bytes(n) => return n as i64,
        ReadAttempt::Eof => return 0,
        ReadAttempt::Recoverable(cause) => cause.to_string(),
    };
    let offset = stream.position;

    for attempt in 1..=READ_RECOVERY_ATTEMPTS {
        tracing::warn!(
            target: "mpv",
            "mediastation stream read interrupted; retrying range: offset={} attempt={}/{} cause={}",
            offset,
            attempt,
            READ_RECOVERY_ATTEMPTS,
            cause,
        );
        std::thread::sleep(std::time::Duration::from_millis(
            RETRY_BASE_DELAY_MS * attempt as u64,
        ));
        match open_reader_at(stream, offset) {
            Ok(()) => match read_once(stream, out) {
                ReadAttempt::Bytes(n) => {
                    tracing::info!(
                        target: "mpv",
                        "mediastation stream read recovered: offset={} attempt={}",
                        offset,
                        attempt,
                    );
                    return n as i64;
                }
                ReadAttempt::Eof => return 0,
                ReadAttempt::Recoverable(next_cause) => cause = next_cause.to_string(),
            },
            Err(error) => cause = format!("range_open_{error}"),
        }
    }

    tracing::warn!(
        target: "mpv",
        "mediastation stream read failed after bounded recovery: offset={} attempts={} cause={}",
        offset,
        READ_RECOVERY_ATTEMPTS,
        cause,
    );
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
    if offset < 0 {
        return MPV_ERROR_UNSUPPORTED;
    }
    let mut last_error = None;
    for attempt in 0..RANGE_OPEN_ATTEMPTS {
        match open_reader_at(&mut inner, offset as u64) {
            Ok(()) => return offset,
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
    tracing::warn!(
        target: "mpv",
        "mediastation stream seek failed after retries: offset={} attempts={} cause={}",
        offset,
        RANGE_OPEN_ATTEMPTS,
        last_error.unwrap_or_else(|| "unknown".to_string()),
    );
    MPV_ERROR_UNSUPPORTED
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
    // Reclaim the leaked Box.
    drop(unsafe { Box::from_raw(cookie as *mut StreamCookie) });
}

unsafe extern "C" fn cancel_cb(_cookie: *mut c_void) {
    // Interrupt in-flight reads. We don't drive a background worker, so
    // nothing to cancel; the next read/seek returns promptly.
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
    // URI format: mediastation://<url>|<user-agent>|<content-length>|<proxy-mode>
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
    let Some(agent) = stream_agent(proxy_mode) else {
        return MPV_ERROR_LOADING_FAILED;
    };
    let cookie = StreamCookie {
        inner: Mutex::new(Stream {
            agent,
            url: url.to_string(),
            user_agent: user_agent.to_string(),
            content_length,
            position: 0,
            reader: None,
        }),
    };
    let cookie_ptr = Box::into_raw(Box::new(cookie)) as *mut c_void;
    let info = unsafe { &mut *info };
    info.cookie = cookie_ptr;
    info.read_fn = Some(read_cb);
    info.seek_fn = Some(seek_cb);
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
pub fn register_protocol(handle: *mut sys::mpv_handle) {
    if handle.is_null() {
        return;
    }
    let protocol = CString::new(PROTOCOL).unwrap_or_default();
    let status = unsafe {
        sys::mpv_stream_cb_add_ro(handle, protocol.as_ptr(), ptr::null_mut(), Some(open_cb))
    };
    if status != 0 {
        tracing::warn!(target: "mpv", "mediastation stream protocol registration failed: {status}");
    } else {
        tracing::info!(target: "mpv", "mediastation stream protocol registered");
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
    let content_length = content_length
        .map(|value| value.to_string())
        .unwrap_or_default();
    format!(
        "mediastation://{url}|{user_agent}|{content_length}|{}",
        proxy_mode.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestResponse {
        status: u16,
        reason: &'static str,
        content_range: Option<&'static str>,
        declared_length: usize,
        body: &'static [u8],
    }

    fn response(
        status: u16,
        reason: &'static str,
        content_range: Option<&'static str>,
        declared_length: usize,
        body: &'static [u8],
    ) -> TestResponse {
        TestResponse {
            status,
            reason,
            content_range,
            declared_length,
            body,
        }
    }

    fn spawn_range_server(
        responses: Vec<TestResponse>,
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
                let content_range = response
                    .content_range
                    .map_or(String::new(), |value| format!("Content-Range: {value}\r\n"));
                let headers = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n",
                    response.status, response.reason, response.declared_length, content_range,
                );
                stream.write_all(headers.as_bytes())?;
                stream.write_all(response.body)?;
                stream.flush()?;
                let _ = stream.shutdown(Shutdown::Both);
            }
            Ok(ranges)
        });
        Ok((format!("http://{address}/video"), handle))
    }

    fn test_stream(url: &str, content_length: u64) -> std::io::Result<Stream> {
        let Some(agent) = stream_agent(StreamProxyMode::Direct) else {
            return Err(std::io::Error::other("direct stream agent unavailable"));
        };
        Ok(Stream {
            agent,
            url: url.to_string(),
            user_agent: "MediaStationGoWindows/test".to_string(),
            content_length: Some(content_length),
            position: 0,
            reader: None,
        })
    }

    fn join_ranges(
        handle: std::thread::JoinHandle<std::io::Result<Vec<String>>>,
    ) -> std::io::Result<Vec<String>> {
        match handle.join() {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other("range server thread panicked")),
        }
    }

    #[test]
    fn uri_carries_explicit_proxy_mode_and_preserves_empty_content_length() {
        assert_eq!(
            build_uri(
                "https://cdn.example/video.mkv",
                "MediaStationGoWindows/test",
                None,
                StreamProxyMode::System,
            ),
            "mediastation://https://cdn.example/video.mkv|MediaStationGoWindows/test||system"
        );
        assert_eq!(
            build_uri(
                "https://cdn.example/video.mkv",
                "MediaStationGoWindows/test",
                Some(42),
                StreamProxyMode::Direct,
            ),
            "mediastation://https://cdn.example/video.mkv|MediaStationGoWindows/test|42|direct"
        );
    }

    #[test]
    fn read_recovers_after_truncated_body_with_same_url() -> std::io::Result<()> {
        let (url, server) = spawn_range_server(vec![
            response(206, "Partial Content", Some("bytes 0-5/6"), 6, b"abc"),
            response(206, "Partial Content", Some("bytes 3-5/6"), 3, b"def"),
        ])?;
        let mut stream = test_stream(&url, 6)?;
        open_reader_at(&mut stream, 0).map_err(std::io::Error::other)?;

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
        open_reader_at(&mut stream, 0).map_err(std::io::Error::other)?;

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
        open_reader_at(&mut stream, 0).map_err(std::io::Error::other)?;

        let mut first = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut first), 2);
        let mut second = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut second), 1);
        let mut third = [0_u8; 2];
        assert_eq!(read_with_recovery(&mut stream, &mut third), -1);
        assert_eq!(stream.position, 3);
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
}
