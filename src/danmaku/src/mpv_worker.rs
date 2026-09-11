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

/// The player UI's danmaku switch, shared with the worker thread.
///
/// Cheap to clone: the flag lives behind an `Arc`, so the UI can flip it while
/// the worker is mid-tick without any locking.
#[derive(Clone, Debug)]
pub struct DanmakuControls {
    enabled: Arc<AtomicBool>,
}

impl DanmakuControls {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Switching off takes effect on the next tick, which clears whatever is on
    /// screen; switching back on redraws from the current position.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
}

impl Default for DanmakuControls {
    fn default() -> Self {
        Self::new(true)
    }
}

/// A running danmaku worker for one playback item.
pub struct DanmakuSession {
    stop: Arc<AtomicBool>,
    controls: DanmakuControls,
    worker: Option<JoinHandle<()>>,
}

impl DanmakuSession {
    /// Spawn the worker. `on_error` receives human readable problems (a frame
    /// containing a NUL byte, a rejected overlay command) so the caller can log
    /// them in its own logging category instead of the failure being swallowed.
    ///
    /// `controls` is shared with the caller, so the player UI can switch danmaku
    /// off (and on again) while this session runs.
    pub fn start(
        comments: Vec<DanmakuComment>,
        style: DanmakuStyle,
        controls: DanmakuControls,
        on_error: impl Fn(&str) + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_controls = controls.clone();
        let worker = thread::Builder::new()
            .name("danmaku-overlay".to_string())
            .spawn(move || worker_loop(worker_stop, worker_controls, comments, style, on_error))?;
        Ok(Self {
            stop,
            controls,
            worker: Some(worker),
        })
    }

    /// The switch this session reads; callers keep a clone to control it.
    pub fn controls(&self) -> DanmakuControls {
        self.controls.clone()
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

/// What the worker should do on one tick.
///
/// Kept separate from the loop so the three cases (draw / no timeline / switched
/// off) can be tested without an mpv handle.
#[derive(Debug, Clone, Copy, PartialEq)]
enum TickAction {
    /// Draw the frame for this position on the media timeline.
    Draw(f64),
    /// No timeline yet (loading, stopping, between files): clear what is shown.
    Clear,
    /// Danmaku is switched off by the player UI: clear once, then stay quiet.
    Off,
}

fn tick_action(enabled: bool, position: Option<f64>) -> TickAction {
    if !enabled {
        return TickAction::Off;
    }
    match position {
        Some(position) => TickAction::Draw(position),
        None => TickAction::Clear,
    }
}

fn worker_loop<F>(
    stop: Arc<AtomicBool>,
    controls: DanmakuControls,
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
    // Whether anything of ours may still be on screen. mpv keeps an overlay
    // until it is removed, so both "no timeline" and "switched off" have to clear
    // it exactly once instead of every tick.
    let mut overlays_live = false;
    while !stop.load(Ordering::Relaxed) {
        match tick_action(controls.enabled(), mpv_property_double(c"time-pos")) {
            TickAction::Draw(position) => {
                let paused = mpv_property_int(c"pause").unwrap_or(0) != 0;
                let (res_x, res_y) = render_resolution();
                driver.tick(position, paused, res_x, res_y, &mut sink);
                overlays_live = true;
                thread::sleep(Duration::from_millis(TICK_MILLIS));
            }
            TickAction::Clear | TickAction::Off => {
                if overlays_live {
                    // Never leave comments from the previous position (or from
                    // before the switch was flipped) on screen.
                    driver.stop(&mut sink);
                    overlays_live = false;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_start_enabled_and_can_be_flipped_from_another_handle() {
        let controls = DanmakuControls::default();
        assert!(controls.enabled());
        let ui = controls.clone();
        ui.set_enabled(false);
        assert!(!controls.enabled(), "the worker sees the UI's switch");
        controls.set_enabled(true);
        assert!(ui.enabled());
    }

    #[test]
    fn tick_action_draws_clears_or_stays_off() {
        assert_eq!(tick_action(true, Some(12.5)), TickAction::Draw(12.5));
        assert_eq!(tick_action(true, None), TickAction::Clear);
        // Switched off wins over a live timeline: this is what removes the
        // overlay from the screen when the user turns danmaku off.
        assert_eq!(tick_action(false, Some(12.5)), TickAction::Off);
        assert_eq!(tick_action(false, None), TickAction::Off);
    }
}
