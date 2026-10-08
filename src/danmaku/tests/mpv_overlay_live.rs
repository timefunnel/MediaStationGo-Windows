#![cfg(feature = "mpv")]
//! Live verification that danmaku reaches mpv's video output *and* coexists with
//! the normal subtitle track.
//!
//! `osd-overlay` (`format=ass-events`) is drawn by the video output, not by the
//! subtitle renderer, so unit tests can only prove that mpv *accepted* the
//! command. This test proves pixels: the same clip is rendered through the
//! player's own video output in four configurations and the frames are compared
//! region by region.
//!
//! Facts asserted:
//!
//! * `screenshot-raw window` reads the composited image straight out of the VO.
//!   In `vo_gpu_next.c` that mode is `subs = osd = true`, i.e. `osd_flags = 0`,
//!   so libass-drawn *external* OSD overlays and subtitles are composited exactly
//!   like they are in the player's window.
//! * the danmaku changes rows in the upper half of the frame and the subtitle
//!   changes rows in the lower half;
//! * each layer's pixels are *identical* whether or not the other layer is
//!   present — the danmaku neither displaces nor covers the subtitle;
//! * `osd-overlay ... compute_bounds=yes` reports the rectangle libass
//!   rasterized, which must land inside the rows that actually changed;
//! * clearing the overlay restores the frame byte for byte.
//!
//! Ignored by default: it needs libmpv and a desktop session (mpv opens its own
//! window for a few seconds). The clip is generated on the spot — a solid-color
//! Y4M frame sequence with a real presentation clock — so no fixture is shipped; set
//! `DANMAKU_LIVE_VIDEO` to run against a real clip instead.
//!
//! ```text
//! cargo test -p jfn-danmaku --features mpv --test mpv_overlay_live -- --ignored --nocapture
//! ```

use jfn_danmaku::{
    DanmakuComment, DanmakuControls, DanmakuDriver, DanmakuSession, DanmakuStyle, MODE_SCROLL,
    OverlaySink, TickOutcome,
};
use jfn_mpv::api::{
    WaitEvent, jfn_mpv_danmaku_overlay, jfn_mpv_danmaku_overlay_remove,
    jfn_mpv_get_property_double, jfn_mpv_get_property_int, jfn_mpv_get_property_string,
    jfn_mpv_wait_event, wait_event_owned,
};
use jfn_mpv::boot::{
    DisplayBackend, JfnMpvBoot, jfn_mpv_handle_get, jfn_mpv_handle_init, jfn_mpv_handle_terminate,
};
use jfn_mpv::{Event, Node};
use std::env;
use std::ffi::{CStr, CString};
use std::fs;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);
const CLIP_WIDTH: usize = 320;
const CLIP_HEIGHT: usize = 240;
const CLIP_FPS: usize = 30;
/// Long enough for the serial live tests; short enough to keep the generated
/// fixture small.
const CLIP_SECONDS: usize = 10;
/// Overlay id used for the `compute_bounds` probe. The driver owns its own ids.
const PROBE_ID: i64 = 999;
/// `reply_userdata` of the bounds probe, so its reply is unambiguous: every
/// other command in this test (including the player's danmaku pushes) uses 0.
const PROBE_REPLY: u64 = 0x6461_6e6d_0001;
/// `reply_userdata` of the raw screenshot request, which returns the frame
/// itself as a `MPV_FORMAT_BYTE_ARRAY`.
const FRAME_REPLY: u64 = 0x6461_6e6d_0002;
const LIVE_TEXT: &str = "DANMAKU OVERLAY LIVE TEST";
const SUBTITLE_TEXT: &str = "SUBTITLE COEXISTENCE TEST";

/// libmpv's handle is process-global (`jfn_mpv_handle_init` keeps it in a slot)
/// and the video output can only belong to one of them, so the live tests in
/// this file must not overlap even though cargo runs them as threads of one
/// process.
static MPV_GUARD: Mutex<()> = Mutex::new(());

/// Serialize against the other live test in this file.
fn lock_mpv() -> std::sync::MutexGuard<'static, ()> {
    MPV_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write a solid navy Y4M clip. The pixels are deterministic, while the frame
/// sequence gives libass a real media clock for testing `\move`.
#[allow(
    clippy::expect_used,
    reason = "a live test must fail loudly when it cannot write its fixture"
)]
fn write_clip(path: &Path) {
    let pixels_per_frame = CLIP_WIDTH * CLIP_HEIGHT;
    let frames = CLIP_FPS * CLIP_SECONDS;
    let mut bytes =
        format!("YUV4MPEG2 W{CLIP_WIDTH} H{CLIP_HEIGHT} F{CLIP_FPS}:1 Ip A0:0 C420jpeg\n")
            .into_bytes();
    bytes.reserve(frames * (6 + pixels_per_frame * 3 / 2));
    for _ in 0..frames {
        bytes.extend_from_slice(b"FRAME\n");
        // BT.601-ish YUV for a dark navy solid; only repeatability matters.
        bytes.extend(std::iter::repeat_n(16, pixels_per_frame));
        bytes.extend(std::iter::repeat_n(184, pixels_per_frame / 4));
        bytes.extend(std::iter::repeat_n(105, pixels_per_frame / 4));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("clip directory");
    }
    fs::write(path, bytes).expect("clip fixture");
}

