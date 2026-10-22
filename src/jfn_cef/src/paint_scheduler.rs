use cef::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};

use cef::{
    CefString, Frame, ImplFrame, ImplTask, Task, ThreadId, WrapTask, post_delayed_task, post_task,
    wrap_task,
};

use crate::client::{Inner, now_ns};

const BOOST_MULTIPLIER: i32 = 2;
const INVALIDATE_TICK_LIMIT: i32 = 1000;
const SKIP_PAINTS_AFTER_RESIZE: i32 = 1;

// After each window resize, keep producing compositor frames until
// `CefLayer::noteStableSize` calls `window.__cefStopRaf`.
const JS_PAINT_NUDGE: &str = r#"
(function () {
    console.debug('CEF paint nudge installed');
    var running = false;
    var stop = false;
    function tick() {
        if (stop) {
            stop = false;
            running = false;
            return;
        }
        requestAnimationFrame(tick);
    }
    window.addEventListener('resize', function () {
        stop = false;
        if (!running) {
            running = true;
            requestAnimationFrame(tick);
        }
    });
    window.__cefStopRaf = function () { stop = true; };
})();
"#;

struct PaintState {
    saved_frame_rate: AtomicI32,
    resize_gen: AtomicU64,
    invalidate_running: AtomicBool,
    invalidate_stop: AtomicBool,
    invalidate_tick_count: AtomicI32,
    last_paint_gen: AtomicU64,
    paints_since_resize: AtomicI32,
    pump_paint_count: AtomicI32,
    last_skip_reset_ns: AtomicI64,
}

impl PaintState {
    fn new() -> Self {
        Self {
            saved_frame_rate: AtomicI32::new(0),
            resize_gen: AtomicU64::new(0),
            invalidate_running: AtomicBool::new(false),
            invalidate_stop: AtomicBool::new(false),
            invalidate_tick_count: AtomicI32::new(0),
            last_paint_gen: AtomicU64::new(0),
            paints_since_resize: AtomicI32::new(SKIP_PAINTS_AFTER_RESIZE),
            pump_paint_count: AtomicI32::new(0),
            last_skip_reset_ns: AtomicI64::new(0),
        }
    }

    fn begin_resize(&self) {
        self.resize_gen.fetch_add(1, Ordering::AcqRel);
    }

    fn stop_invalidate_loop(&self) {
        self.invalidate_stop.store(true, Ordering::Release);
    }

    fn start_invalidate_loop(&self) -> bool {
        self.invalidate_stop.store(false, Ordering::Release);
        if self
            .invalidate_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.invalidate_tick_count.store(0, Ordering::Release);
        true
    }

    fn update_boost_saved_frame_rate(&self, target: i32) -> bool {
        if self.saved_frame_rate.load(Ordering::Acquire) == 0 {
            return false;
        }
        self.saved_frame_rate.store(target, Ordering::Release);
        true
    }

    fn note_paint(&self, fps: i32, now: i64, period_ns: i64) -> (bool, bool) {
        let generation = self.resize_gen.load(Ordering::Acquire);
        let previous = self.last_paint_gen.load(Ordering::Acquire);
        if generation != previous {
            self.last_paint_gen.store(generation, Ordering::Release);
            // The first paint can be the monotonic clock's origin (now == 0).
            // It must arm the completion budget even inside the first period.
            // Subsequent resize events remain rate-clamped during a live drag.
            if previous == 0 || now - self.last_skip_reset_ns.load(Ordering::Acquire) >= period_ns {
                self.last_skip_reset_ns.store(now, Ordering::Release);
                self.pump_paint_count
                    .store(if fps > 0 { 1 + fps } else { 0 }, Ordering::Release);
                self.paints_since_resize.store(0, Ordering::Release);
                jfn_logging::log(
                    jfn_logging::CATEGORY_CEF,
                    jfn_logging::LEVEL_DEBUG,
                    &format!("resize paint budget generation={generation} fps={fps} now_ns={now}"),
                );
            }
        }
        let count = self.paints_since_resize.fetch_add(1, Ordering::AcqRel) + 1;
        let budget = self.pump_paint_count.load(Ordering::Acquire);
        (
            count > SKIP_PAINTS_AFTER_RESIZE,
            budget > 0 && count == budget,
        )
    }
}

#[derive(Debug)]
pub(crate) struct PaintMode {
    shared_textures: bool,
}

impl PaintMode {
    pub(crate) fn new(shared_textures: bool) -> Self {
        Self { shared_textures }
    }

    pub(crate) fn shared_textures(&self) -> bool {
        self.shared_textures
    }

