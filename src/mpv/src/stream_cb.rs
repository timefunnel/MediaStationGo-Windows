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

unsafe extern "C" fn read_cb(cookie: *mut c_void, buf: *mut c_char, nbytes: u64) -> i64 {
    let stream = stream_from_cookie(cookie);
    let mut inner = stream.inner.lock();
    let Some(reader) = inner.reader.as_mut() else {
        return -1;
    };
    let out = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, nbytes as usize) };
    match reader.read(out) {
        Ok(0) => 0,
        Ok(n) => {
            inner.position += n as u64;
            n as i64
        }
        Err(_) => -1,
    }
}

unsafe extern "C" fn seek_cb(cookie: *mut c_void, offset: i64) -> i64 {
    let stream = stream_from_cookie(cookie);
    let mut inner = stream.inner.lock();
    if offset < 0 {
        return MPV_ERROR_UNSUPPORTED;
    }
    // Reopen a Range request at the target offset (ureq has no way to seek
    // an in-flight body, and the CDN requires a fresh Range per position).
    // The CDN intermittently 403s a range request; retry a couple of times
    // with a short pause so transient rate limits don't fail the seek.
    let range = format!("bytes={offset}-");
    let mut last_error = None;
    for attempt in 0..3 {
        let result = inner
            .agent
            .get(inner.url.as_str())
            .header("User-Agent", inner.user_agent.as_str())
            .header("Range", range.as_str())
            .call();
        match result {
            Ok(resp) => {
                inner.reader = Some(resp.into_body().into_reader());
                inner.position = offset as u64;
                return offset;
            }
            Err(e) => {
                last_error = Some(e);
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(150 * (attempt + 1)));
                }
            }
        }
    }
    tracing::warn!(target: "mpv", "mediastation stream seek failed after retries: {}", last_error.map_or_else(String::new, |e| e.to_string()));
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
}
