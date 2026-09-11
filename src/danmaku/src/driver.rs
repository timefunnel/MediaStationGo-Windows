//! Tick driven driver that turns the layout engine output into overlay pushes.
//!
//! Kept free of mpv types so the state machine (dedupe, pause, stop, episode
//! switch, resolution change) can be unit tested; production passes an
//! [`OverlaySink`] that forwards to `jfn_mpv_danmaku_overlay`.

use crate::{DanmakuComment, DanmakuEngine, DanmakuStyle};

/// Overlay ids: one layer for scrolling comments (z=0), one for top/bottom
/// comments (z=1). Both are custom OSD, so mpv draws them above subtitles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayIds {
    pub scroll: i64,
    pub fixed: i64,
    pub scroll_z: i32,
    pub fixed_z: i32,
}

impl Default for OverlayIds {
    fn default() -> Self {
        Self {
            scroll: 100,
            fixed: 101,
            scroll_z: 0,
            fixed_z: 1,
        }
    }
}

/// Where rendered frames go. Implemented by the mpv binding and by tests.
pub trait OverlaySink {
    /// Push (or replace) the overlay `id` with `data` ASS event lines.
    fn push(&mut self, id: i64, data: &str, res_x: u32, res_y: u32, z: i32);
    /// Remove the overlay `id`.
    fn clear(&mut self, id: i64);
}

/// What the driver did on the last tick; surfaced for diagnostics and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// Overlays were pushed or refreshed.
    Updated,
    /// Nothing changed, so nothing was sent to mpv.
    Unchanged,
    /// Danmaku is off, paused, or there is nothing to draw.
    Idle,
}

#[derive(Debug)]
pub struct DanmakuDriver {
    engine: DanmakuEngine,
    ids: OverlayIds,
    style: DanmakuStyle,
    enabled: bool,
    last: Option<Frame>,
    visible: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct Frame {
    scroll: String,
    fixed: String,
    res_x: u32,
    res_y: u32,
}

impl DanmakuDriver {
    pub fn new(comments: Vec<DanmakuComment>, style: DanmakuStyle) -> Self {
        Self {
            engine: DanmakuEngine::new(comments, style.clone()),
            ids: OverlayIds::default(),
            style,
            enabled: true,
            last: None,
            visible: 0,
        }
    }

