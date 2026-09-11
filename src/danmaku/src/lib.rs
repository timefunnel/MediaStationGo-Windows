//! Danmaku (bullet comment) layout and ASS event generation for mpv overlays.
//!
//! mpv's `osd-overlay` with `format=ass-events` has **no timeline**: every line
//! of the `data` string becomes one ASS `Dialogue` Text, and timing dependent
//! tags such as `\move`, `\t` and `\fad` do not animate. So the caller has to
//! recompute positions itself and push a fresh frame every tick, which is what
//! this crate does: given the comments of one episode, the current playback
//! position and the render resolution, produce the ASS event lines for two
//! overlays (z=0 scrolling, z=1 top/bottom).
//!
//! Everything here is pure computation: no mpv, no I/O, so the layout rules can
//! be unit tested without a player.

mod driver;

#[cfg(feature = "mpv")]
mod mpv_worker;

pub use driver::{DanmakuDriver, OverlayIds, OverlaySink, TickOutcome};

#[cfg(feature = "mpv")]
pub use mpv_worker::{DanmakuControls, DanmakuSession};

/// Danmaku modes as used by dandanplay/Bilibili payloads.
pub const MODE_SCROLL: u8 = 1;
pub const MODE_BOTTOM: u8 = 4;
pub const MODE_TOP: u8 = 5;
pub const MODE_REVERSE: u8 = 6;

/// One comment as served by MediaStationGo (`comments[]` of the danmaku payload).
#[derive(Debug, Clone, PartialEq)]
pub struct DanmakuComment {
    pub cid: String,
    /// Seconds on the media timeline (already offset by the server).
    pub time: f64,
    pub mode: u8,
    pub color: u32,
    pub text: String,
}

impl DanmakuComment {
    pub fn new(
        cid: impl Into<String>,
        time: f64,
        mode: u8,
        color: u32,
        text: impl Into<String>,
    ) -> Self {
        Self {
            cid: cid.into(),
            time,
            mode,
            color,
            text: text.into(),
        }
    }

    /// Comments we cannot render faithfully are dropped by the server; anything
    /// unknown that still arrives is treated as a plain scrolling comment.
    fn effective_mode(&self) -> u8 {
        match self.mode {
            MODE_BOTTOM | MODE_TOP | MODE_REVERSE => self.mode,
            _ => MODE_SCROLL,
        }
    }
}

/// Visual settings, mirroring what the web player exposes.
#[derive(Debug, Clone, PartialEq)]
pub struct DanmakuStyle {
    pub font_size: f32,
    /// 0.0 - 1.0, applied as the ASS alpha tag.
    pub opacity: f32,
    /// Seconds a scrolling comment takes to cross the screen.
    pub scroll_seconds: f32,
    /// Seconds a top/bottom comment stays visible.
    pub fixed_seconds: f32,
    /// Fraction of the height that may be covered (0.0 - 1.0).
    pub display_area: f32,
    pub outline: f32,
    pub shadow: f32,
}

impl Default for DanmakuStyle {
    fn default() -> Self {
        // Values chosen to match the web player and the uosc_danmaku defaults
        // that the research settled on (0.85 leaves room for subtitles).
        Self {
            font_size: 50.0,
            opacity: 0.7,
            scroll_seconds: 15.0,
            fixed_seconds: 5.0,
            display_area: 0.85,
            outline: 1.0,
            shadow: 0.0,
        }
    }
}

impl DanmakuStyle {
    fn lane_height(&self) -> f32 {
        self.font_size * 1.25
    }

    fn alpha_tag(&self) -> u32 {
        let clamped = self.opacity.clamp(0.0, 1.0);
        ((1.0 - clamped) * 255.0).round() as u32
    }
}

/// The two overlays the caller should push every tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OverlayFrame {
    /// z=0: scrolling comments (mode 1 and 6).
    pub scroll: String,
    /// z=1: fixed comments (mode 4 and 5).
    pub fixed: String,
    /// How many comments are on screen right now.
    pub visible: usize,
}