/// A subtitle cue covering the whole (still) clip, so the video output has a
/// second, independent OSD layer to compare against.
#[allow(
    clippy::expect_used,
    reason = "a live test must fail loudly when it cannot write its fixture"
)]
fn write_subtitle(path: &Path) -> String {
    let text = format!("1\n00:00:00,000 --> 00:00:29,000\n{SUBTITLE_TEXT}\n");
    fs::write(path, text).expect("subtitle fixture");
    path.to_string_lossy().into_owned()
}

/// Set a property synchronously and fail loudly when mpv rejects it.
///
/// After `mpv_initialize` only update-flagged options can still be changed, and
/// a silently ignored setting would make this test assert against a
/// configuration it never actually had.
#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn set_property(name: &str, value: &str) {
    let name_c = CString::new(name).expect("property name");
    let value_c = CString::new(value).expect("property value");
    let handle = jfn_mpv_handle_get();
    assert!(!handle.is_null(), "the mpv handle should be alive");
    let status =
        unsafe { jfn_mpv::sys::mpv_set_property_string(handle, name_c.as_ptr(), value_c.as_ptr()) };
    assert_eq!(
        status, 0,
        "mpv should accept {name}={value} (error {status})"
    );
}

/// Fire an async command. `reply` becomes the reply's `reply_userdata`, which is
/// how a caller recognizes its own asynchronous result.
#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn run_command_with_reply(fields: &[String], reply: u64) {
    let owned: Vec<CString> = fields
        .iter()
        .map(|field| CString::new(field.as_str()).expect("command argument"))
        .collect();
    let mut pointers: Vec<*const c_char> = owned.iter().map(|field| field.as_ptr()).collect();
    assert!(!pointers.is_empty(), "a command needs a name");
    pointers.push(ptr::null());
    let handle = jfn_mpv_handle_get();
    assert!(!handle.is_null(), "the mpv handle should be alive");
    // SAFETY: `pointers` is NULL terminated and outlives the call; mpv copies
    // the arguments before returning.
    unsafe {
        jfn_mpv::sys::mpv_command_async(handle, reply, pointers.as_ptr() as *mut *const c_char);
    }
}

#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn run_command(fields: &[String]) {
    run_command_with_reply(fields, 0);
}

#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn loadfile(path: &Path) {
    run_command(&["loadfile".to_string(), path.to_string_lossy().into_owned()]);
}

/// Add an external subtitle file and select it, like the player does for a
/// server-provided subtitle track.
#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn add_subtitle(path: &str) {
    run_command(&[
        "sub-add".to_string(),
        path.to_string(),
        "select".to_string(),
    ]);
}

#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn video_size() -> Option<(u32, u32)> {
    let mut width = 0i64;
    let mut height = 0i64;
    let width_status = unsafe { jfn_mpv_get_property_int(c"width".as_ptr(), &mut width) };
    let height_status = unsafe { jfn_mpv_get_property_int(c"height".as_ptr(), &mut height) };
    if width_status != 0 || height_status != 0 || width < 1 || height < 1 {
        return None;
    }
    Some((
        u32::try_from(width).expect("a video width fits in u32"),
        u32::try_from(height).expect("a video height fits in u32"),
    ))
}

fn print_log(message: &jfn_mpv::LogMessage) {
    println!("mpv[{}]: {}", message.prefix, message.text);
}

/// Drain queued events for `duration`, forwarding mpv's log lines so a failing
/// live test explains itself instead of only reporting "no frame".
///
/// Returns the error codes of every command mpv rejected: mpv accepts a command
/// into its queue before it parses it, so a rejected command only shows up as an
/// async reply, never as a Rust-side error.
fn drain_commands(duration: Duration) -> Vec<i32> {
    let deadline = Instant::now() + duration;
    let mut errors = Vec::new();
    while Instant::now() < deadline {
        match wait_event_owned(0.05) {
            WaitEvent::LogMessage(message) => print_log(&message),
            WaitEvent::Event(Event::LogMessage(message)) => print_log(&message),
            WaitEvent::Event(Event::CommandReply { reply, error }) if error != 0 => {
                println!("mpv rejected a command (reply {reply}): error {error}");
                errors.push(error);
            }
            _ => {}
        }
    }
    errors
}

/// [`drain_commands`] for callers that only want mpv's log output.
fn drain(duration: Duration) {
    let _ = drain_commands(duration);
}