    pub(crate) fn make_scheduler(&self) -> PaintScheduler {
        PaintScheduler::new(self.shared_textures)
    }
}

#[derive(Clone)]
pub(crate) struct PaintScheduler {
    mode: Arc<dyn PaintSchedulerMode>,
}

trait PaintSchedulerMode: Send + Sync {
    fn before_resize(&self) {}
    fn after_resize(&self, _scheduler: PaintScheduler, _inner: &Arc<Inner>) {}
    fn before_close(&self) {}
    fn refresh_rate_changed(&self, _target: i32) -> bool {
        false
    }
    fn should_present_paint(&self, _inner: &Inner) -> bool {
        true
    }
    fn kick_task(&self, _scheduler: PaintScheduler, _inner: &Arc<Inner>) {}
    fn tick_task(&self, _scheduler: PaintScheduler, _inner: &Arc<Inner>) {}
}

impl PaintScheduler {
    fn new(shared_textures: bool) -> Self {
        let mode: Arc<dyn PaintSchedulerMode> = if shared_textures {
            Arc::new(ActivePaintScheduler {
                state: PaintState::new(),
            })
        } else {
            Arc::new(PassivePaintScheduler)
        };
        Self { mode }
    }

    pub(crate) fn on_context_created(shared_textures: bool, frame: &Frame) {
        if !shared_textures {
            return;
        }
        let code = CefString::from(JS_PAINT_NUDGE);
        let url_uf = frame.url();
        let url = CefString::from(&url_uf);
        frame.execute_java_script(Some(&code), Some(&url), 0);
    }

    pub(crate) fn during_resize<R>(&self, inner: &Arc<Inner>, resize: impl FnOnce() -> R) -> R {
        self.mode.before_resize();
        let result = resize();
        self.mode.after_resize(self.clone(), inner);
        result
    }

    pub(crate) fn before_close(&self) {
        self.mode.before_close();
    }

    pub(crate) fn refresh_rate_changed(&self, target: i32) -> bool {
        self.mode.refresh_rate_changed(target)
    }

    pub(crate) fn should_present_paint(&self, inner: &Inner) -> bool {
        self.mode.should_present_paint(inner)
    }

    fn kick_task(&self, inner: &Arc<Inner>) {
        self.mode.kick_task(self.clone(), inner);
    }

    fn tick_task(&self, inner: &Arc<Inner>) {
        self.mode.tick_task(self.clone(), inner);
    }
}

struct PassivePaintScheduler;

impl PaintSchedulerMode for PassivePaintScheduler {}

struct ActivePaintScheduler {
    state: PaintState,
}

impl PaintSchedulerMode for ActivePaintScheduler {
    fn before_resize(&self) {
        self.state.begin_resize();
    }

    fn after_resize(&self, scheduler: PaintScheduler, inner: &Arc<Inner>) {
        inner.invalidate_view();
        start_invalidate_loop(scheduler, &self.state, inner);
    }

    fn before_close(&self) {
        self.state.stop_invalidate_loop();
    }

    fn refresh_rate_changed(&self, target: i32) -> bool {
        self.state.update_boost_saved_frame_rate(target)
    }

    fn should_present_paint(&self, inner: &Inner) -> bool {
        active_should_present_paint(&self.state, inner)
    }

    fn kick_task(&self, scheduler: PaintScheduler, inner: &Arc<Inner>) {
        active_kick_apply(scheduler, &self.state, inner);
    }

    fn tick_task(&self, scheduler: PaintScheduler, inner: &Arc<Inner>) {
        active_invalidate_tick(scheduler, &self.state, inner);
    }
}

fn start_invalidate_loop(scheduler: PaintScheduler, state: &PaintState, inner: &Arc<Inner>) {
    if !state.start_invalidate_loop() {
        return;
    }
    let next = Arc::clone(inner);
    let mut task = KickTask::new(scheduler, next);
    let _ = post_task(ThreadId::UI, Some(&mut task));
}

fn active_kick_apply(scheduler: PaintScheduler, state: &PaintState, inner: &Arc<Inner>) {
    // Boost CEF compositor rate while the loop is live — JS rAF ties to
    // compositor rate, so this speeds up convergence to post-resize dims.
    let fps = inner.frame_rate.load(Ordering::Acquire);
    if inner.browser_alive() && fps > 0 && state.saved_frame_rate.load(Ordering::Acquire) == 0 {
        state.saved_frame_rate.store(fps, Ordering::Release);
        inner.set_frame_rate(fps * BOOST_MULTIPLIER);
        jfn_logging::log(
            jfn_logging::CATEGORY_CEF,
            jfn_logging::LEVEL_DEBUG,
            &format!(
                "resize boost start base_fps={fps} boosted_fps={}",
                fps * BOOST_MULTIPLIER
            ),
        );
    }
    active_invalidate_tick(scheduler, state, inner);
}