    pub fn with_overlay_ids(mut self, ids: OverlayIds) -> Self {
        self.ids = ids;
        self
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Comments on screen at the last tick.
    pub fn visible(&self) -> usize {
        self.visible
    }

    pub fn style(&self) -> &DanmakuStyle {
        &self.style
    }

    pub fn set_style(&mut self, style: DanmakuStyle, sink: &mut impl OverlaySink) {
        self.style = style.clone();
        self.engine.set_style(style);
        self.last = None;
        self.clear_overlays(sink);
    }

    /// Enable or disable rendering. Disabling removes the overlays immediately
    /// so nothing is left on screen.
    pub fn set_enabled(&mut self, enabled: bool, sink: &mut impl OverlaySink) {
        self.enabled = enabled;
        if !enabled {
            self.last = None;
            self.visible = 0;
            self.clear_overlays(sink);
        }
    }

    /// Swap in another episode's comments (or drop them when playback stops).
    pub fn replace_comments(&mut self, comments: Vec<DanmakuComment>, sink: &mut impl OverlaySink) {
        self.engine = DanmakuEngine::new(comments, self.style.clone());
        self.last = None;
        self.visible = 0;
        self.clear_overlays(sink);
    }

    pub fn has_comments(&self) -> bool {
        !self.engine.is_empty()
    }

    /// Advance to `position` seconds. `paused` clears the overlays so a paused
    /// frame never keeps stale comments on screen.
    pub fn tick(
        &mut self,
        position: f64,
        paused: bool,
        res_x: u32,
        res_y: u32,
        sink: &mut impl OverlaySink,
    ) -> TickOutcome {
        if !self.enabled || paused {
            if self.last.is_some() {
                self.last = None;
                self.visible = 0;
                self.clear_overlays(sink);
            }
            return TickOutcome::Idle;
        }
        let res_x = res_x.max(1);
        let res_y = res_y.max(1);
        let frame = self.engine.frame(position, res_x as f32, res_y as f32);
        self.visible = frame.visible;
        let next = Frame {
            scroll: frame.scroll,
            fixed: frame.fixed,
            res_x,
            res_y,
        };
        if self.last.as_ref() == Some(&next) {
            return TickOutcome::Unchanged;
        }
        if next.scroll.is_empty() && next.fixed.is_empty() {
            // Nothing on screen right now: drop the overlays instead of pushing
            // empty strings every tick. The empty frame is deliberately *not*
            // remembered, so repeated idle ticks stay `Idle`.
            let had_overlays = self.last.take().is_some();
            if had_overlays {
                self.clear_overlays(sink);
            }
            return TickOutcome::Idle;
        }
        let previous = self.last.replace(next.clone());
        let had_overlays = previous.is_some();
        if next.scroll.is_empty() {
            if had_overlays {
                sink.clear(self.ids.scroll);
            }
        } else {
            sink.push(
                self.ids.scroll,
                &next.scroll,
                res_x,
                res_y,
                self.ids.scroll_z,
            );
        }
        if next.fixed.is_empty() {
            if had_overlays {
                sink.clear(self.ids.fixed);
            }
        } else {
            sink.push(self.ids.fixed, &next.fixed, res_x, res_y, self.ids.fixed_z);
        }
        TickOutcome::Updated
    }

    /// Remove both overlays; call when playback ends or the player tears down.
    pub fn stop(&mut self, sink: &mut impl OverlaySink) {
        self.last = None;
        self.visible = 0;
        self.clear_overlays(sink);
    }

    fn clear_overlays(&self, sink: &mut impl OverlaySink) {
        sink.clear(self.ids.scroll);
        sink.clear(self.ids.fixed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MODE_SCROLL;

    #[derive(Debug, PartialEq)]
    enum Op {
        Push {
            id: i64,
            data: String,
            res_x: u32,
            res_y: u32,
            z: i32,
        },
        Clear {
            id: i64,
        },
    }

    #[derive(Default)]
    struct Recorder {
        ops: Vec<Op>,
    }

    impl Recorder {
        fn pushes(&self) -> Vec<&Op> {
            self.ops
                .iter()
                .filter(|op| matches!(op, Op::Push { .. }))
                .collect()
        }

        fn last_push(&self, id: i64) -> Option<&Op> {
            self.ops
                .iter()
                .rev()
                .find(|op| matches!(op, Op::Push { id: pushed, .. } if *pushed == id))
        }
    }

    impl OverlaySink for Recorder {
        fn push(&mut self, id: i64, data: &str, res_x: u32, res_y: u32, z: i32) {
            self.ops.push(Op::Push {
                id,
                data: data.to_string(),
                res_x,
                res_y,
                z,
            });
        }

        fn clear(&mut self, id: i64) {
            self.ops.push(Op::Clear { id });
        }
    }

    fn comment(time: f64, mode: u8, text: &str) -> DanmakuComment {
        DanmakuComment::new("1", time, mode, 0xFF_FFFF, text)
    }

    fn driver(comments: Vec<DanmakuComment>) -> DanmakuDriver {
        DanmakuDriver::new(comments, DanmakuStyle::default())
    }

    #[test]
    fn a_tick_pushes_both_overlays_with_their_z_orders() {
        let mut driver = driver(vec![
            comment(0.0, MODE_SCROLL, "scroll"),
            comment(0.0, 5, "top"),
        ]);
        let mut sink = Recorder::default();
        assert_eq!(
            driver.tick(0.5, false, 1920, 1080, &mut sink),
            TickOutcome::Updated
        );
        assert_eq!(sink.pushes().len(), 2);
        assert!(matches!(
            sink.last_push(100),
            Some(Op::Push {
                z: 0,
                res_x: 1920,
                res_y: 1080,
                ..
            })
        ));
        assert!(matches!(sink.last_push(101), Some(Op::Push { z: 1, .. })));
        assert_eq!(driver.visible(), 2);
    }

    #[test]
    fn identical_frames_are_not_repushed() {
        let mut driver = driver(vec![comment(0.0, MODE_SCROLL, "x")]);
        let mut sink = Recorder::default();
        driver.tick(0.5, false, 1920, 1080, &mut sink);
        let first = sink.pushes().len();
        // Same position, same resolution: the rendered lines are identical.
        assert_eq!(
            driver.tick(0.5, false, 1920, 1080, &mut sink),
            TickOutcome::Unchanged
        );
        assert_eq!(sink.pushes().len(), first);
        // A new position moves the comment, so a push happens again.
        assert_eq!(
            driver.tick(1.0, false, 1920, 1080, &mut sink),
            TickOutcome::Updated
        );
        assert!(sink.pushes().len() > first);
    }

    #[test]
    fn resolution_change_forces_a_repush() {
        let mut driver = driver(vec![comment(0.0, MODE_SCROLL, "x")]);
        let mut sink = Recorder::default();
        driver.tick(0.5, false, 1920, 1080, &mut sink);
        let first = sink.pushes().len();
        // Same position but a resized window: PlayRes changes, so mpv must be
        // told even though the ASS text may look identical.
        assert_eq!(
            driver.tick(0.5, false, 1280, 720, &mut sink),
            TickOutcome::Updated
        );
        assert!(sink.pushes().len() > first);
        assert!(matches!(
            sink.last_push(100),
            Some(Op::Push {
                res_x: 1280,
                res_y: 720,
                ..
            })
        ));
    }

    #[test]
    fn pausing_clears_the_overlays_once_and_resuming_pushes_again() {
        let mut driver = driver(vec![comment(0.0, MODE_SCROLL, "x")]);
        let mut sink = Recorder::default();
        driver.tick(0.5, false, 1920, 1080, &mut sink);
        assert_eq!(
            driver.tick(0.5, true, 1920, 1080, &mut sink),
            TickOutcome::Idle
        );
        assert!(sink.ops.contains(&Op::Clear { id: 100 }));
        assert!(sink.ops.contains(&Op::Clear { id: 101 }));
        assert_eq!(driver.visible(), 0);
        let clears = sink
            .ops
            .iter()
            .filter(|op| matches!(op, Op::Clear { .. }))
            .count();
        // Still paused: nothing else is sent.
        assert_eq!(
            driver.tick(0.5, true, 1920, 1080, &mut sink),
            TickOutcome::Idle
        );
        assert_eq!(
            sink.ops
                .iter()
                .filter(|op| matches!(op, Op::Clear { .. }))
                .count(),
            clears
        );
        assert_eq!(
            driver.tick(0.6, false, 1920, 1080, &mut sink),
            TickOutcome::Updated
        );
    }

    #[test]
    fn disabling_stops_rendering_and_clears_immediately() {
        let mut driver = driver(vec![comment(0.0, MODE_SCROLL, "x")]);
        let mut sink = Recorder::default();
        driver.tick(0.5, false, 1920, 1080, &mut sink);
        driver.set_enabled(false, &mut sink);
        assert!(!driver.enabled());
        assert!(sink.ops.contains(&Op::Clear { id: 100 }));
        let before = sink.ops.len();
        assert_eq!(
            driver.tick(1.0, false, 1920, 1080, &mut sink),
            TickOutcome::Idle
        );
        assert_eq!(sink.ops.len(), before, "disabled driver must not push");
    }

    #[test]
    fn stop_clears_and_episode_switch_resets_lanes() {
        let mut driver = driver(vec![comment(0.0, MODE_SCROLL, "old episode")]);
        let mut sink = Recorder::default();
        driver.tick(0.5, false, 1920, 1080, &mut sink);
        driver.replace_comments(vec![comment(0.0, MODE_SCROLL, "new episode")], &mut sink);
        assert!(sink.ops.contains(&Op::Clear { id: 100 }));
        assert!(driver.has_comments());
        driver.tick(0.5, false, 1920, 1080, &mut sink);
        let pushed = match sink.last_push(100) {
            Some(Op::Push { data, .. }) => data.clone(),
            other => panic!("expected a scroll push, got {other:?}"),
        };
        assert!(
            pushed.contains("new episode"),
            "stale comments survived: {pushed}"
        );

        driver.stop(&mut sink);
        assert_eq!(driver.visible(), 0);
        driver.replace_comments(Vec::new(), &mut sink);
        assert!(!driver.has_comments());
    }

    #[test]
    fn no_comments_means_no_pushes() {
        let mut driver = driver(Vec::new());
        let mut sink = Recorder::default();
        assert_eq!(
            driver.tick(1.0, false, 1920, 1080, &mut sink),
            TickOutcome::Idle
        );
        assert!(sink.ops.is_empty());
        assert_eq!(driver.visible(), 0);
    }

    #[test]
    fn gaps_between_comments_clear_rather_than_push_empty_frames() {
        let mut driver = driver(vec![comment(0.0, MODE_SCROLL, "only")]);
        let mut sink = Recorder::default();
        driver.tick(0.5, false, 1920, 1080, &mut sink);
        // Long after the comment left the screen.
        assert_eq!(
            driver.tick(120.0, false, 1920, 1080, &mut sink),
            TickOutcome::Idle
        );
        assert!(sink.ops.contains(&Op::Clear { id: 100 }));
        let pushes = sink.pushes().len();
        assert_eq!(
            driver.tick(121.0, false, 1920, 1080, &mut sink),
            TickOutcome::Idle
        );
        assert_eq!(
            sink.pushes().len(),
            pushes,
            "empty frames must not be pushed"
        );
    }

    #[test]
    fn custom_overlay_ids_are_used() {
        let ids = OverlayIds {
            scroll: 7,
            fixed: 8,
            scroll_z: 5,
            fixed_z: 6,
        };
        let mut driver = driver(vec![comment(0.0, MODE_SCROLL, "x")]).with_overlay_ids(ids);
        let mut sink = Recorder::default();
        driver.tick(0.5, false, 1920, 1080, &mut sink);
        assert!(matches!(sink.last_push(7), Some(Op::Push { z: 5, .. })));
    }
}