/// Wait until the clip is loaded *and* the video output is configured.
///
/// mpv notifies `video-reconfig` *before* `file-loaded`, so both are collected
/// in one pass instead of waiting for them in order.
fn wait_for_load() -> (bool, bool) {
    let mut loaded = false;
    let mut reconfigured = false;
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline && !(loaded && reconfigured) {
        match wait_event_owned(0.2) {
            WaitEvent::LogMessage(message) => print_log(&message),
            WaitEvent::Event(Event::LogMessage(message)) => print_log(&message),
            WaitEvent::Event(Event::FileLoaded) => loaded = true,
            WaitEvent::Event(Event::VideoReconfig) => reconfigured = true,
            WaitEvent::Event(Event::EndFile(reason)) => {
                println!("mpv ended before the clip was ready: {reason:?}");
                break;
            }
            WaitEvent::Event(Event::Shutdown) => break,
            _ => {}
        }
    }
    (loaded, reconfigured)
}

#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn read_string(name: &str) -> String {
    let name = CString::new(name).expect("property name");
    let pointer = unsafe { jfn_mpv_get_property_string(name.as_ptr()) };
    if pointer.is_null() {
        return String::from("<unavailable>");
    }
    let value = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    unsafe { jfn_mpv::api::jfn_mpv_free_string(pointer) };
    value
}

#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn read_double(name: &str) -> f64 {
    let name = CString::new(name).expect("property name");
    let mut value = 0.0;
    let status = unsafe { jfn_mpv_get_property_double(name.as_ptr(), &mut value) };
    assert_eq!(status, 0, "mpv should expose {name:?} (error {status})");
    value
}

/// mpv reports the video size only once the video output has video parameters,
/// i.e. once a screenshot can actually produce a frame.
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when the video never becomes ready"
)]
fn wait_for_video_size() -> (u32, u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(size) = video_size() {
            return size;
        }
        drain(Duration::from_millis(100));
    }
    panic!(
        "mpv never reported a video size (vo-configured: {}, current-vo: {}, video-format: {})",
        read_string("vo-configured"),
        read_string("current-vo"),
        read_string("video-format")
    );
}

/// Boot mpv the way the player does. `_screenshot_dir` is unused today but kept
/// so the helper signature matches the player boot path it mirrors.
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when libmpv cannot start"
)]
fn boot(_screenshot_dir: &Path) -> *mut jfn_mpv::sys::mpv_handle {
    let boot = JfnMpvBoot {
        display_backend: DisplayBackend::Other as u8,
        hwdec: c"no".as_ptr(),
        user_agent: c"MediaStationDanmakuLive/0.1".as_ptr(),
        audio_passthrough: ptr::null(),
        audio_exclusive: false,
        audio_channels: ptr::null(),
        geometry: c"320x240".as_ptr(),
        force_window_position: false,
        window_maximized_at_boot: false,
        mpv_log_level: c"info".as_ptr(),
        client_side_decorations: false,
    };
    let raw = unsafe { jfn_mpv_handle_init(&boot) };
    assert!(!raw.is_null(), "libmpv should initialize");
    raw
}

/// One composited frame straight out of the video output (`screenshot-raw`).
struct Frame {
    width: usize,
    height: usize,
    stride: usize,
    data: Vec<u8>,
}

impl Frame {
    fn row(&self, row: usize) -> Option<&[u8]> {
        let start = row.checked_mul(self.stride)?;
        let end = start.checked_add(self.stride)?;
        self.data.get(start..end)
    }

    /// Rows whose pixels differ between two frames.
    fn differing_rows(&self, other: &Self) -> Vec<usize> {
        (0..self.height.min(other.height))
            .filter(|row| self.row(*row) != other.row(*row))
            .collect()
    }

    /// Bounds of the pixels that changed relative to another composited frame.
    fn changed_bounds(&self, other: &Self) -> Option<PixelBounds> {
        let width = self.width.min(other.width);
        let height = self.height.min(other.height);
        let mut bounds: Option<PixelBounds> = None;
        for y in 0..height {
            for x in 0..width {
                let start = y * self.stride + x * 4;
                let other_start = y * other.stride + x * 4;
                if self.data[start..start + 4] != other.data[other_start..other_start + 4] {
                    bounds = Some(match bounds {
                        Some(bounds) => bounds.extend(x, y),
                        None => PixelBounds::new(x, y),
                    });
                }
            }
        }
        bounds
    }
}

#[derive(Debug, Clone, Copy)]
struct PixelBounds {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl PixelBounds {
    fn new(x: usize, y: usize) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x,
            y1: y,
        }
    }

    fn extend(self, x: usize, y: usize) -> Self {
        Self {
            x0: self.x0.min(x),
            y0: self.y0.min(y),
            x1: self.x1.max(x),
            y1: self.y1.max(y),
        }
    }
}