impl OverlayFrame {
    pub fn is_empty(&self) -> bool {
        self.visible == 0
    }
}

#[derive(Debug, Clone, Copy)]
struct ScrollOccupant {
    start: f64,
    speed: f32,
    width: f32,
}

#[derive(Debug, Clone, Copy)]
struct Placement {
    /// Vertical position of the comment's anchor, in ASS PlayRes pixels.
    top: f32,
    width: f32,
    speed: f32,
}

/// Assigns lanes and renders frames for one episode's comments.
#[derive(Debug, Default)]
pub struct DanmakuEngine {
    comments: Vec<DanmakuComment>,
    style: DanmakuStyle,
    scroll_lanes: Vec<Option<ScrollOccupant>>,
    fixed_top_lanes: Vec<Option<f64>>,
    fixed_bottom_lanes: Vec<Option<f64>>,
    placements: Vec<Option<Placement>>,
    next_index: usize,
    last_now: f64,
    geometry: Option<(f32, f32)>,
}

impl DanmakuEngine {
    pub fn new(comments: Vec<DanmakuComment>, style: DanmakuStyle) -> Self {
        let mut engine = Self {
            placements: vec![None; comments.len()],
            comments,
            style,
            last_now: f64::NEG_INFINITY,
            ..Self::default()
        };
        engine.comments.sort_by(|a, b| a.time.total_cmp(&b.time));
        engine
    }

    pub fn style(&self) -> &DanmakuStyle {
        &self.style
    }

    pub fn set_style(&mut self, style: DanmakuStyle) {
        self.style = style;
        self.reset();
    }

    pub fn len(&self) -> usize {
        self.comments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.comments.is_empty()
    }

    /// Forget lane assignments; call this on seek or when the geometry changes.
    pub fn reset(&mut self) {
        self.scroll_lanes.clear();
        self.fixed_top_lanes.clear();
        self.fixed_bottom_lanes.clear();
        for slot in &mut self.placements {
            *slot = None;
        }
        self.next_index = 0;
        self.last_now = f64::NEG_INFINITY;
        self.geometry = None;
    }

    fn duration(&self, mode: u8) -> f64 {
        match mode {
            MODE_TOP | MODE_BOTTOM => self.style.fixed_seconds.max(0.5) as f64,
            _ => self.style.scroll_seconds.max(1.0) as f64,
        }
    }

    fn ensure_geometry(&mut self, res_x: f32, res_y: f32) {
        let geometry = (res_x.max(1.0), res_y.max(1.0));
        if self.geometry == Some(geometry) && !self.scroll_lanes.is_empty() {
            return;
        }
        self.geometry = Some(geometry);
        let lane_height = self.style.lane_height().max(1.0);
        let usable = (res_y.max(1.0) * self.style.display_area.clamp(0.1, 1.0)).max(lane_height);
        let lane_count = (usable / lane_height).floor().max(1.0) as usize;
        self.scroll_lanes = vec![None; lane_count];
        self.fixed_top_lanes = vec![None; lane_count];
        self.fixed_bottom_lanes = vec![None; lane_count];
    }

    /// Render the frame for `now` (seconds on the media timeline).
    pub fn frame(&mut self, now: f64, res_x: f32, res_y: f32) -> OverlayFrame {
        // A backwards jump is a seek; a large forward jump means we skipped over
        // comments, so their lane reservations are stale.
        if now + 0.05 < self.last_now || now - self.last_now > 5.0 {
            self.reset();
        }
        self.last_now = now;
        self.ensure_geometry(res_x, res_y);

        let mut frame = OverlayFrame::default();
        self.place_due_comments(now, res_x);

        for index in 0..self.comments.len() {
            let Some(placement) = self.placements[index] else {
                continue;
            };
            let comment = &self.comments[index];
            let mode = comment.effective_mode();
            let duration = self.duration(mode);
            let elapsed = now - comment.time;
            if elapsed < 0.0 || elapsed > duration {
                continue;
            }
            let line = self.render_comment(comment, &placement, elapsed, res_x);
            frame.visible += 1;
            match mode {
                MODE_TOP | MODE_BOTTOM => {
                    frame.fixed.push_str(&line);
                    frame.fixed.push('\n');
                }
                _ => {
                    frame.scroll.push_str(&line);
                    frame.scroll.push('\n');
                }
            }
        }
        frame
    }

