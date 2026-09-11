#![cfg(feature = "mpv")]
//! Live verification that danmaku actually reaches mpv's video output.
//!
//! `osd-overlay` (`format=ass-events`) is drawn by the video output, not by the
//! subtitle renderer, so unit tests can only prove that mpv *accepted* the
//! command. This test proves pixels: the same clip is rendered twice through the
//! player's own video output and the screenshots must differ.
//!
//! Two independent facts are asserted:
//!
//! * `screenshot-to-file <file> window` renders video + subtitles + OSD through
//!   the VO. In `vo_gpu_next.c` that mode is `subs = osd = true`, i.e.
//!   `osd_flags = 0`, so libass-drawn *external* OSD overlays are composited
//!   exactly like they are in the player's window.
//! * `osd-overlay ... compute_bounds=yes` makes mpv report the rectangle libass
//!   rasterized the pushed events into, so the test can check that a
//!   top-anchored comment really lands in the upper half of the frame.
//!
//! Ignored by default: it needs libmpv and a desktop session (mpv opens its own
//! window for a few seconds). The clip is generated on the spot — a still PPM,
//! which mpv reads through its image demuxer — so no fixture is shipped; set
//! `DANMAKU_LIVE_VIDEO` to run against a real clip instead.
//!
//! ```text
//! cargo test -p jfn-danmaku --features mpv --test mpv_overlay_live -- --ignored --nocapture
//! ```

use jfn_danmaku::{DanmakuComment, DanmakuDriver, DanmakuStyle, OverlaySink, TickOutcome};
use jfn_mpv::api::{
    WaitEvent, jfn_mpv_command_async, jfn_mpv_danmaku_overlay, jfn_mpv_danmaku_overlay_remove,
    jfn_mpv_get_property_int, jfn_mpv_get_property_string, jfn_mpv_wait_event, wait_event_owned,
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
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);
const CLIP_WIDTH: usize = 320;
const CLIP_HEIGHT: usize = 240;
/// mpv holds a still image this long; the test is done long before it ends, so
/// `video_screenshot` never runs against an end-of-file frame queue.
const CLIP_SECONDS: &str = "30";
/// Overlay id used for the `compute_bounds` probe. The driver owns its own ids.
const PROBE_ID: i64 = 999;
/// `reply_userdata` of the bounds probe, so its reply is unambiguous: every
/// other command in this test (including the player's danmaku pushes) uses 0.
const PROBE_REPLY: u64 = 0x6461_6e6d_0001;
const LIVE_TEXT: &str = "DANMAKU OVERLAY LIVE TEST";
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Write a solid navy PPM. mpv reads PPM through its image demuxer, so this
/// gives us a deterministic frame source without shipping a video fixture or
/// depending on a runnable ffmpeg.
#[allow(
    clippy::expect_used,
    reason = "a live test must fail loudly when it cannot write its fixture"
)]
fn write_clip(path: &Path) {
    let mut bytes = format!("P6\n{CLIP_WIDTH} {CLIP_HEIGHT}\n255\n").into_bytes();
    bytes.reserve(CLIP_WIDTH * CLIP_HEIGHT * 3);
    for _ in 0..CLIP_WIDTH * CLIP_HEIGHT {
        bytes.extend_from_slice(&[0, 0, 128]);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("clip directory");
    }
    fs::write(path, bytes).expect("clip fixture");
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

#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn loadfile(path: &Path) {
    let command = CString::new("loadfile").expect("command");
    let file = CString::new(path.to_string_lossy().as_bytes()).expect("path");
    let args = [command.as_ptr(), file.as_ptr()];
    // SAFETY: both CStrings outlive the call.
    unsafe { jfn_mpv_command_async(args.as_ptr(), args.len()) };
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
/// live test explains itself instead of only reporting "no screenshot".
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

/// Boot mpv the way the player does. `screenshot_dir` is used for mpv's own
/// diagnostics only; the test writes its screenshot to an explicit path.
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
    // The clip is a still PPM, so mpv's one-second default would end playback
    // before the screenshot is taken.
    set_property("image-display-duration", CLIP_SECONDS);
    raw
}

/// `screenshot-to-file <path> window`: video + subtitles + external OSD.
#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn screenshot(dest: &Path) -> bool {
    let _ = fs::remove_file(dest);
    let command = CString::new("screenshot-to-file").expect("command");
    let path = CString::new(dest.to_string_lossy().as_bytes()).expect("path");
    let flags = CString::new("window").expect("flags");
    let args = [command.as_ptr(), path.as_ptr(), flags.as_ptr()];
    // SAFETY: all three CStrings outlive the call.
    unsafe { jfn_mpv_command_async(args.as_ptr(), args.len()) };

    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        drain(Duration::from_millis(100));
        if dest.is_file() {
            return true;
        }
    }
    false
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

struct Capture {
    png: Vec<u8>,
    /// Screenshot taken after the overlays were cleared again.
    cleared: Option<Vec<u8>>,
    bounds: Option<Bounds>,
    payload: Option<String>,
}