/// Read the current composited frame (video + subtitles + external OSD).
///
/// Retries a rejected request: right after a load mpv can still be creating its
/// video output, and "no frame yet" (`MPV_ERROR_COMMAND` from
/// `screenshot_get()`) is transient. A persistent failure still panics, with the
/// last error mpv gave.
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when mpv cannot produce a frame"
)]
fn screenshot_raw() -> Frame {
    let deadline = Instant::now() + TIMEOUT;
    let mut last_error = 0;
    while Instant::now() < deadline {
        run_command_with_reply(
            &[
                "screenshot-raw".to_string(),
                "window".to_string(),
                "bgr0".to_string(),
            ],
            FRAME_REPLY,
        );
        if let Some(frame) = frame_reply(deadline, &mut last_error) {
            return frame;
        }
    }
    panic!("mpv never returned a raw frame (last error {last_error})");
}

/// Wait for the reply to `screenshot-raw`. `None` means mpv rejected it (the
/// error is recorded) or the deadline passed.
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when mpv cannot produce a frame"
)]
fn frame_reply(deadline: Instant, last_error: &mut i32) -> Option<Frame> {
    while Instant::now() < deadline {
        // SAFETY: the returned event is decoded before the next wait call.
        let raw = jfn_mpv_wait_event(0.2);
        if raw.is_null() {
            continue;
        }
        let event = unsafe { &*raw };
        if event.event_id != jfn_mpv::sys::mpv_event_id::MPV_EVENT_COMMAND_REPLY
            || event.reply_userdata != FRAME_REPLY
        {
            continue;
        }
        if event.error != 0 {
            println!("screenshot-raw rejected with error {}", event.error);
            *last_error = event.error;
            return None;
        }
        assert!(!event.data.is_null(), "the raw frame reply carries no data");
        let node = unsafe { Node::from_raw(event.data as *const jfn_mpv::sys::mpv_node) };
        let number = |key: &str| node.get(key).and_then(|value| value.as_int());
        let (Some(width), Some(height), Some(stride)) =
            (number("w"), number("h"), number("stride"))
        else {
            panic!("the raw frame reply should describe its geometry: {node:?}");
        };
        let Some(Node::ByteArray(data)) = node.get("data") else {
            panic!("the raw frame reply should carry pixel data");
        };
        let (width, height, stride) = (
            usize::try_from(width).expect("width fits usize"),
            usize::try_from(height).expect("height fits usize"),
            usize::try_from(stride).expect("stride fits usize"),
        );
        assert!(
            stride >= width * 4,
            "bgr0 rows are at least 4 bytes per pixel (stride {stride}, width {width})"
        );
        assert_eq!(
            data.len(),
            stride * height,
            "the frame should hold one stride per row"
        );
        return Some(Frame {
            width,
            height,
            stride,
            data: data.clone(),
        });
    }
    None
}

/// Rectangle mpv reports for a pushed overlay, in the units of `res_x`/`res_y`.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

/// Ask mpv what rectangle libass rasterized `ass` into.
///
/// `osd_set_external` (sub/osd_libass.c) renders the pushed events through
/// libass with the event time pinned to 0 and hands the resulting bitmap list to
/// `mp_ass_get_bb`, which converts the union box back to `res_x`/`res_y` units.
/// An empty result means the events produced no pixels at all.
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when mpv cannot be asked"
)]
fn measure_bounds(ass: &str, res_x: u32, res_y: u32) -> Option<Bounds> {
    let fields = [
        "osd-overlay".to_string(),
        PROBE_ID.to_string(),
        "ass-events".to_string(),
        ass.to_string(),
        res_x.to_string(),
        res_y.to_string(),
        "0".to_string(),
        "no".to_string(),
        "yes".to_string(), // compute_bounds
    ];
    let owned: Vec<CString> = fields
        .iter()
        .map(|field| CString::new(field.as_str()).expect("command argument"))
        .collect();
    let mut pointers: Vec<*const c_char> = owned.iter().map(|field| field.as_ptr()).collect();
    pointers.push(ptr::null());
    let handle = jfn_mpv_handle_get();
    assert!(!handle.is_null(), "the mpv handle should be alive");
    // SAFETY: `pointers` is NULL terminated and outlives the call; the reply is
    // matched by `PROBE_REPLY`.
    unsafe {
        jfn_mpv::sys::mpv_command_async(
            handle,
            PROBE_REPLY,
            pointers.as_ptr() as *mut *const c_char,
        );
    }

    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        // SAFETY: the returned event is decoded before the next wait call.
        let raw = jfn_mpv_wait_event(0.2);
        if raw.is_null() {
            continue;
        }
        let event = unsafe { &*raw };
        if event.event_id != jfn_mpv::sys::mpv_event_id::MPV_EVENT_COMMAND_REPLY
            || event.reply_userdata != PROBE_REPLY
        {
            continue;
        }
        if event.error != 0 {
            println!("osd-overlay compute_bounds failed: {}", event.error);
            return None;
        }
        if event.data.is_null() {
            return None;
        }
        let node = unsafe { Node::from_raw(event.data as *const jfn_mpv::sys::mpv_node) };
        let number = |key: &str| node.get(key).and_then(|value| value.as_double());
        let (Some(x0), Some(y0), Some(x1), Some(y1)) =
            (number("x0"), number("y0"), number("x1"), number("y1"))
        else {
            // An empty box is reported by omitting the keys entirely.
            return None;
        };
        return Some(Bounds { x0, y0, x1, y1 });
    }
    None
}

