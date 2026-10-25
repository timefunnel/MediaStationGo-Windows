//! Pure deterministic state machine. No threads, globals, or I/O.

use crate::types::*;

pub struct PlaybackStateMachine {
    s: PlaybackSnapshot,
    pending_load: bool,
    pause_requested: bool,
    paused_for_cache: bool,
    core_idle: bool,
    frame_available: bool,
    last_known_item_id: String,
    pending_actions: Vec<PlaybackAction>,
    stream_session: Option<u64>,
    load_failed: bool,
}

impl Default for PlaybackStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackStateMachine {
    pub fn new() -> Self {
        Self {
            s: PlaybackSnapshot::fresh(),
            pending_load: false,
            pause_requested: false,
            paused_for_cache: false,
            core_idle: false,
            frame_available: false,
            last_known_item_id: String::new(),
            pending_actions: Vec::new(),
            stream_session: None,
            load_failed: false,
        }
    }

    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.s.clone()
    }

    pub fn item_id(&self) -> &str {
        &self.last_known_item_id
    }

    pub fn consume_actions(&mut self) -> Vec<PlaybackAction> {
        std::mem::take(&mut self.pending_actions)
    }

    pub fn on_tracks_preloaded(&mut self, hook_id: u64) -> Vec<PlaybackEvent> {
        // A native hook must always be released, including during cancellation.
        self.pending_actions.push(PlaybackAction {
            kind: PlaybackActionKind::PreparePendingTracks,
            hook_id,
        });
        vec![]
    }

    pub fn on_file_loaded(&mut self) -> Vec<PlaybackEvent> {
        if self.load_failed {
            return vec![];
        }
        self.s.presence = PlayerPresence::Present;
        self.s.phase = PlaybackPhase::Starting;
        self.s.seeking = false;
        self.s.buffering = self.paused_for_cache || self.core_idle;
        // variant_switch_pending intentionally NOT cleared — mpv loads
        // paused after FILE_LOADED and the flag must span until the new
        // variant's first frame promotes to Playing.
        self.pending_load = false;
        self.pause_requested = false;
        self.frame_available = false;
        self.pending_actions.push(PlaybackAction {
            kind: PlaybackActionKind::ApplyPendingTrackSelectionAndPlay,
            hook_id: 0,
        });
        vec![PlaybackEvent::new(PlaybackEventKind::TrackLoaded)]
    }

    pub fn on_load_starting(&mut self, item_id: String) -> Vec<PlaybackEvent> {
        self.stream_session = None;
        self.load_failed = false;
        self.pending_load = true;
        self.s.presence = PlayerPresence::Present;
        self.s.phase = PlaybackPhase::Starting;
        self.s.seeking = false;
        self.s.buffering = self.paused_for_cache || self.core_idle;
        self.s.variant_switch_pending = !item_id.is_empty() && item_id == self.last_known_item_id;
        if !item_id.is_empty() {
            self.last_known_item_id = item_id;
        }
        self.pause_requested = false;
        self.frame_available = false;
        vec![PlaybackEvent::new(PlaybackEventKind::TrackLoaded)]
    }

    pub fn on_stream_load_starting(
        &mut self,
        item_id: String,
        session_id: u64,
    ) -> Vec<PlaybackEvent> {
        let events = self.on_load_starting(item_id);
        self.stream_session = Some(session_id);
        events
    }

    pub fn on_stream_failed(
        &mut self,
        failure: jfn_mpv::stream_cb::StreamFailure,
    ) -> Vec<PlaybackEvent> {
        if self.load_failed || self.stream_session != Some(failure.session_id) {
            return vec![];
        }
        let mut events = self.on_end_file(EndReason::Error, failure.user_message());
        for event in &mut events {
            if event.kind == PlaybackEventKind::Error {
                event.stream_failure = Some(failure.clone());
            }
        }
        // mpv can report EOF or FILE_LOADED after a callback has already failed.
        // Keep the terminal error until the next explicit load starts.
        self.load_failed = true;
        self.pending_actions.clear();
        events
    }

    pub fn on_pause_changed(&mut self, paused: bool) -> Vec<PlaybackEvent> {
        if self.s.presence == PlayerPresence::None {
            return vec![];
        }
        if self.s.phase == PlaybackPhase::Stopped {
            return vec![];
        }
        self.pause_requested = !paused;

        if paused {
            if self.s.phase == PlaybackPhase::Paused {
                return vec![];
            }
            let mut out = vec![];
            if self.s.buffering {
                self.s.buffering = false;
                let mut buffering = PlaybackEvent::new(PlaybackEventKind::BufferingChanged);
                buffering.flag = false;
                out.push(buffering);
            }
            self.s.phase = PlaybackPhase::Paused;
            out.push(PlaybackEvent::new(PlaybackEventKind::Paused));
            return out;
        }

        if self.s.phase == PlaybackPhase::Playing {
            return vec![];
        }
        if self.s.phase == PlaybackPhase::Paused {
            // The initial native pause can arrive after FILE_LOADED. Resuming
            // that pause still needs a first video frame, just like Starting.
            if !ready_to_play(self.s.media_type, self.frame_available) {
                self.s.phase = PlaybackPhase::Starting;
                return vec![];
            }
            let mut out = vec![];
            transition_to_playing(&mut self.s, &mut out);
            return out;
        }
        if self.s.phase == PlaybackPhase::Starting {
            if self.s.buffering {
                return vec![];
            }
            if !ready_to_play(self.s.media_type, self.frame_available) {
                return vec![];
            }
            let mut out = vec![];
            transition_to_playing(&mut self.s, &mut out);
            return out;
        }
        vec![]
    }

    pub fn on_end_file(&mut self, reason: EndReason, error_message: String) -> Vec<PlaybackEvent> {
        if self.load_failed {
            return vec![];
        }
        let mut out = vec![];

        if self.s.seeking {
            self.s.seeking = false;
            let mut e = PlaybackEvent::new(PlaybackEventKind::SeekingChanged);
            e.flag = false;
            out.push(e);
        }
        if self.s.buffering {
            self.s.buffering = false;
            let mut e = PlaybackEvent::new(PlaybackEventKind::BufferingChanged);
            e.flag = false;
            out.push(e);
        }

        // Track-switch path: pending_load means a fresh loadfile is in
        // flight. Eat the EOF/cancel so consumers don't see a Stopped
        // flicker between tracks. Errors still terminate so failures
        // surface to the user.
        if self.pending_load && reason != EndReason::Error {
            self.pending_load = false;
            self.s.presence = PlayerPresence::Present;
            self.s.phase = PlaybackPhase::Starting;
            return out;
        }

        self.pending_load = false;
        self.pause_requested = false;
        self.stream_session = None;
        self.s.presence = PlayerPresence::None;
        self.s.phase = PlaybackPhase::Stopped;
        self.s.position_us = 0;
        self.s.variant_switch_pending = false;
        self.last_known_item_id.clear();

        let mut terminal = match reason {
            EndReason::Eof => PlaybackEvent::new(PlaybackEventKind::Finished),
            EndReason::Canceled => PlaybackEvent::new(PlaybackEventKind::Canceled),
            EndReason::Error => PlaybackEvent::new(PlaybackEventKind::Error),
        };
        if reason == EndReason::Error {
            terminal.error_message = error_message;
            // A startup failure followed by stop must remain Error; late
            // native FILE_LOADED/EOF/cancel cannot revive or finish that load.
            self.load_failed = true;
        }
        out.push(terminal);
        out
    }

    pub fn on_seeking_changed(&mut self, seeking: bool) -> Vec<PlaybackEvent> {
        if !is_active_phase(self.s.phase) {
            return vec![];
        }
        if self.s.seeking == seeking {
            return vec![];
        }
        self.s.seeking = seeking;
        let mut e = PlaybackEvent::new(PlaybackEventKind::SeekingChanged);
        e.flag = seeking;
        vec![e]
    }

    pub fn on_paused_for_cache(&mut self, pfc: bool) -> Vec<PlaybackEvent> {
        if self.paused_for_cache == pfc {
            return vec![];
        }
        self.paused_for_cache = pfc;
        if !is_active_phase(self.s.phase) {
            return vec![];
        }
        if self.s.phase == PlaybackPhase::Paused {
            return vec![];
        }
        apply_buffering_change(
            &mut self.s,
            self.paused_for_cache,
            self.core_idle,
            self.pause_requested,
            self.frame_available,
        )
    }

    pub fn on_core_idle(&mut self, core_idle: bool) -> Vec<PlaybackEvent> {
        if self.core_idle == core_idle {
            return vec![];
        }
        self.core_idle = core_idle;
        if !is_active_phase(self.s.phase) {
            return vec![];
        }
        if self.s.phase == PlaybackPhase::Paused {
            return vec![];
        }
        apply_buffering_change(
            &mut self.s,
            self.paused_for_cache,
            self.core_idle,
            self.pause_requested,
            self.frame_available,
        )
    }

    pub fn on_position(&mut self, position_us: i64) -> Vec<PlaybackEvent> {
        if self.s.position_us == position_us && !self.s.seeking {
            return vec![];
        }
        self.s.position_us = position_us;
        let mut out = vec![];
        if self.s.seeking {
            self.s.seeking = false;
            let mut e = PlaybackEvent::new(PlaybackEventKind::SeekingChanged);
            e.flag = false;
            out.push(e);
        }
        out.push(PlaybackEvent::new(PlaybackEventKind::PositionChanged));
        out
    }

    pub fn on_media_type(&mut self, ty: MediaType) -> Vec<PlaybackEvent> {
        if self.s.media_type == ty {
            return vec![];
        }
        self.s.media_type = ty;
        let mut out = vec![PlaybackEvent::new(PlaybackEventKind::MediaTypeChanged)];
        // Switching to Audio relaxes the frame-available gate; promote
        // now if the rest of the conditions are met.
        if self.s.phase == PlaybackPhase::Starting
            && self.pause_requested
            && !self.s.buffering
            && ready_to_play(self.s.media_type, self.frame_available)
        {
            transition_to_playing(&mut self.s, &mut out);
        }
        out
    }

    pub fn on_video_frame_available(&mut self, available: bool) -> Vec<PlaybackEvent> {
        if self.frame_available == available {
            return vec![];
        }
        self.frame_available = available;
        if !available {
            return vec![];
        }
        if self.s.phase != PlaybackPhase::Starting {
            return vec![];
        }
        if !self.pause_requested {
            return vec![];
        }
        if self.s.buffering {
            return vec![];
        }
        let mut out = vec![];
        transition_to_playing(&mut self.s, &mut out);
        out
    }

    pub fn on_playback_restart(&mut self) -> Vec<PlaybackEvent> {
        if self.s.phase != PlaybackPhase::Starting {
            return vec![];
        }
        self.pause_requested = true;
        // A trackless/old-frame restart is possible before the selected video
        // has reached VO. Only video-frame-info establishes video readiness.
        if self.s.buffering || !ready_to_play(self.s.media_type, self.frame_available) {
            return vec![];
        }
        let mut out = vec![];
        transition_to_playing(&mut self.s, &mut out);
        out
    }

    pub fn on_speed(&mut self, rate: f64) -> Vec<PlaybackEvent> {
        if self.s.rate == rate {
            return vec![];
        }
        self.s.rate = rate;
        vec![PlaybackEvent::new(PlaybackEventKind::RateChanged)]
    }

    pub fn on_duration(&mut self, duration_us: i64) -> Vec<PlaybackEvent> {
        if self.s.duration_us == duration_us {
            return vec![];
        }
        self.s.duration_us = duration_us;
        vec![PlaybackEvent::new(PlaybackEventKind::DurationChanged)]
    }

    pub fn on_fullscreen(&mut self, fullscreen: bool, was_maximized: bool) -> Vec<PlaybackEvent> {
        if self.s.fullscreen == fullscreen {
            return vec![];
        }
        self.s.fullscreen = fullscreen;
        self.s.maximized_before_fullscreen = if fullscreen { was_maximized } else { false };
        vec![PlaybackEvent::new(PlaybackEventKind::FullscreenChanged)]
    }

    pub fn on_buffered_ranges(&mut self, ranges: Vec<PlaybackBufferedRange>) -> Vec<PlaybackEvent> {
        if ranges == self.s.buffered {
            return vec![];
        }
        self.s.buffered = ranges;
        vec![PlaybackEvent::new(PlaybackEventKind::BufferedRangesChanged)]
    }

    pub fn on_display_hz(&mut self, hz: f64) -> Vec<PlaybackEvent> {
        if self.s.display_hz == hz {
            return vec![];
        }
        self.s.display_hz = hz;
        vec![PlaybackEvent::new(PlaybackEventKind::DisplayHzChanged)]
    }
}