/// Render `video` once through the player's video output, optionally with one
/// live danmaku comment, and return the screenshot plus what mpv reported.
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when rendering does not complete"
)]
fn capture(dir: &Path, video: &Path, comment: Option<&str>) -> Capture {
    fs::create_dir_all(dir).expect("screenshot directory");
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
    let mut sink = RecordingSink::default();
    let mut driver = match comment {
        Some(text) => {
            let comments = vec![DanmakuComment::new("live-1", 0.0, 5, 0x00_FF_00, text)];
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
        None => None,
    };
    // mpv parses queued commands on its own thread, so a rejected danmaku
    // overlay would otherwise go unnoticed until the screenshot comparison.
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
        // Drop the probe overlay again so the screenshot only shows what the
        // player itself pushed.
        remove_probe();
        let rejected = drain_commands(Duration::from_millis(200));
        assert!(
            rejected.is_empty(),
            "mpv rejected command(s) while removing the probe overlay: errors {rejected:?}"
        );
    }

    let png_path = dir.join("frame.png");
    assert!(
        screenshot(&png_path),
        "mpv should write a window screenshot to {}",
        png_path.display()
    );
    let png = fs::read(&png_path).expect("screenshot bytes");

    if let Some(driver) = driver.as_mut() {
        driver.stop(&mut sink);
    }
    let rejected = drain_commands(Duration::from_millis(200));
    assert!(
        rejected.is_empty(),
        "mpv rejected command(s) while clearing the overlay: errors {rejected:?}"
    );
    // A cleared overlay has to leave the frame exactly as it was before.
    let cleared_path = dir.join("cleared.png");
    let cleared = screenshot(&cleared_path).then(|| fs::read(&cleared_path).ok());
    jfn_mpv_handle_terminate();

    Capture {
        png,
        cleared: cleared.flatten(),
        bounds,
        payload,
    }
}

/// Removes the `compute_bounds` probe overlay again.
///
/// The array form of `osd-overlay` needs every argument up to `data`, so the
/// empty string is passed explicitly.
#[allow(clippy::expect_used, reason = "a live test must fail loudly")]
fn remove_probe() {
    let fields = [
        "osd-overlay".to_string(),
        PROBE_ID.to_string(),
        "none".to_string(),
        String::new(),
    ];
    let owned: Vec<CString> = fields
        .iter()
        .map(|field| CString::new(field.as_str()).expect("command argument"))
        .collect();
    let pointers: Vec<*const c_char> = owned.iter().map(|field| field.as_ptr()).collect();
    // SAFETY: the CStrings outlive the call.
    unsafe { jfn_mpv_command_async(pointers.as_ptr(), pointers.len()) };
}

/// PNG dimensions from the IHDR chunk, plus a signature check.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..8)? != PNG_SIGNATURE || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

fn run_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("danmaku-overlay-live-{name}"));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
#[ignore = "needs libmpv and a desktop session"]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a live test must fail loudly when its input is missing"
)]
fn danmaku_overlay_is_rendered_into_the_video_output() {
    let video = match env::var("DANMAKU_LIVE_VIDEO") {
        Ok(path) => PathBuf::from(path),
        Err(_) => {
            let generated = env::temp_dir().join("danmaku-overlay-live-clip.ppm");
            write_clip(&generated);
            generated
        }
    };
    assert!(video.is_file(), "clip not found: {}", video.display());

    let baseline = capture(&run_dir("baseline"), &video, None);
    let with_overlay = capture(&run_dir("overlay"), &video, Some(LIVE_TEXT));

    let base_size = png_size(&baseline.png).expect("baseline screenshot should be a PNG");
    let overlay_size = png_size(&with_overlay.png).expect("overlay screenshot should be a PNG");
    assert_eq!(
        base_size, overlay_size,
        "both runs must render the same frame geometry"
    );
    let differing = baseline
        .png
        .iter()
        .zip(with_overlay.png.iter())
        .filter(|(left, right)| left != right)
        .count();
    assert!(
        differing > 0,
        "the danmaku overlay did not reach the video output: both screenshots are identical \
         ({}x{} PNG, {} bytes)",
        base_size.0,
        base_size.1,
        baseline.png.len()
    );

    let payload = with_overlay
        .payload
        .as_deref()
        .expect("the driver should have pushed ASS events");
    assert!(
        payload.contains(LIVE_TEXT),
        "the pushed payload should carry the comment text: {payload}"
    );

    let bounds = with_overlay
        .bounds
        .expect("mpv should report the rasterized bounds of the pushed overlay");
    let (width, height) = (f64::from(base_size.0), f64::from(base_size.1));
    assert!(
        bounds.x0 < bounds.x1 && bounds.y0 < bounds.y1,
        "the overlay should rasterize to a non-empty box: {bounds:?}"
    );
    assert!(
        bounds.x0 >= 0.0 && bounds.y0 >= 0.0 && bounds.x1 <= width && bounds.y1 <= height,
        "the overlay should stay inside the {width}x{height} frame: {bounds:?}"
    );
    assert!(
        bounds.y1 <= height / 2.0,
        "a top-anchored comment should stay in the upper half of the frame: {bounds:?}"
    );

    // Clearing the overlays must remove the danmaku from the video output again
    // (and, with a still clip, reproduce the baseline frame byte for byte).
    let cleared = with_overlay
        .cleared
        .as_deref()
        .expect("mpv should still write a screenshot after the overlays are cleared");
    assert_eq!(
        png_size(cleared),
        Some(base_size),
        "the cleared screenshot should keep the frame geometry"
    );
    assert_ne!(
        cleared, with_overlay.png,
        "clearing the overlays did not remove the danmaku from the video output"
    );
    let cleared_matches_baseline = cleared == baseline.png;
    assert!(
        cleared_matches_baseline,
        "the cleared frame should be identical to the baseline ({} vs {} PNG bytes)",
        cleared.len(),
        baseline.png.len()
    );

    println!(
        "overlay rasterized to {bounds:?} in a {width}x{height} frame; \
         {differing} of {} PNG bytes differ from the baseline, and the cleared frame \
         is byte-identical to it",
        baseline.png.len()
    );
}
