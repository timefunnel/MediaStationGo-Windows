//! mpv backed danmaku session (feature `mpv`).
//!
//! A normal ASS subtitle track owns the animation timeline, so libass advances
//! scrolling comments together with video presentation. The worker only reacts
//! to UI changes; it never submits a full overlay for every display frame.

use crate::{DanmakuComment, DanmakuEngine, DanmakuStyle};
use jfn_mpv::api::{
    jfn_mpv_danmaku_subtitle_add_checked, jfn_mpv_danmaku_subtitle_enable_ass_tags_checked,
    jfn_mpv_danmaku_subtitle_reload_checked, jfn_mpv_danmaku_subtitle_remove_checked,
    jfn_mpv_danmaku_subtitle_restore_ass_tags_checked,
    jfn_mpv_danmaku_subtitle_set_enabled_checked,
};
use std::ffi::CString;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use tempfile::TempPath;

const ASS_PLAY_RES_X: f32 = 1920.0;
const ASS_PLAY_RES_Y: f32 = 1080.0;
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The player UI's danmaku visibility and style, shared with the worker thread.
///
/// Cheap to clone: the atomic switch and the style snapshot live behind `Arc`s,
/// so the UI can update them without restarting the worker.
#[derive(Clone, Debug)]
pub struct DanmakuControls {
    enabled: Arc<AtomicBool>,
    style: Arc<Mutex<DanmakuStyle>>,
}

impl DanmakuControls {
    pub fn new(enabled: bool) -> Self {
        Self::new_with_style(enabled, DanmakuStyle::default())
    }

    pub fn new_with_style(enabled: bool, style: DanmakuStyle) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            style: Arc::new(Mutex::new(style)),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Switching off hides the secondary ASS track; switching back on restores
    /// it without rebuilding the track.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn style(&self) -> DanmakuStyle {
        self.style
            .lock()
            .expect("danmaku style lock poisoned")
            .clone()
    }

    /// Apply a new style to the running worker. It reloads the ASS track once.
    pub fn set_style(&self, style: DanmakuStyle) {
        *self.style.lock().expect("danmaku style lock poisoned") = style;
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
    /// Spawn the worker. `on_error` receives human readable ASS generation,
    /// subtitle-track, and cleanup failures so the caller can log them in its
    /// own category instead of the failure being swallowed.
    ///
    /// `controls` is shared with the caller, so the player UI can switch danmaku
    /// off (and on again) while this session runs.
    pub fn start(
        comments: Vec<DanmakuComment>,
        controls: DanmakuControls,
        on_error: impl Fn(&str) + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker_controls = controls.clone();
        let worker = thread::Builder::new()
            .name("danmaku-overlay".to_string())
            .spawn(move || worker_loop(worker_stop, worker_controls, comments, on_error))?;
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

    /// Stop without waiting: the thread finishes its control pass, removes the
    /// auxiliary subtitle track, and exits.
    pub fn detach(&mut self) {
        self.signal_stop();
        self.worker.take();
    }

    /// Stop and wait, so this session's auxiliary track is removed before a
    /// newer playback session starts.
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

struct AssDanmakuTrack {
    path: TempPath,
    id: i64,
    previous_secondary_ass_override: CString,
}

impl AssDanmakuTrack {
    fn create(comments: &[DanmakuComment], style: &DanmakuStyle) -> io::Result<Self> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(render_ass_track(comments, style).as_bytes())?;
        file.flush()?;
        let path = file.into_temp_path();
        let previous_secondary_ass_override =
            jfn_mpv_danmaku_subtitle_enable_ass_tags_checked().map_err(mpv_error_to_io)?;
        let id = match jfn_mpv_danmaku_subtitle_add_checked(&path_as_cstring(&path)?) {
            Ok(id) => id,
            Err(error) => {
                let _ = jfn_mpv_danmaku_subtitle_restore_ass_tags_checked(
                    &previous_secondary_ass_override,
                );
                return Err(mpv_error_to_io(error));
            }
        };
        Ok(Self {
            path,
            id,
            previous_secondary_ass_override,
        })
    }

    fn reload(&mut self, comments: &[DanmakuComment], style: &DanmakuStyle) -> io::Result<()> {
        std::fs::write(&self.path, render_ass_track(comments, style))?;
        self.id = jfn_mpv_danmaku_subtitle_reload_checked(self.id, &path_as_cstring(&self.path)?)
            .map_err(mpv_error_to_io)?;
        Ok(())
    }

    fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        jfn_mpv_danmaku_subtitle_set_enabled_checked(self.id, enabled).map_err(mpv_error_to_io)
    }

    fn remove(self) -> io::Result<()> {
        let remove = jfn_mpv_danmaku_subtitle_remove_checked(self.id);
        let restore = jfn_mpv_danmaku_subtitle_restore_ass_tags_checked(
            &self.previous_secondary_ass_override,
        );
        remove.map_err(mpv_error_to_io)?;
        restore.map_err(mpv_error_to_io)
    }
}

fn render_ass_track(comments: &[DanmakuComment], style: &DanmakuStyle) -> String {
    DanmakuEngine::new(comments.to_vec(), style.clone()).ass_track(ASS_PLAY_RES_X, ASS_PLAY_RES_Y)
}

fn path_as_cstring(path: &TempPath) -> io::Result<CString> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "danmaku ASS path contains NUL"))
}

fn mpv_error_to_io(error: jfn_mpv::Error) -> io::Error {
    io::Error::other(error)
}

fn worker_loop<F>(
    stop: Arc<AtomicBool>,
    controls: DanmakuControls,
    comments: Vec<DanmakuComment>,
    on_error: F,
) where
    F: Fn(&str),
{
    let mut applied_style = controls.style();
    let mut track = match AssDanmakuTrack::create(&comments, &applied_style) {
        Ok(track) => track,
        Err(error) => {
            on_error(&format!("danmaku ASS track could not be loaded: {error}"));
            return;
        }
    };
    let mut applied_enabled = true;
    if !controls.enabled() {
        if let Err(error) = track.set_enabled(false) {
            on_error(&format!("danmaku ASS track could not be hidden: {error}"));
        } else {
            applied_enabled = false;
        }
    }
    while !stop.load(Ordering::Relaxed) {
        let next_style = controls.style();
        if next_style != applied_style {
            match track.reload(&comments, &next_style) {
                Ok(()) => {
                    applied_style = next_style;
                    if !applied_enabled {
                        if let Err(error) = track.set_enabled(false) {
                            on_error(&format!(
                                "danmaku ASS track could not be re-hidden: {error}"
                            ));
                        }
                    }
                }
                Err(error) => on_error(&format!("danmaku ASS style reload failed: {error}")),
            }
        }
        let enabled = controls.enabled();
        if enabled != applied_enabled {
            match track.set_enabled(enabled) {
                Ok(()) => applied_enabled = enabled,
                Err(error) => on_error(&format!("danmaku ASS visibility change failed: {error}")),
            }
        }
        thread::sleep(CONTROL_POLL_INTERVAL);
    }
    if let Err(error) = track.remove() {
        on_error(&format!("danmaku ASS track could not be removed: {error}"));
    }
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
    fn controls_style_is_shared_with_the_worker() {
        let controls = DanmakuControls::default();
        assert_eq!(controls.style(), DanmakuStyle::default());
        let style = DanmakuStyle {
            font_size: 72.0,
            display_area: 0.6,
            ..DanmakuStyle::default()
        };
        controls.set_style(style.clone());
        assert_eq!(controls.style(), style);
    }
}