fn is_active_phase(p: PlaybackPhase) -> bool {
    matches!(
        p,
        PlaybackPhase::Starting | PlaybackPhase::Playing | PlaybackPhase::Paused
    )
}

fn ready_to_play(ty: MediaType, frame_available: bool) -> bool {
    ty == MediaType::Audio || frame_available
}

fn transition_to_playing(s: &mut PlaybackSnapshot, out: &mut Vec<PlaybackEvent>) {
    s.phase = PlaybackPhase::Playing;
    s.variant_switch_pending = false;
    out.push(PlaybackEvent::new(PlaybackEventKind::Started));
}

fn apply_buffering_change(
    s: &mut PlaybackSnapshot,
    pfc: bool,
    core_idle: bool,
    pause_requested: bool,
    frame_available: bool,
) -> Vec<PlaybackEvent> {
    let combined = pfc || core_idle;
    if s.buffering == combined {
        return vec![];
    }
    s.buffering = combined;

    let mut out = vec![];
    let mut be = PlaybackEvent::new(PlaybackEventKind::BufferingChanged);
    be.flag = combined;
    out.push(be);

    if !combined
        && s.phase == PlaybackPhase::Starting
        && pause_requested
        && ready_to_play(s.media_type, frame_available)
    {
        transition_to_playing(s, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(events: &[PlaybackEvent], kind: PlaybackEventKind) -> bool {
        events.iter().any(|e| e.kind == kind)
    }

    #[test]
    fn default_snapshot_stopped_absent() {
        let sm = PlaybackStateMachine::new();
        let s = sm.snapshot();
        assert_eq!(s.presence, PlayerPresence::None);
        assert_eq!(s.phase, PlaybackPhase::Stopped);
        assert!(!s.seeking);
        assert!(!s.buffering);
        assert_eq!(s.media_type, MediaType::Unknown);
        assert_eq!(s.position_us, 0);
    }

    #[test]
    fn file_loaded_emits_track_loaded() {
        let mut sm = PlaybackStateMachine::new();
        let out = sm.on_file_loaded();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, PlaybackEventKind::TrackLoaded);
        let s = sm.snapshot();
        assert_eq!(s.presence, PlayerPresence::Present);
        assert_eq!(s.phase, PlaybackPhase::Starting);
    }

    #[test]
    fn playback_restart_waits_for_video_frame_without_pause_property_edge() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting("item-A".into());
        sm.on_media_type(MediaType::Video);
        sm.on_file_loaded();

        let out = sm.on_playback_restart();
        assert!(!has(&out, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Starting);
        let frame = sm.on_video_frame_available(true);
        assert!(has(&frame, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);
    }

    #[test]
    fn playback_restart_waits_for_core_idle_clear_without_pause_property_edge() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting("item-A".into());
        sm.on_media_type(MediaType::Video);
        sm.on_core_idle(true);
        sm.on_file_loaded();

        let restart = sm.on_playback_restart();
        assert!(!has(&restart, PlaybackEventKind::Started));
        assert!(sm.on_video_frame_available(true).is_empty());

        let ready = sm.on_core_idle(false);
        assert!(has(&ready, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);
    }

    #[test]
    fn playback_restart_does_not_resume_an_explicit_pause() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(true);

        let out = sm.on_playback_restart();

        assert!(!has(&out, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Paused);
    }

    #[test]
    fn initial_pause_resume_does_not_skip_video_frame_gate() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting("movie".into());
        sm.on_media_type(MediaType::Video);
        sm.on_file_loaded();
        sm.on_pause_changed(true);
        assert!(!has(
            &sm.on_pause_changed(false),
            PlaybackEventKind::Started
        ));
        assert!(!has(&sm.on_playback_restart(), PlaybackEventKind::Started));
        assert!(has(
            &sm.on_video_frame_available(true),
            PlaybackEventKind::Started
        ));
    }

    #[test]
    fn unknown_metadata_cannot_bypass_first_frame_gate() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting("unprobed-movie".into());
        sm.on_file_loaded();
        assert!(!has(
            &sm.on_pause_changed(false),
            PlaybackEventKind::Started
        ));
        assert!(!has(&sm.on_playback_restart(), PlaybackEventKind::Started));
        assert!(has(
            &sm.on_video_frame_available(true),
            PlaybackEventKind::Started
        ));
    }

    #[test]
    fn parsed_audio_source_does_not_need_video_frame() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting("audio".into());
        sm.on_media_type(MediaType::Audio);
        sm.on_file_loaded();
        assert!(has(&sm.on_playback_restart(), PlaybackEventKind::Started));
    }

    #[test]
    fn load_starting_emits_track_loaded() {
        let mut sm = PlaybackStateMachine::new();
        let out = sm.on_load_starting(String::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, PlaybackEventKind::TrackLoaded);
        let s = sm.snapshot();
        assert_eq!(s.presence, PlayerPresence::Present);
        assert_eq!(s.phase, PlaybackPhase::Starting);
    }

    #[test]
    fn position_seed_survives_file_loaded() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting(String::new());
        sm.on_position(5_000_000);
        assert_eq!(sm.snapshot().position_us, 5_000_000);
        sm.on_file_loaded();
        assert_eq!(sm.snapshot().position_us, 5_000_000);
    }

    #[test]
    fn track_switch_preserves_seeded_position() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_position(1_000_000);

        sm.on_load_starting(String::new());
        sm.on_position(8_000_000);
        let out = sm.on_end_file(EndReason::Eof, String::new());
        assert!(!has(&out, PlaybackEventKind::Finished));
        assert_eq!(sm.snapshot().position_us, 8_000_000);
        sm.on_file_loaded();
        assert_eq!(sm.snapshot().position_us, 8_000_000);
    }

    #[test]
    fn variant_switch_first_load_then_same_id_reload() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_media_type(MediaType::Audio);
        sm.on_load_starting("item-A".into());
        assert!(!sm.snapshot().variant_switch_pending);
        sm.on_file_loaded();
        assert!(!sm.snapshot().variant_switch_pending);
        sm.on_pause_changed(false);
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);

        sm.on_load_starting("item-A".into());
        assert!(sm.snapshot().variant_switch_pending);
        sm.on_file_loaded();
        assert!(sm.snapshot().variant_switch_pending);
        sm.on_pause_changed(false);
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);
        assert!(!sm.snapshot().variant_switch_pending);
    }

    #[test]
    fn variant_switch_cleared_by_started_from_buffering_clear() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_media_type(MediaType::Audio);
        sm.on_load_starting("item-A".into());
        sm.on_file_loaded();
        sm.on_pause_changed(false);

        sm.on_load_starting("item-A".into());
        sm.on_file_loaded();
        assert!(sm.snapshot().variant_switch_pending);
        sm.on_paused_for_cache(true);
        sm.on_pause_changed(false);
        assert!(sm.snapshot().variant_switch_pending);
        let out = sm.on_paused_for_cache(false);
        assert!(has(&out, PlaybackEventKind::Started));
        assert!(!sm.snapshot().variant_switch_pending);
    }

    #[test]
    fn variant_switch_cleared_by_terminal() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting("item-A".into());
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_load_starting("item-A".into());
        assert!(sm.snapshot().variant_switch_pending);
        sm.on_end_file(EndReason::Error, "boom".into());
        assert!(!sm.snapshot().variant_switch_pending);
    }

    #[test]
    fn variant_switch_false_on_different_id() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting("item-A".into());
        sm.on_file_loaded();
        sm.on_load_starting("item-B".into());
        assert!(!sm.snapshot().variant_switch_pending);
    }

    #[test]
    fn variant_switch_empty_id_never_marks() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting(String::new());
        sm.on_file_loaded();
        sm.on_load_starting(String::new());
        assert!(!sm.snapshot().variant_switch_pending);
    }

    #[test]
    fn variant_switch_terminal_clears_identity() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting("item-A".into());
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_end_file(EndReason::Eof, String::new());
        sm.on_load_starting("item-A".into());
        assert!(!sm.snapshot().variant_switch_pending);
    }

    #[test]
    fn pending_load_swallows_eof() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_media_type(MediaType::Audio);
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);

        sm.on_load_starting(String::new());
        let out = sm.on_end_file(EndReason::Eof, String::new());
        assert!(!has(&out, PlaybackEventKind::Finished));
        assert!(!has(&out, PlaybackEventKind::Canceled));
        let s = sm.snapshot();
        assert_eq!(s.presence, PlayerPresence::Present);
        assert_eq!(s.phase, PlaybackPhase::Starting);

        let loaded = sm.on_file_loaded();
        assert!(has(&loaded, PlaybackEventKind::TrackLoaded));
        let resumed = sm.on_pause_changed(false);
        assert!(has(&resumed, PlaybackEventKind::Started));
    }

    #[test]
    fn pending_load_swallows_cancel() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_load_starting(String::new());
        let out = sm.on_end_file(EndReason::Canceled, String::new());
        assert!(!has(&out, PlaybackEventKind::Canceled));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Starting);
    }

    #[test]
    fn pending_load_does_not_swallow_error() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_load_starting(String::new());
        let out = sm.on_end_file(EndReason::Error, "boom".into());
        assert!(has(&out, PlaybackEventKind::Error));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Stopped);
        assert_eq!(sm.snapshot().presence, PlayerPresence::None);
    }

    #[test]
    fn file_loaded_clears_stale_pending_load() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting(String::new());
        sm.on_file_loaded();
        let out = sm.on_end_file(EndReason::Eof, String::new());
        assert!(has(&out, PlaybackEventKind::Finished));
    }

    #[test]
    fn pause_while_idle_ignored() {
        let mut sm = PlaybackStateMachine::new();
        assert!(sm.on_pause_changed(false).is_empty());
        assert!(sm.on_pause_changed(true).is_empty());
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Stopped);
        assert_eq!(sm.snapshot().presence, PlayerPresence::None);
    }

    #[test]
    fn started_waits_for_core_idle_clear() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_media_type(MediaType::Audio);
        sm.on_core_idle(true);
        sm.on_file_loaded();
        assert!(sm.snapshot().buffering);

        let unpause = sm.on_pause_changed(false);
        assert!(!has(&unpause, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Starting);

        let cleared = sm.on_core_idle(false);
        assert!(has(&cleared, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);
        assert!(!sm.snapshot().buffering);
    }

    #[test]
    fn core_idle_preserved_across_file_loaded() {
        let mut sm = PlaybackStateMachine::new();
        let idle = sm.on_core_idle(true);
        assert!(idle.is_empty());
        let loaded = sm.on_file_loaded();
        assert!(has(&loaded, PlaybackEventKind::TrackLoaded));
        assert!(sm.snapshot().buffering);
    }

    #[test]
    fn pfc_preserved_across_file_loaded() {
        let mut sm = PlaybackStateMachine::new();
        let pfc = sm.on_paused_for_cache(true);
        assert!(pfc.is_empty());
        let loaded = sm.on_file_loaded();
        assert!(has(&loaded, PlaybackEventKind::TrackLoaded));
        assert!(sm.snapshot().buffering);
    }

    #[test]
    fn pause_toggles_edge_triggered() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_media_type(MediaType::Audio);
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        let p1 = sm.on_pause_changed(true);
        assert!(has(&p1, PlaybackEventKind::Paused));
        let p2 = sm.on_pause_changed(true);
        assert!(p2.is_empty());
        let r = sm.on_pause_changed(false);
        assert!(has(&r, PlaybackEventKind::Started));
        let r2 = sm.on_pause_changed(false);
        assert!(r2.is_empty());
    }

    #[test]
    fn intentional_pause_does_not_surface_core_idle_as_buffering() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_pause_changed(true);

        let idle = sm.on_core_idle(true);

        assert!(idle.is_empty());
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Paused);
        assert!(!sm.snapshot().buffering);
    }

    #[test]
    fn intentional_pause_clears_active_buffering() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_paused_for_cache(true);
        assert!(sm.snapshot().buffering);

        let paused = sm.on_pause_changed(true);

        assert!(
            paused
                .iter()
                .any(|event| { event.kind == PlaybackEventKind::BufferingChanged && !event.flag })
        );
        assert!(has(&paused, PlaybackEventKind::Paused));
        assert!(!sm.snapshot().buffering);
    }

    #[test]
    fn eof_force_clears_seeking_and_buffering() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_seeking_changed(true);
        sm.on_paused_for_cache(true);
        let out = sm.on_end_file(EndReason::Eof, String::new());
        assert!(has(&out, PlaybackEventKind::Finished));
        let mut saw_seek_false = false;
        let mut saw_buf_false = false;
        for e in &out {
            if e.kind == PlaybackEventKind::SeekingChanged && !e.flag {
                saw_seek_false = true;
            }
            if e.kind == PlaybackEventKind::BufferingChanged && !e.flag {
                saw_buf_false = true;
            }
        }
        assert!(saw_seek_false);
        assert!(saw_buf_false);
        let s = sm.snapshot();
        assert_eq!(s.phase, PlaybackPhase::Stopped);
        assert_eq!(s.presence, PlayerPresence::None);
        assert!(!s.seeking);
        assert!(!s.buffering);
    }

    #[test]
    fn error_end_file_carries_message() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        let out = sm.on_end_file(EndReason::Error, "boom".into());
        assert!(
            out.iter()
                .any(|e| e.kind == PlaybackEventKind::Error && e.error_message == "boom")
        );
    }

    #[test]
    fn startup_error_cannot_be_replaced_by_late_native_events() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_load_starting("movie".into());
        assert!(has(
            &sm.on_end_file(EndReason::Error, "startup failed".into()),
            PlaybackEventKind::Error
        ));
        assert!(sm.on_file_loaded().is_empty());
        assert!(
            sm.on_end_file(EndReason::Canceled, String::new())
                .is_empty()
        );
        assert!(sm.on_end_file(EndReason::Eof, String::new()).is_empty());
        sm.on_load_starting("retry".into());
        assert!(has(&sm.on_file_loaded(), PlaybackEventKind::TrackLoaded));
    }

    #[test]
    fn cancel_end_file_emits_canceled() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        let out = sm.on_end_file(EndReason::Canceled, String::new());
        assert!(has(&out, PlaybackEventKind::Canceled));
    }

    #[test]
    fn seeking_edge_triggered() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        let a = sm.on_seeking_changed(true);
        assert_eq!(a.len(), 1);
        assert!(a[0].flag);
        let b = sm.on_seeking_changed(true);
        assert!(b.is_empty());
    }

    #[test]
    fn buffering_during_starting_holds_back_started() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_media_type(MediaType::Audio);
        sm.on_file_loaded();
        sm.on_paused_for_cache(true);
        let u = sm.on_pause_changed(false);
        assert!(!has(&u, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Starting);
        let bc = sm.on_paused_for_cache(false);
        assert!(has(&bc, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);
    }

    #[test]
    fn resume_from_paused_immediate() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_media_type(MediaType::Audio);
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_pause_changed(true);
        sm.on_paused_for_cache(true);
        let out = sm.on_pause_changed(false);
        assert!(has(&out, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);
    }

    #[test]
    fn core_idle_gates_started_pre_roll() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_media_type(MediaType::Audio);
        sm.on_file_loaded();
        sm.on_core_idle(true);
        let u = sm.on_pause_changed(false);
        assert!(!has(&u, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Starting);
        assert!(sm.snapshot().buffering);
        let cleared = sm.on_core_idle(false);
        assert!(has(&cleared, PlaybackEventKind::Started));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);
        assert!(!sm.snapshot().buffering);
    }

    #[test]
    fn buffering_ors_pfc_and_core_idle() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_paused_for_cache(true);
        sm.on_core_idle(true);
        assert!(sm.snapshot().buffering);

        sm.on_paused_for_cache(false);
        assert!(sm.snapshot().buffering);

        sm.on_core_idle(false);
        assert!(!sm.snapshot().buffering);
    }

    #[test]
    fn buffering_uses_pfc() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        let a = sm.on_paused_for_cache(true);
        assert_eq!(a.len(), 1);
        assert!(a[0].flag);
        let b = sm.on_paused_for_cache(false);
        assert_eq!(b.len(), 1);
        assert!(!b[0].flag);
    }

    #[test]
    fn position_completes_seek() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_seeking_changed(true);
        let out = sm.on_position(1234567);
        assert_eq!(sm.snapshot().position_us, 1234567);
        assert!(!sm.snapshot().seeking);
        assert!(
            out.iter()
                .any(|e| e.kind == PlaybackEventKind::SeekingChanged && !e.flag)
        );
    }

    #[test]
    fn media_type_edge_triggered() {
        let mut sm = PlaybackStateMachine::new();
        let a = sm.on_media_type(MediaType::Video);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].kind, PlaybackEventKind::MediaTypeChanged);
        assert_eq!(sm.snapshot().media_type, MediaType::Video);
        let b = sm.on_media_type(MediaType::Video);
        assert!(b.is_empty());
        let c = sm.on_media_type(MediaType::Audio);
        assert_eq!(c.len(), 1);
    }

    fn stream_failure(session_id: u64) -> jfn_mpv::stream_cb::StreamFailure {
        jfn_mpv::stream_cb::StreamFailure {
            session_id,
            phase: "seek",
            offset: 1000,
            error: jfn_mpv::stream_cb::StreamError::Http {
                status: 403,
                message: "115 pmt 3-2".to_string(),
            },
        }
    }

    #[test]
    fn stream_failure_before_file_loaded_is_error_and_late_eof_cannot_finish() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_stream_load_starting("movie".to_string(), 1);
        let events = sm.on_stream_failed(stream_failure(1));
        let error = events
            .iter()
            .find(|e| e.kind == PlaybackEventKind::Error)
            .expect("error event");
        assert_eq!(
            error.stream_failure.as_ref().and_then(|f| f.http_status()),
            Some(403)
        );
        assert!(error.error_message.contains("115 pmt 3-2"));
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Stopped);
        assert!(sm.on_file_loaded().is_empty());
        assert!(sm.on_playback_restart().is_empty());
        assert!(sm.on_end_file(EndReason::Eof, String::new()).is_empty());
        assert!(
            sm.on_end_file(EndReason::Canceled, String::new())
                .is_empty()
        );
        assert!(sm.on_stream_failed(stream_failure(1)).is_empty());
        assert!(sm.consume_actions().is_empty());
    }

    #[test]
    fn stream_failure_stops_playback_and_clears_seeking_and_buffering() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_stream_load_starting("movie".to_string(), 1);
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_playback_restart();
        sm.on_seeking_changed(true);
        sm.on_paused_for_cache(true);
        let events = sm.on_stream_failed(stream_failure(1));
        assert!(has(&events, PlaybackEventKind::Error));
        assert!(!has(&events, PlaybackEventKind::Finished));
        assert!(!sm.snapshot().seeking);
        assert!(!sm.snapshot().buffering);
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Stopped);
        assert!(sm.consume_actions().is_empty());
    }

    #[test]
    fn replay_ignores_old_stream_failure_and_can_start_again() {
        let mut sm = PlaybackStateMachine::new();
        sm.on_media_type(MediaType::Audio);
        sm.on_stream_load_starting("movie".to_string(), 1);
        sm.on_stream_failed(stream_failure(1));
        sm.on_stream_load_starting("movie".to_string(), 2);
        assert!(sm.on_stream_failed(stream_failure(1)).is_empty());
        sm.on_file_loaded();
        sm.on_pause_changed(false);
        sm.on_playback_restart();
        assert_eq!(sm.snapshot().phase, PlaybackPhase::Playing);
        let events = sm.on_end_file(EndReason::Eof, String::new());
        assert!(has(&events, PlaybackEventKind::Finished));
        assert!(sm.on_stream_failed(stream_failure(2)).is_empty());
    }
}
