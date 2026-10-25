//! Coordinator: owns the single mutable state machine, worker thread, and
//! sink fanout. Producers post inputs from any thread; the worker drains
//! them in batches, runs transitions, publishes the canonical snapshot,
//! and hands events to registered sinks via the FFI vtable.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use jfn_wake_event::WakeEvent;

use crate::ffi::{ActionSink, EventSink};
use crate::state_machine::PlaybackStateMachine;
use crate::types::*;

#[derive(Debug)]
pub enum Input {
    FileLoaded,
    TracksPreloaded(u64),
    LoadStarting(String),
    StreamLoadStarting {
        item_id: String,
        session_id: u64,
    },
    StreamFailed(jfn_mpv::stream_cb::StreamFailure),
    PauseChanged(bool),
    EndFile {
        reason: EndReason,
        error_message: String,
    },
    SeekingChanged(bool),
    PausedForCache(bool),
    CoreIdle(bool),
    Position(i64),
    MediaType(MediaType),
    VideoFrameAvailable(bool),
    PlaybackRestart,
    Speed(f64),
    Duration(i64),
    Fullscreen {
        fullscreen: bool,
        was_maximized: bool,
    },
    BufferedRanges(Vec<PlaybackBufferedRange>),
    DisplayHz(f64),
    Metadata(MediaMetadata),
    Artwork(String),
    QueueCaps {
        can_go_next: bool,
        can_go_prev: bool,
    },
    Seeked(i64),
}

struct Shared {
    queue: Mutex<VecDeque<Input>>,
    wake: WakeEvent,
    running: AtomicBool,
    snapshot: Mutex<PlaybackSnapshot>,
    event_sinks: Mutex<Vec<EventSink>>,
    action_sinks: Mutex<Vec<ActionSink>>,
    builtin_event_sinks: Mutex<Vec<EventSink>>,
    builtin_action_sinks: Mutex<Vec<ActionSink>>,
}

pub struct PlaybackCoordinator {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct CoordinatorHandle(Arc<Shared>);

impl CoordinatorHandle {
    pub fn enqueue(&self, in_: Input) {
        {
            let mut q = self.0.queue.lock();
            q.push_back(in_);
        }
        self.0.wake.signal();
    }

    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.0.snapshot.lock().clone()
    }
}

impl PlaybackCoordinator {
    /// Returns `None` if the wake eventfd can't be created (fd exhaustion).
    pub fn new() -> Option<Self> {
        Some(Self {
            shared: Arc::new(Shared {
                queue: Mutex::new(VecDeque::new()),
                wake: WakeEvent::new()?,
                running: AtomicBool::new(false),
                snapshot: Mutex::new(PlaybackSnapshot::fresh()),
                event_sinks: Mutex::new(Vec::new()),
                action_sinks: Mutex::new(Vec::new()),
                builtin_event_sinks: Mutex::new(Vec::new()),
                builtin_action_sinks: Mutex::new(Vec::new()),
            }),
            join: None,
        })
    }

    pub fn start(&mut self) {
        if self.shared.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = Arc::clone(&self.shared);
        self.join = Some(thread::spawn(move || worker(shared)));
    }

    pub fn stop(&mut self) {
        if !self.shared.running.swap(false, Ordering::SeqCst) {
            return;
        }
        self.shared.wake.signal();
        if let Some(h) = self.join.take()
            && let Err(e) = h.join()
        {
            eprintln!("[playback] coordinator worker panicked: {e:?}");
        }
    }

    pub fn handle(&self) -> CoordinatorHandle {
        CoordinatorHandle(Arc::clone(&self.shared))
    }

    pub fn enqueue(&self, in_: Input) {
        self.handle().enqueue(in_);
    }

    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.handle().snapshot()
    }

    pub fn add_event_sink(&self, sink: EventSink) {
        self.shared.event_sinks.lock().push(sink);
    }

    pub fn add_action_sink(&self, sink: ActionSink) {
        self.shared.action_sinks.lock().push(sink);
    }

    pub fn add_builtin_event_sink(&self, sink: EventSink) {
        self.shared.builtin_event_sinks.lock().push(sink);
    }

    pub fn add_builtin_action_sink(&self, sink: ActionSink) {
        self.shared.builtin_action_sinks.lock().push(sink);
    }
}