fn active_invalidate_tick(scheduler: PaintScheduler, state: &PaintState, inner: &Arc<Inner>) {
    if state.invalidate_tick_count.fetch_add(1, Ordering::AcqRel) + 1 > INVALIDATE_TICK_LIMIT {
        state.invalidate_stop.store(true, Ordering::Release);
    }
    if state.invalidate_stop.load(Ordering::Acquire) {
        let saved = state.saved_frame_rate.swap(0, Ordering::AcqRel);
        if inner.browser_alive() && saved > 0 {
            inner.set_frame_rate(saved);
            jfn_logging::log(
                jfn_logging::CATEGORY_CEF,
                jfn_logging::LEVEL_DEBUG,
                &format!(
                    "resize boost stopped restored_fps={saved} ticks={}",
                    state.invalidate_tick_count.load(Ordering::Acquire)
                ),
            );
        }
        state.invalidate_running.store(false, Ordering::Release);
        return;
    }
    if inner.browser_alive() {
        inner.invalidate_view();
        let external_bf = jfn_platform_abi::try_get()
            .and_then(|p| p.cef_host())
            .is_some_and(|h| h.external_begin_frame());
        if external_bf {
            inner.send_external_begin_frame();
        }
    }
    let fps = inner.frame_rate.load(Ordering::Acquire);
    if fps <= 0 {
        state.invalidate_running.store(false, Ordering::Release);
        return;
    }
    // Tick at 4x display refresh so the compositor gets nudged more
    // often than the boosted output rate (2x) — keeps frame production
    // ahead of the present cadence during a resize.
    let tick_hz = fps * 4;
    let delay_ms = ((1000.0 / tick_hz as f64) + 0.5) as i64;
    let delay_ms = delay_ms.max(1);
    let next = Arc::clone(inner);
    let mut task = TickTask::new(scheduler, next);
    let _ = post_delayed_task(ThreadId::UI, Some(&mut task), delay_ms);
}

fn active_should_present_paint(state: &PaintState, inner: &Inner) -> bool {
    let hz = jfn_playback::ingest_driver::jfn_playback_display_hz();
    let period_ns = if hz > 0.0 {
        (1e9 / hz) as i64
    } else {
        16_666_667
    };
    let (present, stop) = state.note_paint(
        inner.frame_rate.load(Ordering::Acquire),
        now_ns(),
        period_ns,
    );
    if stop {
        // Pumped enough frames — signal stop to host Invalidate loop and
        // renderer's rAF loop. Counter remains past pump so subsequent
        // paints don't re-fire.
        state.invalidate_stop.store(true, Ordering::Release);
        inner.exec_js("window.__cefStopRaf && window.__cefStopRaf();");
    }
    present
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_resize_at_clock_origin_stops_after_its_paint_budget() {
        let state = PaintState::new();
        state.begin_resize();
        let period = 1_000_000_000 / 152;
        assert_eq!(state.note_paint(152, 0, period), (false, false));
        for frame in 1..152 {
            assert_eq!(state.note_paint(152, frame * period, period), (true, false));
        }
        assert_eq!(state.note_paint(152, 152 * period, period), (true, true));
        assert_eq!(state.note_paint(152, 153 * period, period), (true, false));
    }

    #[test]
    fn repeated_resize_inside_one_period_does_not_keep_skipping_paints() {
        let state = PaintState::new();
        let period = 1_000_000_000 / 60;
        state.begin_resize();
        assert_eq!(state.note_paint(60, 0, period), (false, false));
        state.begin_resize();
        assert_eq!(state.note_paint(60, period / 2, period), (true, false));
        state.begin_resize();
        assert_eq!(state.note_paint(60, period, period), (false, false));
        assert_eq!(
            state.note_paint(60, period + period / 2, period),
            (true, false)
        );
    }
}

wrap_task! {
    struct KickTask {
        scheduler: PaintScheduler,
        inner: Arc<Inner>,
    }
    impl Task {
        fn execute(&self) {
            self.scheduler.kick_task(&self.inner);
        }
    }
}

wrap_task! {
    struct TickTask {
        scheduler: PaintScheduler,
        inner: Arc<Inner>,
    }
    impl Task {
        fn execute(&self) {
            self.scheduler.tick_task(&self.inner);
        }
    }
}