    fn place_due_comments(&mut self, now: f64, res_x: f32) {
        while self.next_index < self.comments.len() {
            let comment_time = self.comments[self.next_index].time;
            if comment_time > now {
                break;
            }
            // Comments that already finished before this tick (seek forward or a
            // late start) never need a lane.
            let mode = self.comments[self.next_index].effective_mode();
            if now - comment_time > self.duration(mode) {
                self.placements[self.next_index] = None;
                self.next_index += 1;
                continue;
            }
            let index = self.next_index;
            self.next_index += 1;
            self.placements[index] = self.assign_lane(index, res_x);
        }
    }

    fn assign_lane(&mut self, index: usize, res_x: f32) -> Option<Placement> {
        let comment = self.comments.get(index)?.clone();
        let mode = comment.effective_mode();
        let width = estimate_text_width(&comment.text, self.style.font_size);
        match mode {
            MODE_TOP | MODE_BOTTOM => {
                let end = comment.time + self.duration(mode);
                let lanes = if mode == MODE_TOP {
                    &mut self.fixed_top_lanes
                } else {
                    &mut self.fixed_bottom_lanes
                };
                for (lane, occupant) in lanes.iter_mut().enumerate() {
                    if occupant.is_none_or(|finished_at| comment.time >= finished_at) {
                        *occupant = Some(end);
                        let top = self.fixed_lane_top(lane, mode);
                        return Some(Placement {
                            top,
                            width,
                            speed: 0.0,
                        });
                    }
                }
                // Every lane of that kind is busy: skip rather than overlap.
                None
            }
            _ => {
                let duration = self.duration(mode);
                let distance = res_x.max(1.0) + width;
                let speed = distance / duration as f32;
                let lane = self.pick_scroll_lane(comment.time, speed, width, res_x)?;
                let top = lane as f32 * self.style.lane_height() + self.style.lane_height() * 0.5;
                Some(Placement { top, width, speed })
            }
        }
    }

    /// A lane is free when the previous comment's tail has fully entered the
    /// screen and the new comment cannot catch up with it before it leaves.
    fn pick_scroll_lane(
        &mut self,
        start: f64,
        speed: f32,
        width: f32,
        res_x: f32,
    ) -> Option<usize> {
        let res_x = f64::from(res_x.max(1.0));
        let speed_new = f64::from(speed.max(0.001));
        let is_free = |occupant: &ScrollOccupant| -> bool {
            let speed_prev = f64::from(occupant.speed.max(0.001));
            let tail_entered = occupant.start + f64::from(occupant.width) / speed_prev;
            let prev_leaves = occupant.start + (res_x + f64::from(occupant.width)) / speed_prev;
            // The new comment must still be on screen (or past its entry point)
            // by the time the previous one is gone.
            let no_catch_up = prev_leaves - res_x / speed_new;
            start >= tail_entered.max(no_catch_up)
        };
        let lane = self
            .scroll_lanes
            .iter()
            .position(|occupant| occupant.is_none_or(|value| is_free(&value)))?;
        self.scroll_lanes[lane] = Some(ScrollOccupant {
            start,
            speed,
            width,
        });
        Some(lane)
    }

    fn fixed_lane_top(&self, lane: usize, mode: u8) -> f32 {
        let lane_height = self.style.lane_height();
        if mode == MODE_TOP {
            lane as f32 * lane_height + lane_height * 0.5
        } else {
            let total = self.fixed_bottom_lanes.len().max(1);
            (total - lane) as f32 * lane_height - lane_height * 0.5
        }
    }