/// Pushes through the same function the player uses and keeps the ASS payload,
/// so the test can ask mpv what it actually rasterized.
#[derive(Default)]
struct RecordingSink {
    payload: Option<String>,
}

impl OverlaySink for RecordingSink {
    fn push(&mut self, id: i64, data: &str, res_x: u32, res_y: u32, z: i32) {
        self.payload = Some(data.to_string());
        let Ok(payload) = CString::new(data) else {
            return;
        };
        // SAFETY: the CString outlives the call and mpv copies the string.
        unsafe { jfn_mpv_danmaku_overlay(id, payload.as_ptr(), res_x as i32, res_y as i32, z) };
    }

    fn clear(&mut self, id: i64) {
        self.payload = None;
        jfn_mpv_danmaku_overlay_remove(id);
    }
}

/// What a capture run asks for: the layers of the player's UI under test.
#[derive(Clone, Copy, Default)]
struct Scenario {
    danmaku: bool,
    subtitles: bool,
}

struct Capture {
    frame: Frame,
    /// Frame after the overlays were cleared again.
    cleared: Frame,
    bounds: Option<Bounds>,
    payload: Option<String>,
}

/// Render `video` once through the player's video output with the requested
/// layers, and return the composited frame plus what mpv reported.
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when rendering does not complete"
)]
fn capture(dir: &Path, video: &Path, scenario: Scenario) -> Capture {
    fs::create_dir_all(dir).expect("capture directory");
    let _handle = boot(dir);
    loadfile(video);
    let (loaded, reconfigured) = wait_for_load();
    assert!(loaded, "the clip should load: {}", video.display());
    assert!(
        reconfigured,
        "mpv should configure a video output for {}",
        video.display()
    );
    // The player disables mpv's automatic track selection and picks tracks from
    // the server's playback info, so the clip's video track has to be selected
    // explicitly before any frame can be rendered.
    set_property("vid", "1");
    let (res_x, res_y) = wait_for_video_size();

    if scenario.subtitles {
        add_subtitle(&write_subtitle(&dir.join("coexistence.srt")));
    }

    let mut sink = RecordingSink::default();
    let mut driver = match scenario.danmaku {
        true => {
            let comments = vec![DanmakuComment::new("live-1", 0.0, 5, 0x00_FF_00, LIVE_TEXT)];
            let style = DanmakuStyle {
                font_size: 32.0,
                display_area: 0.5,
                ..DanmakuStyle::default()
            };
            let mut driver = DanmakuDriver::new(comments, style);
            // The driver takes the player's pause state; this test does not
            // pause playback, so the comment is expected on screen.
            let outcome = driver.tick(0.5, false, res_x, res_y, &mut sink);
            assert_eq!(
                outcome,
                TickOutcome::Updated,
                "the live comment should be pushed to the overlay"
            );
            Some(driver)
        }
        false => None,
    };
    // mpv parses queued commands on its own thread, so a rejected danmaku
    // overlay would otherwise go unnoticed until the frame comparison.
    let rejected = drain_commands(Duration::from_millis(600));
    assert!(
        rejected.is_empty(),
        "mpv rejected command(s) while pushing the overlay: errors {rejected:?}"
    );

    let payload = sink.payload.clone();
    let bounds = payload
        .as_deref()
        .and_then(|ass| measure_bounds(ass, res_x, res_y));
    if payload.is_some() {
        // Drop the probe overlay again so the frame only shows what the player
        // itself pushed.
        run_command(&[
            "osd-overlay".to_string(),
            PROBE_ID.to_string(),
            "none".to_string(),
            String::new(),
        ]);
        let rejected = drain_commands(Duration::from_millis(200));
        assert!(
            rejected.is_empty(),
            "mpv rejected command(s) while removing the probe overlay: errors {rejected:?}"
        );
    }

    let frame = screenshot_raw();

    if let Some(driver) = driver.as_mut() {
        driver.stop(&mut sink);
    }
    let rejected = drain_commands(Duration::from_millis(200));
    assert!(
        rejected.is_empty(),
        "mpv rejected command(s) while clearing the overlay: errors {rejected:?}"
    );
    // A cleared overlay has to leave the frame exactly as it was before.
    let cleared = screenshot_raw();
    jfn_mpv_handle_terminate();

    Capture {
        frame,
        cleared,
        bounds,
        payload,
    }
}