impl Drop for PlaybackCoordinator {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker(shared: Arc<Shared>) {
    let mut sm = PlaybackStateMachine::new();
    while shared.running.load(Ordering::Relaxed) {
        let work: VecDeque<Input> = {
            let mut q = shared.queue.lock();
            std::mem::take(&mut *q)
        };

        if work.is_empty() {
            shared.wake.wait();
            shared.wake.drain();
            continue;
        }

        let mut events: Vec<PlaybackEvent> = Vec::new();
        let mut actions: Vec<PlaybackAction> = Vec::new();
        for input in work {
            if apply(&mut sm, input, &mut events) {
                // Drop queued play, but always release an outstanding hook.
                actions.retain(|a| a.kind == PlaybackActionKind::PreparePendingTracks);
            }
            actions.extend(sm.consume_actions());
        }

        let snap = sm.snapshot();
        for e in &mut events {
            e.snapshot = snap.clone();
        }
        {
            let mut s = shared.snapshot.lock();
            *s = snap;
        }

        // Sinks: dispatched in registration order. Each closure must
        // not block; sinks own their own queue + consumer thread.
        let event_sinks = shared.event_sinks.lock();
        for sink in event_sinks.iter() {
            for e in &events {
                sink(e);
            }
        }
        let action_sinks = shared.action_sinks.lock();
        for sink in action_sinks.iter() {
            for a in &actions {
                sink(a);
            }
        }
        let builtin_event_sinks = shared.builtin_event_sinks.lock();
        for sink in builtin_event_sinks.iter() {
            for e in &events {
                sink(e);
            }
        }
        let builtin_action_sinks = shared.builtin_action_sinks.lock();
        for sink in builtin_action_sinks.iter() {
            for a in &actions {
                sink(a);
            }
        }
    }
}

fn apply(sm: &mut PlaybackStateMachine, input: Input, out: &mut Vec<PlaybackEvent>) -> bool {
    let diagnostic = diagnostic_input(&input);
    // Terminal transitions clear identity before their events are delivered.
    let previous_item_id = sm.item_id().to_owned();
    let mut emitted = match input {
        Input::FileLoaded => sm.on_file_loaded(),
        Input::TracksPreloaded(id) => sm.on_tracks_preloaded(id),
        Input::LoadStarting(id) => sm.on_load_starting(id),
        Input::StreamLoadStarting {
            item_id,
            session_id,
        } => sm.on_stream_load_starting(item_id, session_id),
        Input::StreamFailed(failure) => sm.on_stream_failed(failure),
        Input::PauseChanged(paused) => sm.on_pause_changed(paused),
        Input::EndFile {
            reason,
            error_message,
        } => sm.on_end_file(reason, error_message),
        Input::SeekingChanged(seeking) => sm.on_seeking_changed(seeking),
        Input::PausedForCache(pfc) => sm.on_paused_for_cache(pfc),
        Input::CoreIdle(ci) => sm.on_core_idle(ci),
        Input::Position(p) => sm.on_position(p),
        Input::MediaType(t) => sm.on_media_type(t),
        Input::VideoFrameAvailable(a) => sm.on_video_frame_available(a),
        Input::PlaybackRestart => sm.on_playback_restart(),
        Input::Speed(r) => sm.on_speed(r),
        Input::Duration(d) => sm.on_duration(d),
        Input::Fullscreen {
            fullscreen,
            was_maximized,
        } => sm.on_fullscreen(fullscreen, was_maximized),
        Input::BufferedRanges(r) => sm.on_buffered_ranges(r),
        Input::DisplayHz(h) => sm.on_display_hz(h),
        Input::Metadata(m) => {
            // Route media_type through the SM so snapshot.media_type
            // tracks metadata changes (idle inhibit reads it).
            let mut events = sm.on_media_type(m.media_type);
            let mut ev = PlaybackEvent::new(PlaybackEventKind::MetadataChanged);
            ev.metadata = m;
            events.push(ev);
            events
        }
        Input::Artwork(uri) => {
            let mut ev = PlaybackEvent::new(PlaybackEventKind::ArtworkChanged);
            ev.artwork_uri = uri;
            vec![ev]
        }
        Input::QueueCaps {
            can_go_next,
            can_go_prev,
        } => {
            let mut ev = PlaybackEvent::new(PlaybackEventKind::QueueCapsChanged);
            ev.can_go_next = can_go_next;
            ev.can_go_prev = can_go_prev;
            vec![ev]
        }
        Input::Seeked(p) => {
            let mut events = sm.on_position(p);
            events.push(PlaybackEvent::new(PlaybackEventKind::Seeked));
            events
        }
    };
    if let Some(input) = diagnostic {
        let snapshot = sm.snapshot();
        let events = emitted
            .iter()
            .map(|event| format!("{:?}", event.kind))
            .collect::<Vec<_>>()
            .join(",");
        tracing::debug!(
            target: "Playback",
            "state input={input} phase={:?} media={:?} buffering={} seeking={} emitted=[{events}]",
            snapshot.phase,
            snapshot.media_type,
            snapshot.buffering,
            snapshot.seeking
        );
    }
    let load_failed = emitted
        .iter()
        .any(|event| event.kind == PlaybackEventKind::Error);
    // Capture identity per input: a later load in this batch must not relabel
    // an old episode's position or terminal event as belonging to the new one.
    let terminal = emitted.iter().any(|event| {
        matches!(
            event.kind,
            PlaybackEventKind::Finished | PlaybackEventKind::Canceled | PlaybackEventKind::Error
        )
    });
    let item_id = if terminal {
        previous_item_id
    } else {
        sm.item_id().to_owned()
    };
    for event in &mut emitted {
        event.item_id.clone_from(&item_id);
    }
    out.append(&mut emitted);
    load_failed
}

fn diagnostic_input(input: &Input) -> Option<String> {
    match input {
        Input::FileLoaded => Some("file-loaded".to_string()),
        Input::TracksPreloaded(_) => Some("tracks-preloaded".to_string()),
        Input::LoadStarting(_) => Some("load-starting".to_string()),
        Input::StreamLoadStarting { session_id, .. } => {
            Some(format!("stream-load-starting session_id={session_id}"))
        }
        Input::StreamFailed(failure) => Some(format!(
            "stream-failed session_id={} cause={}",
            failure.session_id, failure.error
        )),
        Input::PauseChanged(paused) => Some(format!("pause={paused}")),
        Input::EndFile { reason, .. } => Some(format!("end-file={reason:?}")),
        Input::PausedForCache(paused) => Some(format!("paused-for-cache={paused}")),
        Input::CoreIdle(idle) => Some(format!("core-idle={idle}")),
        Input::MediaType(media_type) => Some(format!("media-type={media_type:?}")),
        Input::PlaybackRestart => Some("playback-restart".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn snapshot_starts_fresh() {
        let coord = PlaybackCoordinator::new().unwrap();
        let s = coord.snapshot();
        assert_eq!(s.presence, PlayerPresence::None);
        assert_eq!(s.phase, PlaybackPhase::Stopped);
        assert_eq!(s.rate, 1.0);
    }

    #[test]
    fn batched_events_keep_the_episode_that_produced_them() {
        let mut coord = PlaybackCoordinator::new().unwrap();
        let (tx, rx) = mpsc::channel();
        coord.add_event_sink(Box::new(move |event| {
            tx.send((event.kind, event.item_id.clone())).unwrap();
        }));
        // Queue both episodes before starting to force one delivery batch.
        for (position, item_id) in [(1_000_000, "episode-old"), (2_000_000, "episode-next")] {
            coord.enqueue(Input::LoadStarting(item_id.to_string()));
            coord.enqueue(Input::Position(position));
        }
        coord.start();
        let events = (0..4)
            .map(|_| rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap())
            .collect::<Vec<_>>();
        coord.stop();
        assert_eq!(
            events,
            vec![
                (PlaybackEventKind::TrackLoaded, "episode-old".to_string()),
                (
                    PlaybackEventKind::PositionChanged,
                    "episode-old".to_string()
                ),
                (PlaybackEventKind::TrackLoaded, "episode-next".to_string()),
                (
                    PlaybackEventKind::PositionChanged,
                    "episode-next".to_string()
                ),
            ]
        );
    }

    #[test]
    fn terminal_events_keep_identity_after_the_state_clears_it() {
        for (reason, kind) in [
            (EndReason::Eof, PlaybackEventKind::Finished),
            (EndReason::Canceled, PlaybackEventKind::Canceled),
            (EndReason::Error, PlaybackEventKind::Error),
        ] {
            let mut sm = PlaybackStateMachine::new();
            let mut events = Vec::new();
            apply(
                &mut sm,
                Input::LoadStarting("episode".to_string()),
                &mut events,
            );
            apply(&mut sm, Input::FileLoaded, &mut events);
            events.clear();
            apply(
                &mut sm,
                Input::EndFile {
                    reason,
                    error_message: String::new(),
                },
                &mut events,
            );
            let terminal = events.iter().find(|event| event.kind == kind).unwrap();
            assert_eq!(terminal.item_id, "episode");
            assert_eq!(sm.item_id(), "");
        }
    }

    #[test]
    fn preloaded_prepares_subtitle_before_builtin_track_selection_and_hook_release() {
        let mut coord = PlaybackCoordinator::new().unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let prepared = Arc::clone(&order);
        coord.add_action_sink(Box::new(move |_action| {
            prepared.lock().push("prepare-subtitle");
        }));
        let selected = Arc::clone(&order);
        let (tx, rx) = mpsc::sync_channel(1);
        coord.add_builtin_action_sink(Box::new(move |action| {
            assert_eq!(action.kind, PlaybackActionKind::PreparePendingTracks);
            assert_eq!(action.hook_id, 42);
            selected.lock().push("select-and-play");
            tx.send(()).expect("test receiver should be alive");
        }));
        coord.start();
        coord.enqueue(Input::TracksPreloaded(42));
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("preloaded action should complete");
        assert_eq!(*order.lock(), vec!["prepare-subtitle", "select-and-play"]);
        coord.stop();
    }

    #[test]
    fn stream_failure_discards_batched_play_actions_only_for_the_active_session() {
        for (failed_session, expected_actions) in [(1, 2), (2, 1)] {
            let mut coord = PlaybackCoordinator::new().unwrap();
            let actions = Arc::new(Mutex::new(Vec::new()));
            let recorded_actions = Arc::clone(&actions);
            coord.add_builtin_action_sink(Box::new(move |action| {
                recorded_actions.lock().push(action.kind);
            }));
            let errors = Arc::new(Mutex::new(Vec::new()));
            let recorded_errors = Arc::clone(&errors);
            let (tx, rx) = mpsc::channel();
            coord.add_event_sink(Box::new(move |event| {
                if let Some(failure) = &event.stream_failure {
                    recorded_errors.lock().push(failure.session_id);
                }
                tx.send(()).expect("test receiver should be alive");
            }));
            // Queue before starting so FILE_LOADED and the error share a batch.
            coord.enqueue(Input::StreamLoadStarting {
                item_id: "movie".to_string(),
                session_id: 2,
            });
            coord.enqueue(Input::TracksPreloaded(42));
            coord.enqueue(Input::FileLoaded);
            coord.enqueue(Input::StreamFailed(jfn_mpv::stream_cb::StreamFailure {
                session_id: failed_session,
                phase: "seek",
                offset: 1000,
                error: jfn_mpv::stream_cb::StreamError::Http {
                    status: 403,
                    message: "115 pmt 3-2".to_string(),
                },
            }));
            coord.start();
            rx.recv_timeout(std::time::Duration::from_secs(2))
                .expect("batch should complete");
            coord.stop();
            assert_eq!(actions.lock().len(), expected_actions);
            assert_eq!(actions.lock()[0], PlaybackActionKind::PreparePendingTracks);
            assert_eq!(errors.lock().len(), usize::from(failed_session == 2));
        }
    }

    #[test]
    fn worker_updates_snapshot_after_input() {
        let mut coord = PlaybackCoordinator::new().unwrap();
        coord.start();
        // Register a sink BEFORE enqueuing so the first dispatched batch
        // signals the channel. Sinks fire on the worker thread after the
        // snapshot is published, so receiving = snapshot is up-to-date.
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        coord.add_event_sink(Box::new(move |_ev| {
            let _ = tx.try_send(());
        }));
        coord.enqueue(Input::FileLoaded);
        rx.recv().expect("worker never published an event");
        let s = coord.snapshot();
        assert_eq!(s.presence, PlayerPresence::Present);
        assert_eq!(s.phase, PlaybackPhase::Starting);
        coord.stop();
    }
}