    fn render_comment(
        &self,
        comment: &DanmakuComment,
        placement: &Placement,
        elapsed: f64,
        res_x: f32,
    ) -> String {
        let mode = comment.effective_mode();
        let style = &self.style;
        let color = format!("{:06X}", comment.color & 0x00FF_FFFF);
        let bgr = format!("{}{}{}", &color[4..6], &color[2..4], &color[0..2]);
        let prefix = format!(
            "{{\\c&H{bgr}&\\alpha&H{:02X}&\\fs{}\\bord{}\\shad{}\\q2",
            style.alpha_tag(),
            style.font_size.round() as i32,
            style.outline,
            style.shadow
        );
        let text = escape_ass_text(&comment.text);
        match mode {
            MODE_TOP | MODE_BOTTOM => {
                let anchor = if mode == MODE_TOP { "\\an8" } else { "\\an2" };
                format!(
                    "{prefix}{anchor}\\pos({:.0},{:.0})}}{text}",
                    res_x / 2.0,
                    placement.top
                )
            }
            MODE_REVERSE => {
                // Enters from the left edge and travels right.
                let x = -placement.width + placement.speed * elapsed as f32;
                format!("{prefix}\\an7\\pos({:.0},{:.0})}}{text}", x, placement.top)
            }
            _ => {
                let x = res_x - placement.speed * elapsed as f32;
                format!("{prefix}\\an7\\pos({:.0},{:.0})}}{text}", x, placement.top)
            }
        }
    }
}

/// Rough width in pixels: CJK glyphs are about one em, ASCII about half an em.
///
/// libass measures text properly; this estimate only drives lane assignment, and
/// it is deliberately generous so comments are spaced apart rather than merged.
pub fn estimate_text_width(text: &str, font_size: f32) -> f32 {
    let mut units = 0.0f32;
    for ch in text.chars() {
        units += if is_wide(ch) { 1.0 } else { 0.55 };
    }
    (units * font_size).max(font_size)
}

fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF
    )
}