fn run_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("danmaku-overlay-live-{name}"));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[allow(
    clippy::panic,
    reason = "a live test must fail loudly on its own input"
)]
fn assert_same_geometry(name: &str, reference: &Frame, other: &Frame) {
    assert_eq!(
        (reference.width, reference.height, reference.stride),
        (other.width, other.height, other.stride),
        "{name} should render the same frame geometry"
    );
}

#[test]
#[ignore = "needs libmpv and a desktop session"]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when its input is missing"
)]
fn danmaku_overlay_is_rendered_into_the_video_output() {
    let _guard = lock_mpv();
    let video = match env::var("DANMAKU_LIVE_VIDEO") {
        Ok(path) => PathBuf::from(path),
        Err(_) => {
            let generated = env::temp_dir().join("danmaku-overlay-live-clip.y4m");
            write_clip(&generated);
            generated
        }
    };
    assert!(video.is_file(), "clip not found: {}", video.display());

    let baseline = capture(&run_dir("baseline"), &video, Scenario::default());
    let with_danmaku = capture(
        &run_dir("danmaku"),
        &video,
        Scenario {
            danmaku: true,
            subtitles: false,
        },
    );
    let with_subtitles = capture(
        &run_dir("subtitles"),
        &video,
        Scenario {
            danmaku: false,
            subtitles: true,
        },
    );
    let with_both = capture(
        &run_dir("both"),
        &video,
        Scenario {
            danmaku: true,
            subtitles: true,
        },
    );

    let height = baseline.frame.height;
    for (name, capture) in [
        ("the danmaku run", &with_danmaku),
        ("the subtitle run", &with_subtitles),
        ("the combined run", &with_both),
    ] {
        assert_same_geometry(name, &baseline.frame, &capture.frame);
        assert_same_geometry(name, &baseline.frame, &capture.cleared);
    }

    // Rows each layer touches on its own.
    let danmaku_rows = baseline.frame.differing_rows(&with_danmaku.frame);
    let subtitle_rows = baseline.frame.differing_rows(&with_subtitles.frame);
    assert!(
        !danmaku_rows.is_empty(),
        "the danmaku overlay did not reach the video output: the frame is identical to the baseline"
    );
    assert!(
        !subtitle_rows.is_empty(),
        "the subtitle track did not render, so coexistence cannot be checked"
    );

    let half = height / 2;
    assert!(
        danmaku_rows.iter().all(|row| *row < half),
        "a top-anchored comment should only touch the upper half: rows {danmaku_rows:?}"
    );
    assert!(
        subtitle_rows.iter().all(|row| *row >= half),
        "the subtitle should only touch the lower half: rows {subtitle_rows:?}"
    );

    // Coexistence: each layer draws the same pixels whether or not the other one
    // is present, i.e. neither displaces nor covers the other.
    let only_danmaku = with_both.frame.differing_rows(&with_subtitles.frame);
    assert_eq!(
        only_danmaku, danmaku_rows,
        "with a subtitle present the danmaku should still change exactly the same rows"
    );
    let only_subtitle = with_both.frame.differing_rows(&with_danmaku.frame);
    assert_eq!(
        only_subtitle, subtitle_rows,
        "with danmaku present the subtitle should still change exactly the same rows"
    );

    let payload = with_danmaku
        .payload
        .as_deref()
        .expect("the driver should have pushed ASS events");
    assert!(
        payload.contains(LIVE_TEXT),
        "the pushed payload should carry the comment text: {payload}"
    );

    let bounds = with_danmaku
        .bounds
        .expect("mpv should report the rasterized bounds of the pushed overlay");
    let (width, height_f) = (baseline.frame.width as f64, baseline.frame.height as f64);
    assert!(
        bounds.x0 < bounds.x1 && bounds.y0 < bounds.y1,
        "the overlay should rasterize to a non-empty box: {bounds:?}"
    );
    assert!(
        bounds.x0 >= 0.0 && bounds.y0 >= 0.0 && bounds.x1 <= width && bounds.y1 <= height_f,
        "the overlay should stay inside the {width}x{height_f} frame: {bounds:?}"
    );
    assert!(
        bounds.y1 <= height_f / 2.0,
        "a top-anchored comment should stay in the upper half of the frame: {bounds:?}"
    );
    // The rectangle mpv measured for the overlay must contain the rows that
    // actually changed (the box is the union of the glyph bitmaps, so it can be
    // taller than the ink it contains).
    let first = danmaku_rows.first().copied().unwrap_or(0) as f64;
    let last = danmaku_rows.last().copied().unwrap_or(0) as f64;
    assert!(
        first >= bounds.y0 - 1.0 && last <= bounds.y1 + 1.0,
        "the rows that changed ({first}..={last}) should lie inside the reported box {bounds:?}"
    );

    // Clearing the overlays must restore the frame the player had before.
    assert_eq!(
        with_danmaku.cleared.data, baseline.frame.data,
        "clearing the danmaku should reproduce the baseline frame"
    );
    assert_eq!(
        with_both.cleared.data, with_subtitles.frame.data,
        "clearing the danmaku should leave the subtitle frame untouched"
    );

    println!(
        "danmaku changed rows {first}..={last} (box {bounds:?}), the subtitle changed rows \
         {}..={}, and each layer kept identical pixels with the other one present",
        subtitle_rows.first().copied().unwrap_or(0),
        subtitle_rows.last().copied().unwrap_or(0),
    );
}

