//! mpv backed danmaku session (feature `mpv`).
//!
//! mpv's `osd-overlay` frames carry no timeline, so a worker thread ticks the
//! layout engine against `time-pos` and pushes a fresh frame each tick. Keeping
//! this behind a feature lets the layout crate stay dependency free while the
//! mpv glue is still compiled and linted on its own.
//!
//! Placement in mpv's Z order is what makes this safe next to subtitles: custom
//! OSD is always drawn above the builtin OSD, subtitles included, so danmaku can
//! never displace a subtitle track.

use crate::{DanmakuComment, DanmakuDriver, DanmakuStyle, OverlaySink};
use jfn_mpv::api::{jfn_mpv_danmaku_overlay, jfn_mpv_danmaku_overlay_remove};
use std::ffi::{CStr, CString};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

/// ~30 Hz: smooth enough for scrolling comments, cheap enough for a laptop on
/// battery. Identical frames are not re-pushed, so idle stretches cost nothing.
const TICK_MILLIS: u64 = 33;
/// While no file has a timeline there is nothing to draw; poll slowly.
const IDLE_TICK_MILLIS: u64 = 250;
const DEFAULT_PLAY_RES_X: u32 = 1920;
const DEFAULT_PLAY_RES_Y: u32 = 1080;

/// A running danmaku worker for one playback item.
pub struct DanmakuSession {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl DanmakuSession {
    /// Spawn the worker. `on_error` receives human readable problems (a frame
    /// containing a NUL byte, a rejected overlay command) so the caller can log
    /// them in its own logging category instead of the failure being swallowed.
    pub fn start(
        comments: Vec<DanmakuComment>,
        style: DanmakuStyle,
        on_error: impl Fn(&str) + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("danmaku-overlay".to_string())
            .spawn(move || worker_loop(worker_stop, comments, style, on_error))?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }

    /// Ask the worker to stop without waiting for it. Used on paths that hold a
    /// lock, where joining for up to one tick would be rude.
    pub fn signal_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Stop without waiting: the thread finishes its current tick, clears the
    /// overlays and exits.
    pub fn detach(&mut self) {
        self.signal_stop();
        self.worker.take();
    }

    /// Stop and wait, so this session's final overlay clear cannot land after a
    /// newer session pushed its first frame.
    pub fn stop(&mut self) {
        self.signal_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for DanmakuSession {
    fn drop(&mut self) {
        self.stop();
    }
}

struct MpvOverlaySink<'a> {
    on_error: &'a dyn Fn(&str),
}

impl OverlaySink for MpvOverlaySink<'_> {
    fn push(&mut self, id: i64, data: &str, res_x: u32, res_y: u32, z: i32) {
        let Ok(payload) = CString::new(data) else {
            (self.on_error)("danmaku frame contains a NUL byte; frame dropped");
            return;
        };
        // SAFETY: the CString outlives the call; mpv copies the string.
        unsafe { jfn_mpv_danmaku_overlay(id, payload.as_ptr(), res_x as i32, res_y as i32, z) };
    }

    fn clear(&mut self, id: i64) {
        jfn_mpv_danmaku_overlay_remove(id);
    }
}

fn worker_loop<F>(
    stop: Arc<AtomicBool>,
    comments: Vec<DanmakuComment>,
    style: DanmakuStyle,
    on_error: F,
) where
    F: Fn(&str),
{
    let mut driver = DanmakuDriver::new(comments, style);
    let mut sink = MpvOverlaySink {
        on_error: &on_error,
    };
    let mut had_timeline = true;
    while !stop.load(Ordering::Relaxed) {
        match mpv_property_double(c"time-pos") {
            Some(position) => {
                if !had_timeline {
                    // The worker cleared its overlays while idle; make sure a
                    // fresh timeline starts from a clean state.
                    driver.stop(&mut sink);
                    had_timeline = true;
                }
                let paused = mpv_property_int(c"pause").unwrap_or(0) != 0;
                let (res_x, res_y) = render_resolution();
                driver.tick(position, paused, res_x, res_y, &mut sink);
                thread::sleep(Duration::from_millis(TICK_MILLIS));
            }
            None => {
                // No timeline yet (loading, stopping, between files): never
                // leave comments from the previous position on screen.
                if had_timeline {
                    driver.stop(&mut sink);
                    had_timeline = false;
                }
                thread::sleep(Duration::from_millis(IDLE_TICK_MILLIS));
            }
        }
    }
    driver.stop(&mut sink);
}

/// PlayRes for the overlay. `osd-*` is what libass renders into; fall back to
/// the video size and then to 1080p.
fn render_resolution() -> (u32, u32) {
    if let (Some(width), Some(height)) = (
        mpv_property_int(c"osd-width"),
        mpv_property_int(c"osd-height"),
    ) && width > 0
        && height > 0
    {
        return (width as u32, height as u32);
    }
    if let (Some(width), Some(height)) = (mpv_property_int(c"width"), mpv_property_int(c"height"))
        && width > 0
        && height > 0
    {
        return (width as u32, height as u32);
    }
    (DEFAULT_PLAY_RES_X, DEFAULT_PLAY_RES_Y)
}

fn mpv_property_double(name: &CStr) -> Option<f64> {
    let mut value = 0.0;
    (unsafe { jfn_mpv::api::jfn_mpv_get_property_double(name.as_ptr(), &mut value) } >= 0)
        .then_some(value)
}

fn mpv_property_int(name: &CStr) -> Option<i64> {
    let mut value = 0;
    (unsafe { jfn_mpv::api::jfn_mpv_get_property_int(name.as_ptr(), &mut value) } >= 0)
        .then_some(value)
}