/// Escape comment text for an ASS `Dialogue` Text field.
///
/// `{`/`}` start an override block, a lone `\` starts a tag and raw newlines end
/// the event, so all three have to be neutralised or a comment could restyle the
/// whole overlay.
pub fn escape_ass_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\r' => {}
            '\n' => out.push_str("\\N"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comment(time: f64, mode: u8, text: &str) -> DanmakuComment {
        DanmakuComment::new("1", time, mode, 0xFF_FFFF, text)
    }

    fn engine_with(comments: Vec<DanmakuComment>) -> DanmakuEngine {
        DanmakuEngine::new(comments, DanmakuStyle::default())
    }

    fn positions(frame: &str) -> Vec<(f32, f32)> {
        frame
            .lines()
            .filter_map(|line| {
                let start = line.find("\\pos(")? + 5;
                let end = line[start..].find(')')? + start;
                let mut parts = line[start..end].split(',');
                let x = parts.next()?.parse().ok()?;
                let y = parts.next()?.parse().ok()?;
                Some((x, y))
            })
            .collect()
    }

    #[test]
    fn scrolls_from_right_edge_to_beyond_the_left_edge() {
        let mut engine = engine_with(vec![comment(0.0, MODE_SCROLL, "hello")]);
        let first = engine.frame(0.0, 1920.0, 1080.0);
        let x0 = positions(&first.scroll)[0].0;
        assert!(
            (x0 - 1920.0).abs() < 1.0,
            "comment must start at the right edge, got {x0}"
        );

        let mid = engine.frame(7.5, 1920.0, 1080.0);
        let x_mid = positions(&mid.scroll)[0].0;
        assert!(
            x_mid < x0 && x_mid > 0.0,
            "comment must travel left, got {x_mid}"
        );

        let late = engine.frame(14.9, 1920.0, 1080.0);
        let x_late = positions(&late.scroll)[0].0;
        assert!(x_late < x_mid, "comment must keep travelling, got {x_late}");
    }

    #[test]
    fn reverse_mode_travels_from_left_to_right() {
        let mut engine = engine_with(vec![comment(0.0, MODE_REVERSE, "rev")]);
        let x0 = positions(&engine.frame(0.0, 1920.0, 1080.0).scroll)[0].0;
        let x1 = positions(&engine.frame(5.0, 1920.0, 1080.0).scroll)[0].0;
        assert!(x0 < 0.0, "reverse comment enters from the left, got {x0}");
        assert!(x1 > x0, "reverse comment must travel right, got {x1}");
    }

    #[test]
    fn top_and_bottom_comments_go_to_the_fixed_overlay() {
        let mut engine = engine_with(vec![
            comment(0.0, MODE_TOP, "top"),
            comment(0.0, MODE_BOTTOM, "bottom"),
            comment(0.0, MODE_SCROLL, "scroll"),
        ]);
        let frame = engine.frame(0.5, 1920.0, 1080.0);
        assert_eq!(frame.fixed.lines().count(), 2);
        assert_eq!(frame.scroll.lines().count(), 1);
        assert!(
            frame.fixed.contains("\\an8"),
            "top comment must be top anchored"
        );
        assert!(
            frame.fixed.contains("\\an2"),
            "bottom comment must be bottom anchored"
        );
    }

    #[test]
    fn simultaneous_comments_get_different_lanes() {
        let mut engine = engine_with(vec![
            comment(0.0, MODE_SCROLL, "first"),
            comment(0.1, MODE_SCROLL, "second"),
        ]);
        let frame = engine.frame(1.0, 1920.0, 1080.0);
        let ys: Vec<f32> = positions(&frame.scroll)
            .into_iter()
            .map(|(_, y)| y)
            .collect();
        assert_eq!(ys.len(), 2);
        assert_ne!(ys[0], ys[1], "overlapping comments must not share a lane");
    }

    #[test]
    fn lane_is_reused_once_the_previous_comment_has_cleared() {
        let mut engine = engine_with(vec![
            comment(0.0, MODE_SCROLL, "a"),
            comment(40.0, MODE_SCROLL, "b"),
        ]);
        // First comment is long gone by t=40, so the second one may reuse lane 0.
        let first_lane = engine.frame(0.5, 1920.0, 1080.0);
        let second = engine.frame(40.5, 1920.0, 1080.0);
        assert_eq!(positions(&first_lane.scroll).len(), 1);
        let y0 = positions(&first_lane.scroll)[0].1;
        let y1 = positions(&second.scroll)[0].1;
        assert!(
            (y0 - y1).abs() < 0.01,
            "lane should be reused, y0={y0} y1={y1}"
        );
    }

    #[test]
    fn short_comments_do_not_pile_into_extra_lanes() {
        // A burst of three comments must fill consecutive lanes from the top
        // instead of being scattered down the screen.
        let comments = [0.0, 0.05, 0.1]
            .iter()
            .map(|t| comment(*t, MODE_SCROLL, "x"))
            .collect();
        let mut engine = engine_with(comments);
        let frame = engine.frame(1.0, 1920.0, 1080.0);
        let ys: Vec<f32> = positions(&frame.scroll)
            .into_iter()
            .map(|(_, y)| y)
            .collect();
        assert_eq!(ys.len(), 3);
        assert!(
            ys.iter().all(|y| *y < 200.0),
            "lanes must start from the top: {ys:?}"
        );
        let mut sorted = ys.clone();
        sorted.sort_by(f32::total_cmp);
        assert_eq!(sorted, ys, "lanes must be handed out in order: {ys:?}");
    }

    #[test]
    fn comments_are_hidden_outside_their_lifetime() {
        let mut engine = engine_with(vec![comment(10.0, MODE_SCROLL, "later")]);
        assert_eq!(engine.frame(0.0, 1920.0, 1080.0).visible, 0);
        assert_eq!(engine.frame(10.5, 1920.0, 1080.0).visible, 1);
        assert_eq!(engine.frame(40.0, 1920.0, 1080.0).visible, 0);
    }

    #[test]
    fn seeking_backwards_replays_the_comments() {
        let mut engine = engine_with(vec![comment(5.0, MODE_SCROLL, "seek")]);
        assert_eq!(engine.frame(5.5, 1920.0, 1080.0).visible, 1);
        assert_eq!(engine.frame(30.0, 1920.0, 1080.0).visible, 0);
        assert_eq!(
            engine.frame(5.2, 1920.0, 1080.0).visible,
            1,
            "seek back must replay"
        );
    }

    #[test]
    fn seeking_forward_skips_stale_comments() {
        let comments = (0..5)
            .map(|i| comment(f64::from(i), MODE_SCROLL, "old"))
            .collect();
        let mut engine = engine_with(comments);
        let frame = engine.frame(500.0, 1920.0, 1080.0);
        assert_eq!(
            frame.visible, 0,
            "comments from before a forward seek must not appear"
        );
    }

    #[test]
    fn fixed_comments_without_a_free_lane_are_skipped_not_overlapped() {
        let style = DanmakuStyle {
            display_area: 0.05,
            ..DanmakuStyle::default()
        };
        let comments = (0..6)
            .map(|i| comment(f64::from(i) * 0.1, MODE_TOP, "top"))
            .collect();
        let mut engine = DanmakuEngine::new(comments, style);
        let frame = engine.frame(1.0, 1920.0, 1080.0);
        let ys: Vec<f32> = positions(&frame.fixed)
            .into_iter()
            .map(|(_, y)| y)
            .collect();
        let mut unique = ys.clone();
        unique.sort_by(f32::total_cmp);
        unique.dedup();
        assert_eq!(
            ys.len(),
            unique.len(),
            "two fixed comments must never share a lane: {ys:?}"
        );
    }

    #[test]
    fn escaping_neutralises_override_tags_and_newlines() {
        assert_eq!(escape_ass_text("a{b}c"), "a\\{b\\}c");
        assert_eq!(escape_ass_text("line1\nline2"), "line1\\Nline2");
        assert_eq!(escape_ass_text("back\\slash"), "back\\\\slash");
    }

    #[test]
    fn color_is_written_in_bgr_order_with_alpha() {
        let mut engine = engine_with(vec![DanmakuComment::new(
            "1",
            0.0,
            MODE_SCROLL,
            0x12_34_56,
            "c",
        )]);
        let frame = engine.frame(0.0, 1920.0, 1080.0);
        assert!(
            frame.scroll.contains("\\c&H563412&"),
            "expected BGR colour, got {}",
            frame.scroll
        );
        let expected_alpha = format!(
            "\\alpha&H{:02X}&",
            ((1.0f32 - 0.7f32) * 255.0f32).round() as u32
        );
        assert!(
            frame.scroll.contains(&expected_alpha),
            "missing alpha in {}",
            frame.scroll
        );
    }

    #[test]
    fn text_inside_a_frame_never_opens_an_override_block() {
        let mut engine = engine_with(vec![comment(0.0, MODE_SCROLL, "{\\fs999}bad")]);
        let frame = engine.frame(0.0, 1920.0, 1080.0);
        assert!(
            !frame.scroll.contains("{\\fs999}"),
            "raw override text leaked: {}",
            frame.scroll
        );
        assert!(
            frame.scroll.contains("\\{\\\\fs999\\}bad"),
            "got {}",
            frame.scroll
        );
    }

    #[test]
    fn wide_text_is_estimated_wider_than_ascii() {
        let cjk = estimate_text_width("前方高能", 50.0);
        let ascii = estimate_text_width("abcd", 50.0);
        assert!(cjk > ascii, "cjk={cjk} ascii={ascii}");
    }

    #[test]
    fn lane_count_follows_display_area() {
        let style = DanmakuStyle {
            display_area: 0.25,
            font_size: 100.0,
            ..DanmakuStyle::default()
        };
        let mut engine = DanmakuEngine::new(vec![comment(0.0, MODE_SCROLL, "x")], style);
        engine.frame(0.0, 1920.0, 1000.0);
        // 1000 * 0.25 = 250 usable, lane height 125 -> 2 lanes.
        assert_eq!(engine.scroll_lanes.len(), 2);
    }
}