/// Poll the composited frame until it differs from (or matches) `reference`.
///
/// Returns the last frame it saw; the caller asserts on it, so a timeout shows
/// the frame it gave up on instead of a bare "timed out".
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when mpv stops producing frames"
)]
fn wait_for_frame(reference: &Frame, want_same: bool, timeout: Duration) -> Frame {
    let deadline = Instant::now() + timeout;
    loop {
        let frame = screenshot_raw();
        let same = frame.differing_rows(reference).is_empty();
        if same == want_same || Instant::now() >= deadline {
            return frame;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The player UI's danmaku switch has to work during playback: flipping it off
/// removes the overlay from the video output, flipping it on brings it back, and
/// neither disturbs the video itself.
///
/// Unlike the other live test this one drives `DanmakuSession` (the real worker,
/// reading `time-pos` from mpv) rather than ticking the driver by hand, so it
/// also proves the worker stops and resumes with the switch.
#[test]
#[ignore = "needs libmpv and a desktop session"]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when its input is missing"
)]
fn the_danmaku_switch_clears_and_restores_the_overlay() {
    let _guard = lock_mpv();
    let video = match env::var("DANMAKU_LIVE_VIDEO") {
        Ok(path) => PathBuf::from(path),
        Err(_) => {
            let generated = env::temp_dir().join("danmaku-overlay-live-clip.ppm");
            write_clip(&generated);
            generated
        }
    };
    assert!(video.is_file(), "clip not found: {}", video.display());

    let dir = run_dir("switch");
    fs::create_dir_all(&dir).expect("capture directory");
    let _handle = boot(&dir);
    loadfile(&video);
    let (loaded, reconfigured) = wait_for_load();
    assert!(loaded, "the clip should load: {}", video.display());
    assert!(reconfigured, "mpv should configure a video output");
    set_property("vid", "1");
    let (_res_x, _res_y) = wait_for_video_size();

    // Nothing of ours is on screen yet, and the clip has solid-color pixels, so
    // this frame is the reference the switch has to restore.
    let baseline = screenshot_raw();

    let comments = vec![DanmakuComment::new("live-1", 0.0, 5, 0x00_FF_00, LIVE_TEXT)];
    let style = DanmakuStyle {
        font_size: 32.0,
        display_area: 0.5,
        // Long enough that the comment cannot expire while the test polls.
        fixed_seconds: 600.0,
        ..DanmakuStyle::default()
    };
    let controls = DanmakuControls::new_with_style(true, style);
    let failures = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink_failures = Arc::clone(&failures);
    let mut session = DanmakuSession::start(comments, controls.clone(), move |message| {
        if let Ok(mut collected) = sink_failures.lock() {
            collected.push(message.to_string());
        }
    })
    .expect("the danmaku worker should start");

    let with_danmaku = wait_for_frame(&baseline, false, Duration::from_secs(10));
    let shown = with_danmaku.differing_rows(&baseline);
    let worker_failures = failures.lock().expect("failure list").clone();
    assert!(
        !shown.is_empty(),
        "the worker did not draw anything: sid={} secondary-sid={} secondary-sub-visibility={} failures={worker_failures:?}",
        read_string("sid"),
        read_string("secondary-sid"),
        read_string("secondary-sub-visibility"),
    );
    assert_eq!(
        read_string("sid"),
        "no",
        "danmaku must not take the primary subtitle slot when no subtitle was selected"
    );
    assert_ne!(
        read_string("secondary-sid"),
        "no",
        "danmaku should be selected only as the secondary subtitle"
    );

    controls.set_enabled(false);
    let cleared = wait_for_frame(&with_danmaku, false, Duration::from_secs(10));
    assert_eq!(
        cleared.data, baseline.data,
        "switching danmaku off should remove the overlay and restore the frame exactly"
    );

    controls.set_enabled(true);
    let restored = wait_for_frame(&baseline, false, Duration::from_secs(10));
    assert_eq!(
        restored.data, with_danmaku.data,
        "switching danmaku back on should draw the same frame again"
    );

    session.stop();
    let after_stop = wait_for_frame(&baseline, true, Duration::from_secs(10));
    assert_eq!(
        after_stop.data, baseline.data,
        "stopping the session should clear the overlay"
    );
    jfn_mpv_handle_terminate();

    let failures = failures.lock().expect("failure list").clone();
    assert!(failures.is_empty(), "the worker reported: {failures:?}");
    println!(
        "the switch changed rows {}..={} while on and restored the frame byte for byte when off",
        shown.first().copied().unwrap_or(0),
        shown.last().copied().unwrap_or(0),
    );
}

/// A regular ASS subtitle track, rather than an `ass-events` OSD snapshot,
/// must advance the comment between video frames. This is the real player path:
/// the ordinary subtitle stays primary while danmaku occupies `secondary-sid`.
#[test]
#[ignore = "needs libmpv and a desktop session"]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when its input is missing"
)]
fn timed_ass_danmaku_moves_between_composited_frames() {
    let _guard = lock_mpv();
    let video = match env::var("DANMAKU_LIVE_VIDEO") {
        Ok(path) => PathBuf::from(path),
        Err(_) => {
            let generated = env::temp_dir().join("danmaku-timed-ass-live-clip.y4m");
            write_clip(&generated);
            generated
        }
    };
    assert!(video.is_file(), "clip not found: {}", video.display());

    let dir = run_dir("timed-ass");
    fs::create_dir_all(&dir).expect("capture directory");
    let _handle = boot(&dir);
    loadfile(&video);
    let (loaded, reconfigured) = wait_for_load();
    assert!(loaded, "the clip should load: {}", video.display());
    assert!(reconfigured, "mpv should configure a video output");
    set_property("vid", "1");
    let (_res_x, _res_y) = wait_for_video_size();
    set_property("pause", "no");

    let subtitle = write_subtitle(&dir.join("primary.srt"));
    add_subtitle(&subtitle);
    drain(Duration::from_millis(300));
    let primary_sid = read_string("sid");
    assert_ne!(
        primary_sid, "no",
        "the ordinary subtitle should be selected"
    );
    let baseline = screenshot_raw();
    let previous_secondary_ass_override = read_string("secondary-sub-ass-override");

    let comment_time = read_double("time-pos");
    let comments = vec![DanmakuComment::new(
        "moving-live-1",
        comment_time,
        MODE_SCROLL,
        0x00_FF_00,
        LIVE_TEXT,
    )];
    let controls = DanmakuControls::new_with_style(
        true,
        DanmakuStyle {
            font_size: 32.0,
            display_area: 0.5,
            scroll_seconds: 6.0,
            ..DanmakuStyle::default()
        },
    );
    let failures = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink_failures = Arc::clone(&failures);
    let mut session = DanmakuSession::start(comments, controls, move |message| {
        if let Ok(mut collected) = sink_failures.lock() {
            collected.push(message.to_string());
        }
    })
    .expect("the danmaku worker should start");

    let first = wait_for_frame(&baseline, false, Duration::from_secs(10));
    let first_bounds = first.changed_bounds(&baseline).unwrap_or_else(|| {
        panic!(
            "the first danmaku frame should contain visible glyphs: time-pos={} sid={} secondary-sid={} secondary-sub-visibility={} secondary-sub-ass-override={} failures={:?}",
            read_double("time-pos"),
            read_string("sid"),
            read_string("secondary-sid"),
            read_string("secondary-sub-visibility"),
            read_string("secondary-sub-ass-override"),
            failures.lock().expect("failure list"),
        )
    });
    std::thread::sleep(Duration::from_millis(400));
    let second = screenshot_raw();
    let second_bounds = second
        .changed_bounds(&baseline)
        .expect("the second danmaku frame should contain visible glyphs");
    assert!(
        second_bounds.x0 + 8 < first_bounds.x0,
        "the ASS event should move left while visible: first={first_bounds:?}, second={second_bounds:?}"
    );
    assert_eq!(
        read_string("sid"),
        primary_sid,
        "adding danmaku as secondary must preserve the primary subtitle"
    );
    assert_ne!(
        read_string("secondary-sid"),
        "no",
        "the running danmaku track should be selected as secondary"
    );
    session.stop();
    let after_stop = wait_for_frame(&baseline, true, Duration::from_secs(10));
    assert_eq!(
        after_stop.data, baseline.data,
        "removing the danmaku track should leave the primary subtitle untouched"
    );
    assert_eq!(
        read_string("secondary-sub-ass-override"),
        previous_secondary_ass_override,
        "stopping danmaku should restore the user's secondary subtitle ASS setting"
    );
    jfn_mpv_handle_terminate();

    let failures = failures.lock().expect("failure list").clone();
    assert!(failures.is_empty(), "the worker reported: {failures:?}");
    println!(
        "timed ASS at {comment_time:.3}s moved from x={}..={} to x={}..={} while primary sid {} stayed selected",
        first_bounds.x0, first_bounds.x1, second_bounds.x0, second_bounds.x1, primary_sid,
    );
}
