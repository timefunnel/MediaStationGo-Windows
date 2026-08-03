use base64::Engine as _;
use cef::{ImplListValue, ListValue, sys};
use jfn_frame_interpolation::{
    InterpolationMode, InterpolationModel, InterpolationPlan, PlanRequest, prepare_plan,
};
use jfn_mediastation::{
    ApiError, DeliveryMode, ExternalSubtitleDownload, HeaderEncodingError, MediaCard, MediaDetail,
    MediaHome, MediaImageRef, MediaImageType, MediaPage, MediaStationApiClient,
    MediaStationSession, PlaybackSessionError, PlaybackSessionResolver, PlaybackSource,
    PlaybackTrackPlan, PlaybackTrackPreference, PlaybackTrackPreferenceUpdate, SessionExpirySource,
    SubtitleTrack, UreqTransport, build_playback_track_plan,
};
use jfn_mpv::api::{
    JfnMpvLoadOptions, LoadError, jfn_mpv_free_string, jfn_mpv_get_property_double,
    jfn_mpv_get_property_int, jfn_mpv_get_property_node, jfn_mpv_get_property_string,
    jfn_mpv_load_file, jfn_mpv_set_audio_track_checked, jfn_mpv_set_subtitle_track_checked,
    jfn_mpv_stop, jfn_mpv_sub_add_checked, jfn_mpv_sub_remove_current_checked,
};
use jfn_mpv::boot::jfn_mpv_handle_get;
use jfn_playback::{
    EndReason, Input as PbInput, MediaType as PbMediaType, PlaybackEvent, PlaybackEventKind,
    post as pb_post,
};
use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::ffi::{CStr, CString};
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, OnceLock, Weak};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::NamedTempFile;
use url::Url;

use crate::APP_VERSION;
use crate::client::{Inner, RendererValue, post_renderer_message};
use crate::ipc::list_string;
use crate::mediastation_cache::{HomeSnapshotCache, ImageDiskCache, image_cache_key};
use crate::mediastation_credentials::{StoredSession, WindowsCredentialStore};

const OPERATION_LOAD: &str = "load";
const OPERATION_AUTHENTICATE: &str = "authenticate";
const OPERATION_SESSION_STATUS: &str = "session_status";
const OPERATION_LOGOUT: &str = "logout";
const OPERATION_IMAGE: &str = "image";
const OPERATION_TRACKS: &str = "tracks";
const OPERATION_TRACK_SELECTION: &str = "track_selection";
const MAX_REQUEST_ID_LEN: usize = 128;
const MAX_MEDIA_ID_LEN: usize = 256;
const MAX_SERVER_URL_LEN: usize = 2_048;
const MAX_USERNAME_LEN: usize = 256;
const MAX_PASSWORD_LEN: usize = 4_096;
const MAX_TRACK_KEY_LEN: usize = 512;
const MAX_ACTIVE_LOAD_REQUESTS: usize = 1;
const MAX_ACTIVE_AUTH_REQUESTS: usize = 1;
const MAX_ACTIVE_CATALOG_REQUESTS: usize = 2;
const MAX_ACTIVE_IMAGE_REQUESTS: usize = 2;
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_EXTERNAL_SUBTITLE_BYTES: usize = 16 * 1024 * 1024;
const SUBTITLE_CACHE_DIRECTORY: &str = "mediastation-subtitles";
const HOME_CACHE_DIRECTORY: &str = "mediastation-home-v1";
const IMAGE_CACHE_DIRECTORY: &str = "mediastation-images-v1";
const MAX_IMAGE_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const SUBTITLE_FILE_PREFIX: &str = "subtitle-";
const PROGRESS_REPORT_INTERVAL: Duration = Duration::from_secs(10);
const MAX_PENDING_REPORTS: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReportKind {
    Playing,
    Progress { paused: bool },
    Stopped,
}

struct ReportJob {
    generation: u64,
    session: MediaStationSession,
    source: PlaybackSource,
    kind: ReportKind,
    position_ms: u64,
}

struct ReporterQueue {
    jobs: VecDeque<ReportJob>,
    shutting_down: bool,
}

struct ReporterShared {
    queue: Mutex<ReporterQueue>,
    wake: Condvar,
}

struct PlaybackReportDispatcher {
    shared: Arc<ReporterShared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone, Copy, Debug)]
enum ReportQueueError {
    WorkerUnavailable,
    ShuttingDown,
    QueueFull,
}

impl ReportQueueError {
    const fn code(self) -> &'static str {
        match self {
            Self::WorkerUnavailable => "worker_unavailable",
            Self::ShuttingDown => "shutting_down",
            Self::QueueFull => "queue_full",
        }
    }
}

impl PlaybackReportDispatcher {
    fn new(api: MediaStationApiClient) -> Self {
        let shared = Arc::new(ReporterShared {
            queue: Mutex::new(ReporterQueue {
                jobs: VecDeque::new(),
                shutting_down: false,
            }),
            wake: Condvar::new(),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = match thread::Builder::new()
            .name("mediastation-reporting".to_string())
            .spawn(move || playback_report_worker(api, worker_shared))
        {
            Ok(worker) => Some(worker),
            Err(error) => {
                log_error(&format!(
                    "MediaStation playback reporting worker failed to start: kind={}",
                    error.kind()
                ));
                None
            }
        };
        Self {
            shared,
            worker: Mutex::new(worker),
        }
    }

    fn is_available(&self) -> bool {
        self.worker
            .lock()
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }

    fn enqueue(&self, job: ReportJob) -> Result<(), ReportQueueError> {
        if !self.is_available() {
            return Err(ReportQueueError::WorkerUnavailable);
        }
        let mut queue = self.shared.queue.lock();
        if queue.shutting_down {
            return Err(ReportQueueError::ShuttingDown);
        }
        if matches!(job.kind, ReportKind::Progress { .. })
            && let Some(pending) = queue.jobs.iter_mut().find(|pending| {
                pending.generation == job.generation
                    && matches!(pending.kind, ReportKind::Progress { .. })
            })
        {
            *pending = job;
            self.shared.wake.notify_one();
            return Ok(());
        }
        if queue.jobs.len() >= MAX_PENDING_REPORTS {
            if matches!(job.kind, ReportKind::Playing | ReportKind::Stopped)
                && let Some(index) = queue
                    .jobs
                    .iter()
                    .position(|pending| matches!(pending.kind, ReportKind::Progress { .. }))
            {
                queue.jobs.remove(index);
            } else {
                return Err(ReportQueueError::QueueFull);
            }
        }
        if queue.jobs.len() >= MAX_PENDING_REPORTS {
            return Err(ReportQueueError::QueueFull);
        }
        queue.jobs.push_back(job);
        self.shared.wake.notify_one();
        Ok(())
    }

    fn shutdown(&self) {
        {
            let mut queue = self.shared.queue.lock();
            queue.shutting_down = true;
            self.shared.wake.notify_all();
        }
        let worker = self.worker.lock().take();
        if let Some(worker) = worker
            && worker.join().is_err()
        {
            log_error("MediaStation playback reporting worker terminated unexpectedly");
        }
    }
}

impl Drop for PlaybackReportDispatcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn playback_report_worker(api: MediaStationApiClient, shared: Arc<ReporterShared>) {
    loop {
        let job = {
            let mut queue = shared.queue.lock();
            while queue.jobs.is_empty() && !queue.shutting_down {
                shared.wake.wait(&mut queue);
            }
            if let Some(job) = queue.jobs.pop_front() {
                job
            } else if queue.shutting_down {
                return;
            } else {
                continue;
            }
        };
        let result = match job.kind {
            ReportKind::Playing => {
                api.report_playing(&job.session, &job.source, job.position_ms, false)
            }
            ReportKind::Progress { paused } => {
                api.report_progress(&job.session, &job.source, job.position_ms, paused)
            }
            ReportKind::Stopped => api.report_stopped(&job.session, &job.source, job.position_ms),
        };
        if let Err(error) = result {
            log_error(&format!(
                "MediaStation playback report failed: kind={} generation={} code={}",
                report_kind_label(job.kind),
                job.generation,
                api_error_code(&error)
            ));
        } else if matches!(job.kind, ReportKind::Stopped) {
            // The stopped position is now persisted server-side; tell the
            // renderer so it can refresh Continue Watching without guessing
            // when the async report landed.
            notify_home_stale();
        }
    }
}

fn notify_home_stale() {
    log_debug("MediaStation notifying renderer that the home catalog is stale");
    let Some(layer) = crate::business_web::active_web_layer() else {
        log_error("MediaStation home_stale notify skipped: no active web layer");
        return;
    };
    dispatch_response(
        &layer,
        "",
        "playback_event",
        true,
        json!({ "kind": "home_stale" }),
    );
}

const fn report_kind_label(kind: ReportKind) -> &'static str {
    match kind {
        ReportKind::Playing => "playing",
        ReportKind::Progress { paused: true } => "progress_paused",
        ReportKind::Progress { paused: false } => "progress_playing",
        ReportKind::Stopped => "stopped",
    }
}

fn dispatch_playback_event(event: &PlaybackEvent) {
    let Some(layer) = crate::business_web::active_web_layer() else {
        return;
    };
    dispatch_response(
        &layer,
        "",
        "playback_event",
        true,
        playback_event_payload(event),
    );
}

fn playback_event_payload(event: &PlaybackEvent) -> Value {
    let kind = playback_event_kind_label(event.kind);
    let error_code = (event.kind == PlaybackEventKind::Error
        && event.error_message == RIFE_FILTER_INACTIVE)
        .then_some(RIFE_FILTER_INACTIVE);
    json!({
        "kind": kind,
        "errorCode": error_code,
        "positionMs": event.snapshot.position_us.max(0) / 1_000,
        "durationMs": event.snapshot.duration_us.max(0) / 1_000,
        "buffering": event.snapshot.buffering,
        "seeking": event.snapshot.seeking,
        "rate": event.snapshot.rate,
    })
}

const fn playback_event_kind_label(kind: PlaybackEventKind) -> &'static str {
    match kind {
        PlaybackEventKind::Started => "started",
        PlaybackEventKind::Paused => "paused",
        PlaybackEventKind::Finished => "finished",
        PlaybackEventKind::Canceled => "canceled",
        PlaybackEventKind::Error => "error",
        PlaybackEventKind::SeekingChanged => "seeking",
        PlaybackEventKind::BufferingChanged => "buffering",
        PlaybackEventKind::MediaTypeChanged => "media_type",
        PlaybackEventKind::TrackLoaded => "track_loaded",
        PlaybackEventKind::PositionChanged => "position",
        PlaybackEventKind::DurationChanged => "duration",
        PlaybackEventKind::RateChanged => "rate",
        PlaybackEventKind::FullscreenChanged => "fullscreen",
        PlaybackEventKind::BufferedRangesChanged => "buffered",
        PlaybackEventKind::DisplayHzChanged => "display_hz",
        PlaybackEventKind::MetadataChanged => "metadata",
        PlaybackEventKind::ArtworkChanged => "artwork",
        PlaybackEventKind::QueueCapsChanged => "queue",
        PlaybackEventKind::Seeked => "seeked",
    }
}

fn interpolation_filter_failed(
    filter_seen: &mut bool,
    missing_filter_samples: &mut u8,
    status: Option<&str>,
) -> bool {
    match status {
        Some("initializing" | "active") => {
            *filter_seen = true;
            *missing_filter_samples = 0;
            false
        }
        Some(_) => true,
        None if *filter_seen => true,
        None => {
            *missing_filter_samples = (*missing_filter_samples).saturating_add(1);
            *missing_filter_samples >= RIFE_FILTER_MISSING_SAMPLE_LIMIT
        }
    }
}

struct ActivePlaybackReport {
    generation: u64,
    session: MediaStationSession,
    source: PlaybackSource,
    preference: PlaybackTrackPreference,
    /// Incremented whenever the user explicitly saves a track preference, so
    /// the first-frame reconcile worker can tell whether the user changed the
    /// tracks after playback loaded and must not override that choice.
    preference_revision: u64,
    runtime_tracks_reconciled: bool,
    started: bool,
    paused: bool,
    last_position_ms: u64,
    last_report_at: Option<Instant>,
}

struct RuntimeTrackReconcile {
    snapshot: SessionSnapshot,
    source: PlaybackSource,
    preference: PlaybackTrackPreference,
    /// Track preference revision captured when the session started. The
    /// reconcile worker skips overriding the runtime tracks if this is stale,
    /// meaning the user changed the selection after playback loaded.
    preference_revision: u64,
}

struct PreparedExternalSubtitle {
    _file: NamedTempFile,
    path: CString,
    byte_count: usize,
    redirect_count: usize,
    target_host: String,
}

struct RuntimeState {
    session: Option<MediaStationSession>,
    session_profile: Option<SessionProfile>,
    generation: u64,
    active_request_ids: HashSet<String>,
    active_auth_request_ids: HashSet<String>,
    active_catalog_request_ids: HashSet<String>,
    active_image_request_ids: HashSet<String>,
    active_subtitle: Option<PreparedExternalSubtitle>,
    active_interpolation: Option<ActiveInterpolation>,
    active_report: Option<ActivePlaybackReport>,
}

struct ActiveInterpolation {
    media_id: String,
    plan: InterpolationPlan,
    filter_seen: bool,
    missing_filter_samples: u8,
}

const RIFE_FILTER_INACTIVE: &str = "frame_interpolation_filter_inactive";
const RIFE_FILTER_MISSING_SAMPLE_LIMIT: u8 = 3;

#[derive(Clone)]
struct SessionProfile {
    user_name: String,
    persisted: bool,
}

struct MediaStationRuntime {
    api: MediaStationApiClient,
    resolver: PlaybackSessionResolver<UreqTransport>,
    user_agent: String,
    reporter: PlaybackReportDispatcher,
    home_cache: HomeSnapshotCache,
    image_cache: Mutex<ImageDiskCache>,
    state: Mutex<RuntimeState>,
}

#[derive(Clone)]
struct SessionSnapshot {
    session: MediaStationSession,
    generation: u64,
}

struct NativeLoadRequest<'a> {
    snapshot: &'a SessionSnapshot,
    source: &'a PlaybackSource,
    preference: &'a PlaybackTrackPreference,
    media_id: &'a str,
    start_ms: u64,
    url: &'a CString,
    options: &'a JfnMpvLoadOptions,
    subtitle: Option<PreparedExternalSubtitle>,
    interpolation: Option<InterpolationPlan>,
}

enum TrackSelection {
    Audio {
        mpv_track: i64,
        key: String,
    },
    SubtitleOff,
    SubtitleEmbedded {
        mpv_track: i64,
        key: String,
    },
    SubtitleExternal {
        subtitle: PreparedExternalSubtitle,
        key: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeTrackKind {
    Audio,
    Subtitle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeTrack {
    kind: RuntimeTrackKind,
    key: String,
    mpv_id: Option<i64>,
    codec: Option<String>,
    language: Option<String>,
    label: Option<String>,
    channel_count: Option<u64>,
    selected: bool,
    external: bool,
    is_default: bool,
    is_forced: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RuntimeTrackCatalog {
    audio: Vec<RuntimeTrack>,
    subtitles: Vec<RuntimeTrack>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MpvRuntimeTrack {
    kind: RuntimeTrackKind,
    id: i64,
    source_id: Option<i64>,
    ff_index: Option<i64>,
    codec: Option<String>,
    language: Option<String>,
    title: Option<String>,
    channel_count: Option<u32>,
    sample_rate: Option<u32>,
    selected: bool,
    external: bool,
    is_default: bool,
    is_forced: bool,
}

struct RequestGuard {
    runtime: Arc<MediaStationRuntime>,
    request_id: String,
}

struct AuthRequestGuard {
    runtime: Arc<MediaStationRuntime>,
    request_id: String,
    generation: u64,
}

struct CatalogRequestGuard {
    runtime: Arc<MediaStationRuntime>,
    request_id: String,
}

struct ImageRequestGuard {
    runtime: Arc<MediaStationRuntime>,
    request_id: String,
}

impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.runtime
            .state
            .lock()
            .active_request_ids
            .remove(&self.request_id);
    }
}

impl Drop for AuthRequestGuard {
    fn drop(&mut self) {
        self.runtime
            .state
            .lock()
            .active_auth_request_ids
            .remove(&self.request_id);
    }
}

impl Drop for CatalogRequestGuard {
    fn drop(&mut self) {
        self.runtime
            .state
            .lock()
            .active_catalog_request_ids
            .remove(&self.request_id);
    }
}

impl Drop for ImageRequestGuard {
    fn drop(&mut self) {
        self.runtime
            .state
            .lock()
            .active_image_request_ids
            .remove(&self.request_id);
    }
}

#[derive(Debug)]
struct LoadFailure {
    code: &'static str,
    message: &'static str,
}

impl LoadFailure {
    const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    fn payload(&self) -> Value {
        json!({
            "code": self.code,
            "message": self.message,
        })
    }
}

impl MediaStationRuntime {
    fn new() -> Result<Self, ApiError> {
        let user_agent = format!("MediaStationGoWindows/{APP_VERSION}");
        let api = MediaStationApiClient::new(user_agent.clone())?;
        let reporter = PlaybackReportDispatcher::new(api.clone());
        let cache_root = jfn_paths::cache_dir();
        Ok(Self {
            api,
            resolver: PlaybackSessionResolver::new(UreqTransport::new()),
            user_agent,
            reporter,
            home_cache: HomeSnapshotCache::new(cache_root.join(HOME_CACHE_DIRECTORY)),
            image_cache: Mutex::new(ImageDiskCache::new(
                cache_root.join(IMAGE_CACHE_DIRECTORY),
                MAX_IMAGE_CACHE_BYTES,
            )),
            state: Mutex::new(RuntimeState {
                session: None,
                session_profile: None,
                generation: 0,
                active_request_ids: HashSet::new(),
                active_auth_request_ids: HashSet::new(),
                active_catalog_request_ids: HashSet::new(),
                active_image_request_ids: HashSet::new(),
                active_subtitle: None,
                active_interpolation: None,
                active_report: None,
            }),
        })
    }

    fn configure_session(&self, session: MediaStationSession) -> u64 {
        let user_name = session.user_id.clone();
        self.configure_session_profile(session, user_name, false)
    }

    fn configure_session_profile(
        &self,
        session: MediaStationSession,
        user_name: String,
        persisted: bool,
    ) -> u64 {
        let (generation, stopped) = {
            let mut state = self.state.lock();
            let stopped = if state.session.as_ref() != Some(&session) {
                let stopped = take_stopped_report(&mut state);
                state.generation = state.generation.wrapping_add(1);
                state.session = Some(session);
                state.active_subtitle = None;
                state.active_interpolation = None;
                stopped
            } else {
                None
            };
            state.session_profile = Some(SessionProfile {
                user_name,
                persisted,
            });
            (state.generation, stopped)
        };
        self.enqueue_report(stopped);
        generation
    }

    fn clear_session(&self) -> u64 {
        let (generation, stopped) = {
            let mut state = self.state.lock();
            let stopped = take_stopped_report(&mut state);
            state.generation = state.generation.wrapping_add(1);
            state.session = None;
            state.session_profile = None;
            state.active_subtitle = None;
            state.active_interpolation = None;
            (state.generation, stopped)
        };
        self.enqueue_report(stopped);
        generation
    }

    fn release_playback_resources(&self) {
        let mut state = self.state.lock();
        state.active_subtitle = None;
        state.active_interpolation = None;
    }

    fn handle_playback_event(self: &Arc<Self>, event: &PlaybackEvent) {
        if !matches!(
            event.kind,
            PlaybackEventKind::PositionChanged
                | PlaybackEventKind::DurationChanged
                | PlaybackEventKind::BufferedRangesChanged
                | PlaybackEventKind::RateChanged
        ) {
            log_debug(&format!(
                "MediaStation playback event: kind={} phase={:?} media={:?} buffering={} seeking={}",
                playback_event_kind_label(event.kind),
                event.snapshot.phase,
                event.snapshot.media_type,
                event.snapshot.buffering,
                event.snapshot.seeking
            ));
        }
        let filter_status = (event.kind == PlaybackEventKind::PositionChanged)
            .then(|| mpv_property_string(c"vf-metadata/rife/status"));
        let mut dispatched_event = None;
        let (jobs, reconcile) = {
            let mut state = self.state.lock();
            let filter_failed = filter_status.as_ref().is_some_and(|status| {
                state.active_interpolation.as_mut().is_some_and(|active| {
                    interpolation_filter_failed(
                        &mut active.filter_seen,
                        &mut active.missing_filter_samples,
                        status.as_deref(),
                    )
                })
            });
            if filter_failed {
                let mut failure = event.clone();
                failure.kind = PlaybackEventKind::Error;
                failure.error_message = RIFE_FILTER_INACTIVE.to_string();
                dispatched_event = Some(failure);
            }
            let effective_event = dispatched_event.as_ref().unwrap_or(event);
            if matches!(
                effective_event.kind,
                PlaybackEventKind::Finished
                    | PlaybackEventKind::Canceled
                    | PlaybackEventKind::Error
            ) {
                state.active_subtitle = None;
                state.active_interpolation = None;
            } else if effective_event.kind == PlaybackEventKind::Started
                && let Some(active) = state.active_interpolation.as_ref()
            {
                log_debug(&format!(
                    "RIFE frame interpolation started: media_id={} target_fps={} backend={} runtime={} model={} engine_key={} hwdec={}",
                    active.media_id,
                    active.plan.target_fps,
                    active.plan.backend,
                    active.plan.runtime_version,
                    active.plan.model,
                    active.plan.engine_key,
                    active.plan.hwdec,
                ));
            }
            let reconcile = if effective_event.kind == PlaybackEventKind::Started {
                state.active_report.as_mut().and_then(|active| {
                    if active.runtime_tracks_reconciled {
                        return None;
                    }
                    active.runtime_tracks_reconciled = true;
                    Some(RuntimeTrackReconcile {
                        snapshot: SessionSnapshot {
                            session: active.session.clone(),
                            generation: active.generation,
                        },
                        source: active.source.clone(),
                        preference: active.preference.clone(),
                        preference_revision: active.preference_revision,
                    })
                })
            } else {
                None
            };
            (
                plan_playback_reports(&mut state, effective_event, Instant::now()),
                reconcile,
            )
        };
        for job in jobs {
            self.enqueue_report(Some(job));
        }
        if let Some(reconcile) = reconcile {
            let runtime = Arc::clone(self);
            if let Err(error) = thread::Builder::new()
                .name("mediastation-track-reconcile".to_string())
                .spawn(move || reconcile_runtime_track_preference(&runtime, reconcile))
            {
                log_error(&format!(
                    "MediaStation track reconciliation worker could not start: {error}"
                ));
            }
        }
        if let Some(failure) = dispatched_event.as_ref() {
            log_error("RIFE native filter is missing or inactive; stopping playback");
            dispatch_playback_event(failure);
            jfn_mpv_stop();
        } else {
            dispatch_playback_event(event);
        }
    }

    fn enqueue_report(&self, job: Option<ReportJob>) {
        let Some(job) = job else { return };
        let kind = job.kind;
        let generation = job.generation;
        if let Err(error) = self.reporter.enqueue(job) {
            log_error(&format!(
                "MediaStation playback report could not be queued: kind={} generation={} code={}",
                report_kind_label(kind),
                generation,
                error.code()
            ));
        }
    }

    fn shutdown_resources(&self) {
        let stopped = {
            let mut state = self.state.lock();
            state.active_subtitle = None;
            state.active_interpolation = None;
            take_stopped_report(&mut state)
        };
        self.enqueue_report(stopped);
        self.reporter.shutdown();
    }

    fn begin_request(
        self: &Arc<Self>,
        request_id: &str,
    ) -> Result<(SessionSnapshot, RequestGuard), LoadFailure> {
        let mut state = self.state.lock();
        if state.active_request_ids.contains(request_id) {
            return Err(LoadFailure::new(
                "duplicate_request_id",
                "A request with this identifier is already active",
            ));
        }
        if state.active_request_ids.len() >= MAX_ACTIVE_LOAD_REQUESTS {
            return Err(LoadFailure::new(
                "load_request_in_progress",
                "Another native playback request is still resolving",
            ));
        }
        let session = state.session.clone().ok_or_else(|| {
            LoadFailure::new(
                "session_unavailable",
                "No native MediaStation session is configured",
            )
        })?;
        state.active_request_ids.insert(request_id.to_string());
        Ok((
            SessionSnapshot {
                session,
                generation: state.generation,
            },
            RequestGuard {
                runtime: Arc::clone(self),
                request_id: request_id.to_string(),
            },
        ))
    }

    fn begin_auth_request(
        self: &Arc<Self>,
        request_id: &str,
    ) -> Result<AuthRequestGuard, LoadFailure> {
        let mut state = self.state.lock();
        if state.active_auth_request_ids.contains(request_id) {
            return Err(LoadFailure::new(
                "duplicate_request_id",
                "A request with this identifier is already active",
            ));
        }
        if state.active_auth_request_ids.len() >= MAX_ACTIVE_AUTH_REQUESTS {
            return Err(LoadFailure::new(
                "authentication_in_progress",
                "Another native authentication request is still active",
            ));
        }
        state.active_auth_request_ids.insert(request_id.to_string());
        Ok(AuthRequestGuard {
            runtime: Arc::clone(self),
            request_id: request_id.to_string(),
            generation: state.generation,
        })
    }

    fn begin_catalog_request(
        self: &Arc<Self>,
        request_id: &str,
    ) -> Result<(SessionSnapshot, CatalogRequestGuard), LoadFailure> {
        let mut state = self.state.lock();
        if state.active_catalog_request_ids.contains(request_id) {
            return Err(LoadFailure::new(
                "duplicate_request_id",
                "A request with this identifier is already active",
            ));
        }
        if state.active_catalog_request_ids.len() >= MAX_ACTIVE_CATALOG_REQUESTS {
            return Err(LoadFailure::new(
                "catalog_request_limit",
                "Too many catalog requests are active",
            ));
        }
        let session = state.session.clone().ok_or_else(|| {
            LoadFailure::new(
                "session_unavailable",
                "No native MediaStation session is configured",
            )
        })?;
        state
            .active_catalog_request_ids
            .insert(request_id.to_string());
        Ok((
            SessionSnapshot {
                session,
                generation: state.generation,
            },
            CatalogRequestGuard {
                runtime: Arc::clone(self),
                request_id: request_id.to_string(),
            },
        ))
    }

    fn begin_image_request(
        self: &Arc<Self>,
        request_id: &str,
    ) -> Result<(SessionSnapshot, ImageRequestGuard), LoadFailure> {
        let mut state = self.state.lock();
        if state.active_image_request_ids.contains(request_id) {
            return Err(LoadFailure::new(
                "duplicate_request_id",
                "A request with this identifier is already active",
            ));
        }
        if state.active_image_request_ids.len() >= MAX_ACTIVE_IMAGE_REQUESTS {
            return Err(LoadFailure::new(
                "image_request_limit",
                "Too many image requests are active",
            ));
        }
        let session = state.session.clone().ok_or_else(|| {
            LoadFailure::new(
                "session_unavailable",
                "No native MediaStation session is configured",
            )
        })?;
        state
            .active_image_request_ids
            .insert(request_id.to_string());
        Ok((
            SessionSnapshot {
                session,
                generation: state.generation,
            },
            ImageRequestGuard {
                runtime: Arc::clone(self),
                request_id: request_id.to_string(),
            },
        ))
    }

    fn session_status_payload(&self) -> Value {
        let state = self.state.lock();
        session_status_payload(&state)
    }

    fn commit_persisted_session(
        &self,
        expected_generation: u64,
        session: MediaStationSession,
        user_name: String,
        stored: &StoredSession,
    ) -> Result<Value, LoadFailure> {
        let (payload, stopped) = {
            let mut state = self.state.lock();
            if state.generation != expected_generation {
                return Err(session_changed());
            }
            persist_active_session(stored)?;
            let stopped = take_stopped_report(&mut state);
            state.generation = state.generation.wrapping_add(1);
            state.session = Some(session);
            state.session_profile = Some(SessionProfile {
                user_name,
                persisted: true,
            });
            state.active_subtitle = None;
            state.active_interpolation = None;
            (session_status_payload(&state), stopped)
        };
        self.enqueue_report(stopped);
        Ok(payload)
    }

    fn logout_persisted_session(&self) -> Result<bool, LoadFailure> {
        let (deleted, stopped) = {
            let mut state = self.state.lock();
            let deleted = WindowsCredentialStore::active().delete().map_err(|error| {
                log_error(&format!(
                    "MediaStation credential deletion failed: code={}",
                    error.code()
                ));
                LoadFailure::new(
                    error.code(),
                    "The persisted MediaStation session could not be removed",
                )
            })?;
            let stopped = take_stopped_report(&mut state);
            state.generation = state.generation.wrapping_add(1);
            state.session = None;
            state.session_profile = None;
            state.active_subtitle = None;
            state.active_interpolation = None;
            (deleted, stopped)
        };
        self.enqueue_report(stopped);
        Ok(deleted)
    }

    fn ensure_generation(&self, generation: u64) -> Result<(), LoadFailure> {
        let state = self.state.lock();
        if state.generation != generation || state.session.is_none() {
            return Err(session_changed());
        }
        Ok(())
    }

    fn load_if_current(&self, load: NativeLoadRequest<'_>) -> Result<(), LoadFailure> {
        let mut state = self.state.lock();
        if state.generation != load.snapshot.generation || state.session.is_none() {
            return Err(session_changed());
        }
        if jfn_mpv_handle_get().is_null() {
            return Err(LoadFailure::new(
                "player_unavailable",
                "The native player is not initialized",
            ));
        }

        pb_post(PbInput::LoadStarting(load.media_id.to_string()));
        pb_post(PbInput::MediaType(if load.source.video.is_some() {
            PbMediaType::Video
        } else {
            PbMediaType::Unknown
        }));
        pb_post(PbInput::Position(
            i64::try_from(load.start_ms.saturating_mul(1000)).unwrap_or(i64::MAX),
        ));
        if let Err(error) = unsafe { jfn_mpv_load_file(load.url.as_ptr(), load.options) } {
            log_mpv_error(&error);
            pb_post(PbInput::EndFile {
                reason: EndReason::Error,
                error_message: error.to_string(),
            });
            return Err(LoadFailure::new(
                "player_load_failed",
                "The native player rejected the load request",
            ));
        }
        state.active_subtitle = load.subtitle;
        state.active_interpolation = load.interpolation.map(|plan| ActiveInterpolation {
            media_id: load.media_id.to_string(),
            plan,
            filter_seen: false,
            missing_filter_samples: 0,
        });
        let stopped = activate_playback_report(
            &mut state,
            load.snapshot,
            load.source,
            load.preference,
            load.start_ms,
        );
        drop(state);
        self.enqueue_report(stopped);
        Ok(())
    }

    fn active_playback_source(
        &self,
        snapshot: &SessionSnapshot,
        media_id: &str,
    ) -> Result<PlaybackSource, LoadFailure> {
        let state = self.state.lock();
        if state.generation != snapshot.generation || state.session.is_none() {
            return Err(session_changed());
        }
        let active = state.active_report.as_ref().ok_or_else(|| {
            LoadFailure::new(
                "playback_unavailable",
                "No active playback can change tracks",
            )
        })?;
        if active.source.media_id != media_id {
            return Err(LoadFailure::new(
                "playback_changed",
                "The active playback item has changed",
            ));
        }
        Ok(active.source.clone())
    }

    fn active_preference_revision(&self, snapshot: &SessionSnapshot) -> Result<u64, LoadFailure> {
        let state = self.state.lock();
        if state.generation != snapshot.generation || state.session.is_none() {
            return Err(session_changed());
        }
        let active = state.active_report.as_ref().ok_or_else(|| {
            LoadFailure::new(
                "playback_unavailable",
                "No active playback can change tracks",
            )
        })?;
        Ok(active.preference_revision)
    }

    fn bump_preference_revision(&self, snapshot: &SessionSnapshot) {
        let mut state = self.state.lock();
        if state.generation != snapshot.generation || state.session.is_none() {
            return;
        }
        if let Some(active) = state.active_report.as_mut() {
            active.preference_revision = active.preference_revision.wrapping_add(1);
        }
    }

    fn apply_track_selection(
        &self,
        snapshot: &SessionSnapshot,
        media_id: &str,
        selection: TrackSelection,
    ) -> Result<Value, LoadFailure> {
        let mut state = self.state.lock();
        if state.generation != snapshot.generation || state.session.is_none() {
            return Err(session_changed());
        }
        let active = state.active_report.as_ref().ok_or_else(|| {
            LoadFailure::new(
                "playback_unavailable",
                "No active playback can change tracks",
            )
        })?;
        if active.source.media_id != media_id {
            return Err(LoadFailure::new(
                "playback_changed",
                "The active playback item has changed",
            ));
        }
        if jfn_mpv_handle_get().is_null() {
            return Err(LoadFailure::new(
                "player_unavailable",
                "The native player is not initialized",
            ));
        }

        let payload = match selection {
            TrackSelection::Audio { mpv_track, key } => {
                jfn_mpv_set_audio_track_checked(mpv_track)
                    .map_err(|error| player_track_selection_failure("audio", &error))?;
                json!({
                    "state": "requested",
                    "audioTrackKey": key,
                })
            }
            TrackSelection::SubtitleOff => {
                let previous = state
                    .active_subtitle
                    .as_ref()
                    .map(|subtitle| subtitle.path.clone());
                if previous.is_some() {
                    jfn_mpv_sub_remove_current_checked().map_err(|error| {
                        player_track_selection_failure("subtitle_remove", &error)
                    })?;
                }
                if let Err(error) = jfn_mpv_set_subtitle_track_checked(0) {
                    restore_external_subtitle(previous.as_deref());
                    return Err(player_track_selection_failure("subtitle_disable", &error));
                }
                state.active_subtitle = None;
                json!({
                    "state": "requested",
                    "subtitleEnabled": false,
                    "subtitleTrackKey": Value::Null,
                })
            }
            TrackSelection::SubtitleEmbedded { mpv_track, key } => {
                let previous = state
                    .active_subtitle
                    .as_ref()
                    .map(|subtitle| subtitle.path.clone());
                if previous.is_some() {
                    jfn_mpv_sub_remove_current_checked().map_err(|error| {
                        player_track_selection_failure("subtitle_remove", &error)
                    })?;
                }
                if let Err(error) = jfn_mpv_set_subtitle_track_checked(mpv_track) {
                    restore_external_subtitle(previous.as_deref());
                    return Err(player_track_selection_failure("subtitle_embedded", &error));
                }
                state.active_subtitle = None;
                json!({
                    "state": "requested",
                    "subtitleEnabled": true,
                    "subtitleTrackKey": key,
                })
            }
            TrackSelection::SubtitleExternal { subtitle, key } => {
                let previous = state
                    .active_subtitle
                    .as_ref()
                    .map(|active| active.path.clone());
                if previous.is_some() {
                    jfn_mpv_sub_remove_current_checked().map_err(|error| {
                        player_track_selection_failure("subtitle_remove", &error)
                    })?;
                }
                if let Err(error) = jfn_mpv_sub_add_checked(&subtitle.path) {
                    restore_external_subtitle(previous.as_deref());
                    return Err(player_track_selection_failure("subtitle_external", &error));
                }
                state.active_subtitle = Some(subtitle);
                json!({
                    "state": "requested",
                    "subtitleEnabled": true,
                    "subtitleTrackKey": key,
                })
            }
        };
        Ok(payload)
    }
}

fn restore_external_subtitle(path: Option<&std::ffi::CStr>) {
    let Some(path) = path else { return };
    if let Err(error) = jfn_mpv_sub_add_checked(path) {
        log_error(&format!(
            "MediaStation external subtitle restore failed: mpv_code={}",
            error.code
        ));
    }
}

fn player_track_selection_failure(action: &str, error: &jfn_mpv::Error) -> LoadFailure {
    log_error(&format!(
        "MediaStation player track selection failed: action={action} mpv_code={}",
        error.code
    ));
    LoadFailure::new(
        "player_track_selection_failed",
        "The native player rejected the requested track",
    )
}

fn session_status_payload(state: &RuntimeState) -> Value {
    let Some(session) = state.session.as_ref() else {
        return json!({
            "configured": false,
            "baseUrl": jfn_config::server_url(),
        });
    };
    let profile = state.session_profile.as_ref();
    json!({
        "configured": true,
        "persisted": profile.is_some_and(|profile| profile.persisted),
        "baseUrl": session.base_url.as_str(),
        "userId": session.user_id,
        "userName": profile.map(|profile| profile.user_name.as_str()).unwrap_or(&session.user_id),
    })
}

fn activate_playback_report(
    state: &mut RuntimeState,
    snapshot: &SessionSnapshot,
    source: &PlaybackSource,
    preference: &PlaybackTrackPreference,
    start_ms: u64,
) -> Option<ReportJob> {
    let stopped = take_stopped_report(state);
    state.active_report = Some(ActivePlaybackReport {
        generation: snapshot.generation,
        session: snapshot.session.clone(),
        source: source.clone(),
        preference: preference.clone(),
        preference_revision: 0,
        runtime_tracks_reconciled: false,
        started: false,
        paused: false,
        last_position_ms: start_ms,
        last_report_at: None,
    });
    stopped
}

fn plan_playback_reports(
    state: &mut RuntimeState,
    event: &PlaybackEvent,
    now: Instant,
) -> Vec<ReportJob> {
    if matches!(
        event.kind,
        PlaybackEventKind::Finished | PlaybackEventKind::Canceled | PlaybackEventKind::Error
    ) {
        return take_stopped_report(state).into_iter().collect();
    }
    let Some(active) = state.active_report.as_mut() else {
        return Vec::new();
    };
    let event_position_ms = event.snapshot.position_us.max(0) as u64 / 1000;
    match event.kind {
        PlaybackEventKind::Started => {
            active.last_position_ms = event_position_ms;
            let kind = if !active.started {
                active.started = true;
                ReportKind::Playing
            } else if active.paused {
                ReportKind::Progress { paused: false }
            } else {
                return Vec::new();
            };
            active.paused = false;
            active.last_report_at = Some(now);
            vec![report_job(active, kind)]
        }
        PlaybackEventKind::Paused => {
            active.last_position_ms = event_position_ms;
            active.paused = true;
            if active.started {
                active.last_report_at = Some(now);
                vec![report_job(active, ReportKind::Progress { paused: true })]
            } else {
                Vec::new()
            }
        }
        PlaybackEventKind::PositionChanged => {
            active.last_position_ms = event_position_ms;
            let report_due = active.started
                && active.last_report_at.is_some_and(|last| {
                    now.saturating_duration_since(last) >= PROGRESS_REPORT_INTERVAL
                });
            if report_due {
                active.last_report_at = Some(now);
                vec![report_job(
                    active,
                    ReportKind::Progress {
                        paused: active.paused,
                    },
                )]
            } else {
                Vec::new()
            }
        }
        PlaybackEventKind::Seeked => {
            active.last_position_ms = event_position_ms;
            if active.started {
                active.last_report_at = Some(now);
                vec![report_job(
                    active,
                    ReportKind::Progress {
                        paused: active.paused,
                    },
                )]
            } else {
                Vec::new()
            }
        }
        PlaybackEventKind::Finished
        | PlaybackEventKind::Canceled
        | PlaybackEventKind::Error
        | PlaybackEventKind::SeekingChanged
        | PlaybackEventKind::BufferingChanged
        | PlaybackEventKind::MediaTypeChanged
        | PlaybackEventKind::TrackLoaded
        | PlaybackEventKind::DurationChanged
        | PlaybackEventKind::RateChanged
        | PlaybackEventKind::FullscreenChanged
        | PlaybackEventKind::BufferedRangesChanged
        | PlaybackEventKind::DisplayHzChanged
        | PlaybackEventKind::MetadataChanged
        | PlaybackEventKind::ArtworkChanged
        | PlaybackEventKind::QueueCapsChanged => Vec::new(),
    }
}

fn take_stopped_report(state: &mut RuntimeState) -> Option<ReportJob> {
    let active = state.active_report.take()?;
    active
        .started
        .then(|| report_job(&active, ReportKind::Stopped))
}

fn report_job(active: &ActivePlaybackReport, kind: ReportKind) -> ReportJob {
    ReportJob {
        generation: active.generation,
        session: active.session.clone(),
        source: active.source.clone(),
        kind,
        position_ms: active.last_position_ms,
    }
}

static RUNTIME: OnceLock<Result<Arc<MediaStationRuntime>, ()>> = OnceLock::new();

fn runtime() -> Result<Arc<MediaStationRuntime>, LoadFailure> {
    match RUNTIME.get_or_init(|| {
        let runtime = Arc::new(MediaStationRuntime::new().map_err(|_| ())?);
        install_playback_cleanup(Arc::downgrade(&runtime));
        Ok(runtime)
    }) {
        Ok(runtime) => Ok(Arc::clone(runtime)),
        Err(()) => Err(LoadFailure::new(
            "runtime_initialization_failed",
            "The native MediaStation runtime could not be initialized",
        )),
    }
}

fn install_playback_cleanup(runtime: Weak<MediaStationRuntime>) {
    if !jfn_playback::register_event_sink(Box::new(move |event| {
        if let Some(runtime) = runtime.upgrade() {
            runtime.handle_playback_event(event);
        }
    })) {
        log_error("MediaStation playback event sink registration failed: coordinator unavailable");
    }
}

pub(crate) fn release_playback_resources() {
    if let Some(Ok(runtime)) = RUNTIME.get() {
        runtime.release_playback_resources();
    }
}

pub(crate) fn shutdown_playback_resources() {
    if let Some(Ok(runtime)) = RUNTIME.get() {
        runtime.shutdown_resources();
    }
}

/// Configure credentials from native code. This API is deliberately not
/// exposed through the renderer bridge, so tokens never enter JavaScript.
pub fn jfn_web_configure_mediastation_session(session: MediaStationSession) -> Result<u64, String> {
    runtime()
        .map(|runtime| runtime.configure_session(session))
        .map_err(|error| error.message.to_string())
}

/// Clear the native session and invalidate all in-flight request generations.
pub fn jfn_web_clear_mediastation_session() -> Result<u64, String> {
    runtime()
        .map(|runtime| runtime.clear_session())
        .map_err(|error| error.message.to_string())
}

pub(crate) fn session_is_configured() -> bool {
    runtime()
        .map(|runtime| runtime.state.lock().session.is_some())
        .unwrap_or(false)
}

pub(crate) fn restore_persisted_session_on_startup() -> Result<bool, String> {
    let store = WindowsCredentialStore::active();
    let Some(stored) = store.load().map_err(|error| error.code().to_string())? else {
        return Ok(false);
    };
    let configured_url =
        parse_server_url(&jfn_config::server_url()).map_err(|failure| failure.code.to_string())?;
    if configured_url != stored.base_url {
        return Err("credential_server_mismatch".to_string());
    }
    let authorization = native_authorization_header(Some(stored.access_token_secret()))
        .map_err(|failure| failure.code.to_string())?;
    let session = stored
        .to_session(authorization)
        .map_err(|error| api_error_code(&error).to_string())?;
    runtime()
        .map(|runtime| {
            runtime.configure_session_profile(session, stored.user_name, true);
            true
        })
        .map_err(|failure| failure.code.to_string())
}

pub(crate) fn handle_authenticate_message(
    layer: Option<Arc<Inner>>,
    args: Option<&ListValue>,
) -> bool {
    let Some(layer) = layer else {
        log_error("MediaStation authentication rejected: web layer unavailable");
        return true;
    };
    let request = match parse_auth_request(args) {
        Ok(request) => request,
        Err((request_id, failure)) => {
            dispatch_response(
                &layer,
                &request_id,
                OPERATION_AUTHENTICATE,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_AUTHENTICATE,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let guard = match runtime.begin_auth_request(&request.request_id) {
        Ok(guard) => guard,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_AUTHENTICATE,
                false,
                failure.payload(),
            );
            return true;
        }
    };

    let request_id = request.request_id.clone();
    let spawn_request_id = request_id.clone();
    let expected_generation = guard.generation;
    let worker_layer = Arc::clone(&layer);
    let worker_runtime = Arc::clone(&runtime);
    let spawn = thread::Builder::new()
        .name("mediastation-authentication".to_string())
        .spawn(move || {
            let _guard = guard;
            match execute_authentication(&worker_runtime, expected_generation, &request) {
                Ok(payload) => dispatch_response(
                    &worker_layer,
                    &request_id,
                    OPERATION_AUTHENTICATE,
                    true,
                    payload,
                ),
                Err(failure) => dispatch_response(
                    &worker_layer,
                    &request_id,
                    OPERATION_AUTHENTICATE,
                    false,
                    failure.payload(),
                ),
            }
        });
    if let Err(error) = spawn {
        log_error(&format!(
            "MediaStation authentication worker could not start: kind={}",
            error.kind()
        ));
        dispatch_response(
            &layer,
            &spawn_request_id,
            OPERATION_AUTHENTICATE,
            false,
            LoadFailure::new(
                "request_thread_failed",
                "The native authentication worker could not be started",
            )
            .payload(),
        );
    }
    true
}

pub(crate) fn handle_session_status_message(
    layer: Option<Arc<Inner>>,
    args: Option<&ListValue>,
) -> bool {
    let Some(layer) = layer else {
        log_error("MediaStation session status rejected: web layer unavailable");
        return true;
    };
    let request_id = match parse_request_id(args) {
        Ok(request_id) => request_id,
        Err((request_id, failure)) => {
            dispatch_response(
                &layer,
                &request_id,
                OPERATION_SESSION_STATUS,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    match runtime() {
        Ok(runtime) => dispatch_response(
            &layer,
            &request_id,
            OPERATION_SESSION_STATUS,
            true,
            runtime.session_status_payload(),
        ),
        Err(failure) => dispatch_response(
            &layer,
            &request_id,
            OPERATION_SESSION_STATUS,
            false,
            failure.payload(),
        ),
    }
    true
}

pub(crate) fn handle_logout_message(layer: Option<Arc<Inner>>, args: Option<&ListValue>) -> bool {
    let Some(layer) = layer else {
        log_error("MediaStation logout rejected: web layer unavailable");
        return true;
    };
    let request_id = match parse_request_id(args) {
        Ok(request_id) => request_id,
        Err((request_id, failure)) => {
            dispatch_response(
                &layer,
                &request_id,
                OPERATION_LOGOUT,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request_id,
                OPERATION_LOGOUT,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    match runtime.logout_persisted_session() {
        Ok(deleted) => dispatch_response(
            &layer,
            &request_id,
            OPERATION_LOGOUT,
            true,
            json!({ "configured": false, "credentialDeleted": deleted }),
        ),
        Err(failure) => dispatch_response(
            &layer,
            &request_id,
            OPERATION_LOGOUT,
            false,
            failure.payload(),
        ),
    }
    true
}

pub(crate) fn handle_catalog_message(layer: Option<Arc<Inner>>, args: Option<&ListValue>) -> bool {
    let Some(layer) = layer else {
        log_error("MediaStation catalog request rejected: web layer unavailable");
        return true;
    };
    let request = match parse_catalog_request(args) {
        Ok(request) => request,
        Err((request_id, operation, failure)) => {
            dispatch_response(&layer, &request_id, operation, false, failure.payload());
            return true;
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                request.operation.label(),
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let (snapshot, guard) = match runtime.begin_catalog_request(&request.request_id) {
        Ok(active) => active,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                request.operation.label(),
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let request_id = request.request_id.clone();
    let operation = request.operation.label();
    let spawn_request_id = request_id.clone();
    let worker_layer = Arc::clone(&layer);
    let worker_runtime = Arc::clone(&runtime);
    let spawn = thread::Builder::new()
        .name(format!("mediastation-{operation}"))
        .spawn(move || {
            let _guard = guard;
            match execute_catalog_request(&worker_runtime, &snapshot, &request) {
                Ok(payload) => {
                    dispatch_response(&worker_layer, &request_id, operation, true, payload)
                }
                Err(failure) => dispatch_response(
                    &worker_layer,
                    &request_id,
                    operation,
                    false,
                    failure.payload(),
                ),
            }
        });
    if let Err(error) = spawn {
        log_error(&format!(
            "MediaStation catalog worker could not start: operation={operation} kind={}",
            error.kind()
        ));
        dispatch_response(
            &layer,
            &spawn_request_id,
            operation,
            false,
            LoadFailure::new(
                "request_thread_failed",
                "The native catalog worker could not be started",
            )
            .payload(),
        );
    }
    true
}

pub(crate) fn handle_image_message(layer: Option<Arc<Inner>>, args: Option<&ListValue>) -> bool {
    let Some(layer) = layer else {
        log_error("MediaStation image request rejected: web layer unavailable");
        return true;
    };
    let request = match parse_image_request(args) {
        Ok(request) => request,
        Err((request_id, failure)) => {
            dispatch_response(
                &layer,
                &request_id,
                OPERATION_IMAGE,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_IMAGE,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let (snapshot, guard) = match runtime.begin_image_request(&request.request_id) {
        Ok(active) => active,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_IMAGE,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let request_id = request.request_id.clone();
    let spawn_request_id = request_id.clone();
    let worker_layer = Arc::clone(&layer);
    let worker_runtime = Arc::clone(&runtime);
    let spawn = thread::Builder::new()
        .name("mediastation-image".to_string())
        .spawn(move || {
            let _guard = guard;
            match execute_image_request(&worker_runtime, &snapshot, &request) {
                Ok(payload) => {
                    dispatch_response(&worker_layer, &request_id, OPERATION_IMAGE, true, payload)
                }
                Err(failure) => dispatch_response(
                    &worker_layer,
                    &request_id,
                    OPERATION_IMAGE,
                    false,
                    failure.payload(),
                ),
            }
        });
    if let Err(error) = spawn {
        log_error(&format!(
            "MediaStation image worker could not start: kind={}",
            error.kind()
        ));
        dispatch_response(
            &layer,
            &spawn_request_id,
            OPERATION_IMAGE,
            false,
            LoadFailure::new(
                "request_thread_failed",
                "The native image worker could not be started",
            )
            .payload(),
        );
    }
    true
}

#[derive(Clone, Copy)]
enum CatalogOperation {
    Home,
    Items,
    Detail,
    Search,
    CacheStats,
    ClearImageCache,
}

impl CatalogOperation {
    const fn label(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Items => "items",
            Self::Detail => "detail",
            Self::Search => "search",
            Self::CacheStats => "cache_stats",
            Self::ClearImageCache => "clear_image_cache",
        }
    }
}

struct CatalogRequest {
    request_id: String,
    operation: CatalogOperation,
    params: Value,
}

struct ImageRequest {
    request_id: String,
    key: String,
    image: MediaImageRef,
    max_width: u32,
}

fn parse_catalog_request(
    args: Option<&ListValue>,
) -> Result<CatalogRequest, (String, &'static str, LoadFailure)> {
    let request_id =
        parse_request_id(args).map_err(|(request_id, failure)| (request_id, "catalog", failure))?;
    let Some(args) = args else {
        unreachable!("parse_request_id rejects missing arguments")
    };
    if args.size() < 3
        || args.get_type(1).as_ref() != &sys::cef_value_type_t::VTYPE_STRING
        || args.get_type(2).as_ref() != &sys::cef_value_type_t::VTYPE_STRING
    {
        return Err((
            request_id,
            "catalog",
            LoadFailure::new(
                "invalid_request",
                "Catalog arguments are missing or invalid",
            ),
        ));
    }
    let raw_operation = list_string(args, 1);
    let operation = match raw_operation.as_str() {
        "home" => CatalogOperation::Home,
        "items" => CatalogOperation::Items,
        "detail" => CatalogOperation::Detail,
        "search" => CatalogOperation::Search,
        "cache_stats" => CatalogOperation::CacheStats,
        "clear_image_cache" => CatalogOperation::ClearImageCache,
        _ => {
            return Err((
                request_id,
                "catalog",
                LoadFailure::new("invalid_operation", "The catalog operation is unsupported"),
            ));
        }
    };
    let raw_params = list_string(args, 2);
    if raw_params.len() > 8_192 {
        return Err((
            request_id,
            operation.label(),
            LoadFailure::new("invalid_request", "The catalog request is too large"),
        ));
    }
    let params = serde_json::from_str::<Value>(&raw_params).map_err(|_| {
        (
            request_id.clone(),
            operation.label(),
            LoadFailure::new("invalid_request", "The catalog request is not valid JSON"),
        )
    })?;
    if !params.is_object() {
        return Err((
            request_id,
            operation.label(),
            LoadFailure::new(
                "invalid_request",
                "The catalog request must be a JSON object",
            ),
        ));
    }
    Ok(CatalogRequest {
        request_id,
        operation,
        params,
    })
}

fn parse_image_request(args: Option<&ListValue>) -> Result<ImageRequest, (String, LoadFailure)> {
    let request_id = parse_request_id(args)?;
    let Some(args) = args else {
        unreachable!("parse_request_id rejects missing arguments")
    };
    if args.size() < 3
        || args.get_type(1).as_ref() != &sys::cef_value_type_t::VTYPE_STRING
        || args.get_type(2).as_ref() != &sys::cef_value_type_t::VTYPE_INT
    {
        return Err((
            request_id,
            LoadFailure::new(
                "invalid_image_request",
                "Image arguments are missing or invalid",
            ),
        ));
    }
    let raw = list_string(args, 1);
    let (key, image, max_width) = parse_image_descriptor(&raw, args.int(2))
        .map_err(|failure| (request_id.clone(), failure))?;
    Ok(ImageRequest {
        request_id,
        key,
        image,
        max_width,
    })
}

fn parse_image_descriptor(
    raw: &str,
    raw_width: i32,
) -> Result<(String, MediaImageRef, u32), LoadFailure> {
    if raw.len() > 4_096 {
        return Err(LoadFailure::new(
            "invalid_image_request",
            "The image request is too large",
        ));
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| {
        LoadFailure::new(
            "invalid_image_request",
            "The image request is not valid JSON",
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        LoadFailure::new(
            "invalid_image_request",
            "The image request must be an object",
        )
    })?;
    let required = |field: &str| {
        object
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                LoadFailure::new(
                    "invalid_image_request",
                    "The image descriptor is incomplete",
                )
            })
    };
    let key = required("key")?;
    let item_id = required("itemId")?;
    let tag = required("tag")?;
    let image_type = match required("type")?.as_str() {
        "primary" => MediaImageType::Primary,
        "thumb" => MediaImageType::Thumb,
        "backdrop" => MediaImageType::Backdrop,
        _ => {
            return Err(LoadFailure::new(
                "invalid_image_request",
                "The image type is unsupported",
            ));
        }
    };
    let image_index = object
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let max_width = u32::try_from(raw_width).unwrap_or(0);
    if !(80..=2_000).contains(&max_width) {
        return Err(LoadFailure::new(
            "invalid_image_width",
            "The image width is outside the allowed range",
        ));
    }
    Ok((
        key,
        MediaImageRef {
            item_id,
            image_type,
            image_index,
            tag,
        },
        max_width,
    ))
}

fn execute_catalog_request(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    request: &CatalogRequest,
) -> Result<Value, LoadFailure> {
    runtime.ensure_generation(snapshot.generation)?;
    let payload = match request.operation {
        CatalogOperation::Home => {
            let refresh = bool_param(&request.params, "refresh", false)?;
            if !refresh {
                match runtime.home_cache.load(&snapshot.session) {
                    Ok(Some(cached)) => {
                        log_debug(&format!(
                            "MediaStation home snapshot hit: age_ms={}",
                            unix_time_ms().saturating_sub(cached.saved_at_ms)
                        ));
                        runtime.ensure_generation(snapshot.generation)?;
                        return Ok(with_home_cache_status(
                            cached.payload,
                            "hit",
                            Some(cached.saved_at_ms),
                            false,
                        ));
                    }
                    Ok(None) => log_debug("MediaStation home snapshot miss"),
                    Err(error) => {
                        log_error(&format!("MediaStation home snapshot read failed: {error}"));
                        if let Err(remove_error) = runtime.home_cache.remove(&snapshot.session) {
                            log_error(&format!(
                                "MediaStation invalid home snapshot could not be removed: {remove_error}"
                            ));
                        }
                    }
                }
            }
            let payload = home_payload(
                &runtime
                    .api
                    .load_home(&snapshot.session)
                    .map_err(|error| catalog_failure(request.operation, &error))?,
            );
            let cache_write_failed =
                if let Err(error) = runtime.home_cache.store(&snapshot.session, &payload) {
                    log_error(&format!("MediaStation home snapshot write failed: {error}"));
                    true
                } else {
                    false
                };
            with_home_cache_status(
                payload,
                if refresh { "refreshed" } else { "miss" },
                None,
                cache_write_failed,
            )
        }
        CatalogOperation::Items => {
            let parent_id = required_param(&request.params, "parentId")?;
            let start_index = usize_param(&request.params, "startIndex", 0)?;
            let limit = usize_param(&request.params, "limit", 60)?;
            page_payload(
                &runtime
                    .api
                    .load_library_page(&snapshot.session, &parent_id, start_index, limit)
                    .map_err(|error| catalog_failure(request.operation, &error))?,
            )
        }
        CatalogOperation::Detail => {
            let media_id = required_param(&request.params, "mediaId")?;
            detail_payload(
                &runtime
                    .api
                    .load_media_detail(&snapshot.session, &media_id)
                    .map_err(|error| catalog_failure(request.operation, &error))?,
            )
        }
        CatalogOperation::Search => {
            let query = required_param(&request.params, "query")?;
            let limit = usize_param(&request.params, "limit", 60)?;
            json!({
                "items": runtime
                    .api
                    .search_media(&snapshot.session, &query, limit)
                    .map_err(|error| catalog_failure(request.operation, &error))?
                    .iter()
                    .map(media_card_payload)
                    .collect::<Vec<_>>()
            })
        }
        CatalogOperation::CacheStats => image_cache_stats_payload(
            runtime
                .image_cache
                .lock()
                .stats()
                .map_err(|error| cache_failure("cache_stats_failed", &error))?,
        ),
        CatalogOperation::ClearImageCache => image_cache_stats_payload(
            runtime
                .image_cache
                .lock()
                .clear()
                .map_err(|error| cache_failure("cache_clear_failed", &error))?,
        ),
    };
    runtime.ensure_generation(snapshot.generation)?;
    Ok(payload)
}

fn execute_image_request(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    request: &ImageRequest,
) -> Result<Value, LoadFailure> {
    runtime.ensure_generation(snapshot.generation)?;
    let cache_key = image_cache_key(&snapshot.session, &request.image, request.max_width);
    let cache_lookup = runtime.image_cache.lock().get(&cache_key);
    let (image, cache_status, cache_write_failed) = match cache_lookup {
        Ok(Some(image)) => (image, "disk", false),
        Ok(None) => download_and_cache_image(runtime, snapshot, request, &cache_key)?,
        Err(error) => {
            log_error(&format!("MediaStation image cache read failed: {error}"));
            if let Err(remove_error) = runtime.image_cache.lock().remove(&cache_key) {
                log_error(&format!(
                    "MediaStation invalid image cache entry could not be removed: {remove_error}"
                ));
            }
            download_and_cache_image(runtime, snapshot, request, &cache_key)?
        }
    };
    runtime.ensure_generation(snapshot.generation)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(image.bytes);
    Ok(json!({
        "key": request.key,
        "dataUrl": format!("data:{};base64,{encoded}", image.content_type),
        "cache": cache_status,
        "cacheWriteFailed": cache_write_failed,
    }))
}

fn download_and_cache_image(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    request: &ImageRequest,
    cache_key: &str,
) -> Result<(jfn_mediastation::MediaImage, &'static str, bool), LoadFailure> {
    let image = runtime
        .api
        .download_media_image(
            &snapshot.session,
            &request.image,
            request.max_width,
            MAX_IMAGE_BYTES,
        )
        .map_err(|error| {
            log_error(&format!(
                "MediaStation image request failed: code={}",
                api_error_code(&error)
            ));
            LoadFailure::new("image_load_failed", "The media image could not be loaded")
        })?;
    let cache_write_failed = if let Err(error) = runtime.image_cache.lock().put(cache_key, &image) {
        log_error(&format!("MediaStation image cache write failed: {error}"));
        true
    } else {
        false
    };
    Ok((image, "network", cache_write_failed))
}

fn required_param(params: &Value, field: &'static str) -> Result<String, LoadFailure> {
    params
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .map(str::to_string)
        .ok_or_else(|| LoadFailure::new("invalid_request", "A required request field is invalid"))
}

fn bool_param(params: &Value, field: &'static str, default: bool) -> Result<bool, LoadFailure> {
    match params.get(field) {
        None => Ok(default),
        Some(value) => value.as_bool().ok_or_else(|| {
            LoadFailure::new(
                "invalid_request",
                "A catalog boolean parameter has an invalid type",
            )
        }),
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn with_home_cache_status(
    mut payload: Value,
    status: &'static str,
    saved_at_ms: Option<u64>,
    write_failed: bool,
) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "cache".to_string(),
            json!({
                "status": status,
                "savedAtMs": saved_at_ms,
                "writeFailed": write_failed,
            }),
        );
    }
    payload
}

fn image_cache_stats_payload(stats: crate::mediastation_cache::ImageCacheStats) -> Value {
    json!({
        "imageBytes": stats.bytes,
        "imageCount": stats.count,
    })
}

fn cache_failure(code: &'static str, error: &impl std::fmt::Display) -> LoadFailure {
    log_error(&format!(
        "MediaStation cache operation failed: code={code} error={error}"
    ));
    LoadFailure::new(code, "The MediaStation cache operation failed")
}

fn usize_param(params: &Value, field: &'static str, default: usize) -> Result<usize, LoadFailure> {
    params
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    LoadFailure::new("invalid_request", "A numeric request field is invalid")
                })
        })
        .unwrap_or(Ok(default))
}

fn catalog_failure(operation: CatalogOperation, error: &ApiError) -> LoadFailure {
    log_error(&format!(
        "MediaStation catalog request failed: operation={} code={}",
        operation.label(),
        api_error_code(error)
    ));
    LoadFailure::new(
        "catalog_load_failed",
        "The MediaStation catalog request failed",
    )
}

fn home_payload(home: &MediaHome) -> Value {
    json!({
        "libraries": home.libraries.iter().map(media_card_payload).collect::<Vec<_>>(),
        "resume": home.resume.iter().map(media_card_payload).collect::<Vec<_>>(),
        "latest": home.latest.iter().map(media_card_payload).collect::<Vec<_>>(),
        "latestSections": home.latest_by_library.iter().map(|section| json!({
            "library": media_card_payload(&section.library),
            "items": section.items.iter().map(media_card_payload).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn page_payload(page: &MediaPage) -> Value {
    json!({
        "items": page.items.iter().map(media_card_payload).collect::<Vec<_>>(),
        "startIndex": page.start_index,
        "nextStartIndex": page.start_index.saturating_add(page.items.len()),
        "totalRecordCount": page.total_record_count,
    })
}

fn detail_payload(detail: &MediaDetail) -> Value {
    json!({
        "item": media_card_payload(&detail.item),
        "episodes": detail.episodes.iter().map(media_card_payload).collect::<Vec<_>>(),
    })
}

fn media_card_payload(card: &MediaCard) -> Value {
    json!({
        "id": card.id,
        "title": card.title,
        "type": card.media_type,
        "overview": card.overview,
        "collectionType": card.collection_type,
        "year": card.year,
        "durationMs": card.duration_ms,
        "resumePositionMs": card.resume_position_ms,
        "played": card.played,
        "indexNumber": card.index_number,
        "parentIndexNumber": card.parent_index_number,
        "parentId": card.parent_id,
        "seasonId": card.season_id,
        "seriesId": card.series_id,
        "seriesName": card.series_name,
        "communityRating": card.community_rating,
        "officialRating": card.official_rating,
        "genres": card.genres,
        "videoCodec": card.video_codec,
        "videoProfile": card.video_profile,
        "videoWidth": card.video_width,
        "videoHeight": card.video_height,
        "dynamicRange": card.dynamic_range,
        "playable": card.is_playable(),
        "primaryImage": card.primary_image.as_ref().map(image_ref_payload),
        "landscapeImage": card.landscape_image.as_ref().map(image_ref_payload),
        "backdropImage": card.backdrop_image.as_ref().map(image_ref_payload),
    })
}

fn image_ref_payload(image: &MediaImageRef) -> Value {
    let image_type = match image.image_type {
        MediaImageType::Primary => "primary",
        MediaImageType::Thumb => "thumb",
        MediaImageType::Backdrop => "backdrop",
    };
    let index = image.image_index.unwrap_or(0);
    json!({
        "key": format!("{}:{image_type}:{index}:{}", image.item_id, image.tag),
        "itemId": image.item_id,
        "type": image_type,
        "index": image.image_index,
        "tag": image.tag,
    })
}

struct AuthRequest {
    request_id: String,
    base_url: Url,
    username: String,
    password: String,
}

fn parse_auth_request(args: Option<&ListValue>) -> Result<AuthRequest, (String, LoadFailure)> {
    let request_id = parse_request_id(args)?;
    let Some(args) = args else {
        unreachable!("parse_request_id rejects missing arguments")
    };
    if args.size() < 4 {
        return Err((
            request_id,
            LoadFailure::new("invalid_request", "Authentication arguments are missing"),
        ));
    }
    for index in 1..4 {
        if args.get_type(index).as_ref() != &sys::cef_value_type_t::VTYPE_STRING {
            return Err((
                request_id,
                LoadFailure::new(
                    "invalid_request",
                    "Authentication arguments must be strings",
                ),
            ));
        }
    }
    let raw_base_url = list_string(args, 1);
    let base_url =
        parse_server_url(&raw_base_url).map_err(|failure| (request_id.clone(), failure))?;
    let username = list_string(args, 2);
    if !valid_text(&username, MAX_USERNAME_LEN) {
        return Err((
            request_id,
            LoadFailure::new("invalid_username", "The username is empty or invalid"),
        ));
    }
    let password = list_string(args, 3);
    if password.len() > MAX_PASSWORD_LEN {
        return Err((
            request_id,
            LoadFailure::new("invalid_password", "The password is too long"),
        ));
    }
    Ok(AuthRequest {
        request_id,
        base_url,
        username,
        password,
    })
}

fn parse_request_id(args: Option<&ListValue>) -> Result<String, (String, LoadFailure)> {
    let Some(args) = args else {
        return Err((
            String::new(),
            LoadFailure::new("invalid_request", "Request arguments are missing"),
        ));
    };
    let request_id =
        if args.size() > 0 && args.get_type(0).as_ref() == &sys::cef_value_type_t::VTYPE_STRING {
            list_string(args, 0)
        } else {
            String::new()
        };
    if !valid_identifier(&request_id, MAX_REQUEST_ID_LEN) {
        return Err((
            request_id,
            LoadFailure::new(
                "invalid_request_id",
                "The request identifier is empty or invalid",
            ),
        ));
    }
    Ok(request_id)
}

fn execute_authentication(
    runtime: &MediaStationRuntime,
    expected_generation: u64,
    request: &AuthRequest,
) -> Result<Value, LoadFailure> {
    let base_authorization = native_authorization_header(None)?;
    let authenticated = runtime
        .api
        .authenticate(
            &request.base_url,
            &request.username,
            &request.password,
            &base_authorization,
        )
        .map_err(|error| authentication_failure(&error))?;
    let access_token = authenticated.access_token_secret().to_string();
    let stored = StoredSession::new(
        authenticated.base_url,
        authenticated.user_id,
        authenticated.user_name,
        access_token,
    )
    .map_err(|error| {
        log_error(&format!(
            "MediaStation authenticated session was rejected: code={}",
            error.code()
        ));
        LoadFailure::new(error.code(), "The authenticated session is invalid")
    })?;
    let authorization = native_authorization_header(Some(stored.access_token_secret()))?;
    let session = stored.to_session(authorization).map_err(|error| {
        log_error(&format!(
            "MediaStation authenticated session could not be configured: code={}",
            api_error_code(&error)
        ));
        LoadFailure::new(
            "authenticated_session_invalid",
            "The authenticated session could not be configured",
        )
    })?;
    runtime.commit_persisted_session(
        expected_generation,
        session,
        stored.user_name.clone(),
        &stored,
    )
}

fn persist_active_session(stored: &StoredSession) -> Result<(), LoadFailure> {
    let previous_server_url = jfn_config::server_url();
    jfn_config::set_server_url(stored.base_url.as_str());
    if !jfn_config::settings_save() {
        jfn_config::set_server_url(&previous_server_url);
        log_error("MediaStation server URL persistence failed");
        return Err(LoadFailure::new(
            "settings_write_failed",
            "The server selection could not be persisted",
        ));
    }
    if let Err(error) = WindowsCredentialStore::active().save(stored) {
        jfn_config::set_server_url(&previous_server_url);
        if !jfn_config::settings_save() {
            log_error("MediaStation server URL rollback failed after credential write failure");
        }
        log_error(&format!(
            "MediaStation credential persistence failed: code={}",
            error.code()
        ));
        return Err(LoadFailure::new(
            error.code(),
            "The MediaStation session could not be persisted securely",
        ));
    }
    Ok(())
}

fn parse_server_url(raw: &str) -> Result<Url, LoadFailure> {
    let value = raw.trim();
    if value.is_empty() || value.len() > MAX_SERVER_URL_LEN || value.chars().any(char::is_control) {
        return Err(LoadFailure::new(
            "invalid_server_url",
            "The server URL is empty or invalid",
        ));
    }
    let url = Url::parse(value).map_err(|_| {
        LoadFailure::new("invalid_server_url", "The server URL could not be parsed")
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(LoadFailure::new(
            "invalid_server_url",
            "The server URL is not an allowed HTTP origin",
        ));
    }
    Ok(url)
}

fn native_authorization_header(token: Option<&str>) -> Result<String, LoadFailure> {
    if token.is_some_and(|token| {
        token.is_empty()
            || token.len() > 2_048
            || !token.is_ascii()
            || token
                .chars()
                .any(|character| character.is_ascii_control() || matches!(character, '"' | '\\'))
    }) {
        return Err(LoadFailure::new(
            "invalid_access_token",
            "The server returned an invalid access token",
        ));
    }
    let mut authorization = format!(
        "MediaBrowser Client=\"MediaStation Windows\", Device=\"Windows\", DeviceId=\"mediastation-windows\", Version=\"{APP_VERSION}\""
    );
    if let Some(token) = token {
        authorization.push_str(", Token=\"");
        authorization.push_str(token);
        authorization.push('"');
    }
    Ok(authorization)
}

fn authentication_failure(error: &ApiError) -> LoadFailure {
    let code = match error {
        ApiError::HttpStatus {
            status_code: 401 | 403,
            ..
        } => "invalid_credentials",
        _ => api_error_code(error),
    };
    log_error(&format!("MediaStation authentication failed: code={code}"));
    LoadFailure::new(code, "MediaStation authentication failed")
}

fn valid_text(value: &str, maximum_len: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_len && !value.chars().any(char::is_control)
}

fn frame_interpolation_plan(
    source: &PlaybackSource,
    mode: InterpolationMode,
    model: InterpolationModel,
) -> Result<Option<InterpolationPlan>, LoadFailure> {
    if mode == InterpolationMode::Off {
        return Ok(None);
    }
    let video = source.video.as_ref().ok_or_else(|| {
        LoadFailure::new(
            "frame_interpolation_video_metadata_missing",
            "Frame interpolation requires a video stream",
        )
    })?;
    let width = video
        .width
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let height = video
        .height
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let request = PlanRequest {
        mode,
        model,
        width,
        height,
        source_fps: video.frame_rate.unwrap_or(0.0),
        display_fps: jfn_playback::ingest_driver::jfn_playback_display_hz(),
        dynamic_range: video.dynamic_range.clone(),
        color_space: video.color_space.clone(),
        color_transfer: video.color_transfer.clone(),
        color_range: video.color_range.clone(),
    };
    prepare_plan(request).map_err(|error| {
        log_error(&format!(
            "RTX frame interpolation plan rejected: code={} detail={}",
            error.code, error.detail
        ));
        LoadFailure::new(
            error.code,
            "Frame interpolation could not be enabled for this video",
        )
    })
}

pub(crate) fn handle_load_message(layer: Option<Arc<Inner>>, args: Option<&ListValue>) -> bool {
    let Some(layer) = layer else {
        log_error("MediaStation load rejected: web layer unavailable");
        return true;
    };
    let request = match parse_request(args) {
        Ok(request) => request,
        Err((request_id, failure)) => {
            dispatch_response(
                &layer,
                &request_id,
                OPERATION_LOAD,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_LOAD,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let (snapshot, guard) = match runtime.begin_request(&request.request_id) {
        Ok(request) => request,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_LOAD,
                false,
                failure.payload(),
            );
            return true;
        }
    };

    let request_id = request.request_id.clone();
    let spawn_failure_request_id = request_id.clone();
    let layer_for_worker = Arc::clone(&layer);
    let worker_runtime = Arc::clone(&runtime);
    let spawn = thread::Builder::new()
        .name("mediastation-load".to_string())
        .spawn(move || {
            let _guard = guard;
            let result = execute_load(&worker_runtime, &snapshot, &request);
            match result {
                Ok(payload) => dispatch_response(
                    &layer_for_worker,
                    &request_id,
                    OPERATION_LOAD,
                    true,
                    payload,
                ),
                Err(failure) => dispatch_response(
                    &layer_for_worker,
                    &request_id,
                    OPERATION_LOAD,
                    false,
                    failure.payload(),
                ),
            }
        });
    if let Err(error) = spawn {
        log_error(&format!(
            "MediaStation load worker could not start: {error}"
        ));
        dispatch_response(
            &layer,
            &spawn_failure_request_id,
            OPERATION_LOAD,
            false,
            LoadFailure::new(
                "request_thread_failed",
                "The native playback request worker could not be started",
            )
            .payload(),
        );
    }
    true
}

pub(crate) fn handle_tracks_message(layer: Option<Arc<Inner>>, args: Option<&ListValue>) -> bool {
    let Some(layer) = layer else {
        log_error("MediaStation runtime tracks rejected: web layer unavailable");
        return true;
    };
    let request = match parse_tracks_request(args) {
        Ok(request) => request,
        Err((request_id, failure)) => {
            dispatch_response(
                &layer,
                &request_id,
                OPERATION_TRACKS,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_TRACKS,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let (snapshot, guard) = match runtime.begin_request(&request.request_id) {
        Ok(request) => request,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_TRACKS,
                false,
                failure.payload(),
            );
            return true;
        }
    };

    let request_id = request.request_id.clone();
    let spawn_failure_request_id = request_id.clone();
    let layer_for_worker = Arc::clone(&layer);
    let worker_runtime = Arc::clone(&runtime);
    let spawn = thread::Builder::new()
        .name("mediastation-runtime-tracks".to_string())
        .spawn(move || {
            let _guard = guard;
            let result = execute_tracks_request(&worker_runtime, &snapshot, &request);
            match result {
                Ok(payload) => dispatch_response(
                    &layer_for_worker,
                    &request_id,
                    OPERATION_TRACKS,
                    true,
                    payload,
                ),
                Err(failure) => dispatch_response(
                    &layer_for_worker,
                    &request_id,
                    OPERATION_TRACKS,
                    false,
                    failure.payload(),
                ),
            }
        });
    if let Err(error) = spawn {
        log_error(&format!(
            "MediaStation runtime tracks worker could not start: {error}"
        ));
        dispatch_response(
            &layer,
            &spawn_failure_request_id,
            OPERATION_TRACKS,
            false,
            LoadFailure::new(
                "request_thread_failed",
                "The native runtime tracks worker could not be started",
            )
            .payload(),
        );
    }
    true
}

struct TracksRequest {
    request_id: String,
    media_id: String,
}

fn parse_tracks_request(args: Option<&ListValue>) -> Result<TracksRequest, (String, LoadFailure)> {
    let Some(args) = args else {
        return Err((
            String::new(),
            LoadFailure::new("invalid_request", "Request arguments are missing"),
        ));
    };
    let request_id =
        if args.size() > 0 && args.get_type(0).as_ref() == &sys::cef_value_type_t::VTYPE_STRING {
            list_string(args, 0)
        } else {
            String::new()
        };
    if !valid_identifier(&request_id, MAX_REQUEST_ID_LEN) {
        return Err((
            request_id,
            LoadFailure::new(
                "invalid_request_id",
                "The request identifier is empty or invalid",
            ),
        ));
    }
    if args.size() < 2 || args.get_type(1).as_ref() != &sys::cef_value_type_t::VTYPE_STRING {
        return Err((
            request_id,
            LoadFailure::new("invalid_media_id", "The media identifier is missing"),
        ));
    }
    let media_id = list_string(args, 1);
    if !valid_identifier(&media_id, MAX_MEDIA_ID_LEN) {
        return Err((
            request_id,
            LoadFailure::new("invalid_media_id", "The media identifier is invalid"),
        ));
    }
    Ok(TracksRequest {
        request_id,
        media_id,
    })
}

fn execute_tracks_request(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    request: &TracksRequest,
) -> Result<Value, LoadFailure> {
    let source = runtime.active_playback_source(snapshot, &request.media_id)?;
    let catalog = current_runtime_track_catalog(&source)?;
    runtime.active_playback_source(snapshot, &request.media_id)?;
    log_debug(&format!(
        "MediaStation runtime tracks: media_id={} audio_count={} subtitle_count={}",
        request.media_id,
        catalog.audio.len(),
        catalog.subtitles.len()
    ));
    Ok(runtime_tracks_payload(&catalog))
}

pub(crate) fn handle_track_selection_message(
    layer: Option<Arc<Inner>>,
    args: Option<&ListValue>,
) -> bool {
    let Some(layer) = layer else {
        log_error("MediaStation track selection rejected: web layer unavailable");
        return true;
    };
    let request = match parse_track_selection_request(args) {
        Ok(request) => request,
        Err((request_id, failure)) => {
            dispatch_response(
                &layer,
                &request_id,
                OPERATION_TRACK_SELECTION,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_TRACK_SELECTION,
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let (snapshot, guard) = match runtime.begin_request(&request.request_id) {
        Ok(request) => request,
        Err(failure) => {
            dispatch_response(
                &layer,
                &request.request_id,
                OPERATION_TRACK_SELECTION,
                false,
                failure.payload(),
            );
            return true;
        }
    };

    let request_id = request.request_id.clone();
    let spawn_failure_request_id = request_id.clone();
    let layer_for_worker = Arc::clone(&layer);
    let worker_runtime = Arc::clone(&runtime);
    let spawn = thread::Builder::new()
        .name("mediastation-track-selection".to_string())
        .spawn(move || {
            let _guard = guard;
            let result = execute_track_selection(&worker_runtime, &snapshot, &request);
            match result {
                Ok(payload) => dispatch_response(
                    &layer_for_worker,
                    &request_id,
                    OPERATION_TRACK_SELECTION,
                    true,
                    payload,
                ),
                Err(failure) => dispatch_response(
                    &layer_for_worker,
                    &request_id,
                    OPERATION_TRACK_SELECTION,
                    false,
                    failure.payload(),
                ),
            }
        });
    if let Err(error) = spawn {
        log_error(&format!(
            "MediaStation track selection worker could not start: {error}"
        ));
        dispatch_response(
            &layer,
            &spawn_failure_request_id,
            OPERATION_TRACK_SELECTION,
            false,
            LoadFailure::new(
                "request_thread_failed",
                "The native track selection worker could not be started",
            )
            .payload(),
        );
    }
    true
}

pub(crate) fn handle_frame_interpolation_message(
    layer: Option<Arc<Inner>>,
    args: Option<&ListValue>,
) -> bool {
    let Some(layer) = layer else {
        log_error("RTX frame interpolation request rejected: web layer unavailable");
        return true;
    };
    let request_id = match parse_request_id(args) {
        Ok(request_id) => request_id,
        Err((request_id, failure)) => {
            dispatch_response(
                &layer,
                &request_id,
                "frame_interpolation_status",
                false,
                failure.payload(),
            );
            return true;
        }
    };
    let Some(args) = args else {
        unreachable!("parse_request_id rejects missing arguments")
    };
    if args.size() < 2 || args.get_type(1).as_ref() != &sys::cef_value_type_t::VTYPE_STRING {
        dispatch_response(
            &layer,
            &request_id,
            "frame_interpolation_status",
            false,
            LoadFailure::new(
                "frame_interpolation_request_invalid",
                "The frame interpolation operation is missing",
            )
            .payload(),
        );
        return true;
    }
    let operation = list_string(args, 1);
    let result = match operation.as_str() {
        "frame_interpolation_status" => Ok(frame_interpolation_status_payload()),
        "frame_interpolation_set_model" => set_frame_interpolation_model(args),
        "frame_interpolation_diagnostics" => {
            let media_id = (args.size() >= 3
                && args.get_type(2).as_ref() == &sys::cef_value_type_t::VTYPE_STRING)
                .then(|| list_string(args, 2));
            frame_interpolation_diagnostics_payload(media_id.as_deref())
        }
        _ => Err(LoadFailure::new(
            "frame_interpolation_operation_invalid",
            "The frame interpolation operation is unsupported",
        )),
    };
    match result {
        Ok(payload) => dispatch_response(&layer, &request_id, &operation, true, payload),
        Err(failure) => {
            log_error(&format!(
                "RTX frame interpolation request failed: operation={operation} code={}",
                failure.code
            ));
            dispatch_response(&layer, &request_id, &operation, false, failure.payload());
        }
    }
    true
}

fn frame_interpolation_status_payload() -> Value {
    let report = jfn_frame_interpolation::capability_report();
    let models = report
        .models
        .iter()
        .map(|model| {
            json!({
                "id": model.id,
                "name": model.name,
                "engineCount": model.engine_count,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "componentStatus": if report.ready { "ready" } else { "unavailable" },
        "gpuName": report.gpu_name,
        "gpuUuid": report.gpu_uuid,
        "driverVersion": report.driver_version,
        "runtimeVersion": report.runtime_version,
        "model": report.model,
        "models": models,
        "selectedModel": jfn_config::frame_interpolation_model(),
        "engineCount": report.engine_count,
        "backend": report.backend,
        "filter": report.filter,
        "failureCode": report.failure.as_ref().map(|failure| failure.code),
        "failureDetail": report.failure.map(|failure| failure.detail),
    })
}

fn set_frame_interpolation_model(args: &ListValue) -> Result<Value, LoadFailure> {
    if args.size() < 3 || args.get_type(2).as_ref() != &sys::cef_value_type_t::VTYPE_STRING {
        return Err(LoadFailure::new(
            "frame_interpolation_model_invalid",
            "The RIFE model selection is missing",
        ));
    }
    let model = parse_interpolation_model_value(&list_string(args, 2))?;
    let previous = jfn_config::frame_interpolation_model();
    jfn_config::set_frame_interpolation_model(model.as_str());
    if !jfn_config::settings_save() {
        jfn_config::set_frame_interpolation_model(&previous);
        return Err(LoadFailure::new(
            "frame_interpolation_setting_save_failed",
            "The RIFE model selection could not be persisted",
        ));
    }
    log_debug(&format!(
        "RIFE frame interpolation model selected: model={} name={}",
        model.as_str(),
        model.display_name()
    ));
    Ok(frame_interpolation_status_payload())
}

fn frame_interpolation_diagnostics_payload(media_id: Option<&str>) -> Result<Value, LoadFailure> {
    if let Some(media_id) = media_id
        && !valid_identifier(media_id, MAX_MEDIA_ID_LEN)
    {
        return Err(LoadFailure::new(
            "invalid_media_id",
            "The media identifier is invalid",
        ));
    }
    let active = RUNTIME
        .get()
        .and_then(|result| result.as_ref().ok())
        .and_then(|runtime| {
            let state = runtime.state.lock();
            let active = state.active_interpolation.as_ref()?;
            if media_id.is_some_and(|requested| requested != active.media_id) {
                return None;
            }
            Some(frame_interpolation_payload(&active.plan))
        });
    let container_fps = mpv_property_double(c"container-fps");
    let estimated_vf_fps = mpv_property_double(c"estimated-vf-fps");
    let display_fps = mpv_property_double(c"display-fps");
    let frame_drop_count = mpv_property_int(c"frame-drop-count");
    let decoder_frame_drop_count = mpv_property_int(c"decoder-frame-drop-count");
    let mistimed_frame_count = mpv_property_int(c"mistimed-frame-count");
    let vo_delayed_frame_count = mpv_property_int(c"vo-delayed-frame-count");
    let hwdec_current = mpv_property_string(c"hwdec-current");
    log_debug(&format!(
        "RTX frame interpolation diagnostics: active={} container_fps={:?} estimated_vf_fps={:?} display_fps={:?} frame_drop_count={:?} decoder_frame_drop_count={:?} mistimed_frame_count={:?} vo_delayed_frame_count={:?} hwdec_current={:?}",
        active.is_some(),
        container_fps,
        estimated_vf_fps,
        display_fps,
        frame_drop_count,
        decoder_frame_drop_count,
        mistimed_frame_count,
        vo_delayed_frame_count,
        hwdec_current,
    ));
    Ok(json!({
        "status": frame_interpolation_status_payload(),
        "active": active,
        "playback": {
            "containerFps": container_fps,
            "estimatedVfFps": estimated_vf_fps,
            "displayFps": display_fps,
            "frameDropCount": frame_drop_count,
            "decoderFrameDropCount": decoder_frame_drop_count,
            "mistimedFrameCount": mistimed_frame_count,
            "voDelayedFrameCount": vo_delayed_frame_count,
            "hwdecCurrent": hwdec_current,
        },
    }))
}

fn mpv_property_double(name: &CStr) -> Option<f64> {
    let mut value = 0.0;
    (unsafe { jfn_mpv_get_property_double(name.as_ptr(), &mut value) } >= 0).then_some(value)
}

fn mpv_property_int(name: &CStr) -> Option<i64> {
    let mut value = 0;
    (unsafe { jfn_mpv_get_property_int(name.as_ptr(), &mut value) } >= 0).then_some(value)
}

fn mpv_property_string(name: &CStr) -> Option<String> {
    let pointer = unsafe { jfn_mpv_get_property_string(name.as_ptr()) };
    if pointer.is_null() {
        return None;
    }
    let value = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    unsafe { jfn_mpv_free_string(pointer) };
    Some(value)
}

enum RequestedTrackKind {
    Audio,
    Subtitle,
}

struct TrackSelectionRequest {
    request_id: String,
    media_id: String,
    kind: RequestedTrackKind,
    track_key: Option<String>,
}

fn parse_track_selection_request(
    args: Option<&ListValue>,
) -> Result<TrackSelectionRequest, (String, LoadFailure)> {
    let Some(args) = args else {
        return Err((
            String::new(),
            LoadFailure::new("invalid_request", "Request arguments are missing"),
        ));
    };
    let request_id =
        if args.size() > 0 && args.get_type(0).as_ref() == &sys::cef_value_type_t::VTYPE_STRING {
            list_string(args, 0)
        } else {
            String::new()
        };
    if !valid_identifier(&request_id, MAX_REQUEST_ID_LEN) {
        return Err((
            request_id,
            LoadFailure::new(
                "invalid_request_id",
                "The request identifier is empty or invalid",
            ),
        ));
    }
    if args.size() < 4
        || (1..=3)
            .any(|index| args.get_type(index).as_ref() != &sys::cef_value_type_t::VTYPE_STRING)
    {
        return Err((
            request_id,
            LoadFailure::new("invalid_track_selection", "Track selection is incomplete"),
        ));
    }
    let media_id = list_string(args, 1);
    if !valid_identifier(&media_id, MAX_MEDIA_ID_LEN) {
        return Err((
            request_id,
            LoadFailure::new("invalid_media_id", "The media identifier is invalid"),
        ));
    }
    let kind = match list_string(args, 2).as_str() {
        "audio" => RequestedTrackKind::Audio,
        "subtitle" => RequestedTrackKind::Subtitle,
        _ => {
            return Err((
                request_id,
                LoadFailure::new("invalid_track_kind", "The track type is not supported"),
            ));
        }
    };
    let raw_key = list_string(args, 3);
    let track_key = if raw_key.is_empty() {
        None
    } else if valid_identifier(&raw_key, MAX_TRACK_KEY_LEN) {
        Some(raw_key)
    } else {
        return Err((
            request_id,
            LoadFailure::new("invalid_track_key", "The track identifier is invalid"),
        ));
    };
    if matches!(kind, RequestedTrackKind::Audio) && track_key.is_none() {
        return Err((
            request_id,
            LoadFailure::new("invalid_track_key", "An audio track is required"),
        ));
    }
    Ok(TrackSelectionRequest {
        request_id,
        media_id,
        kind,
        track_key,
    })
}

fn execute_track_selection(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    request: &TrackSelectionRequest,
) -> Result<Value, LoadFailure> {
    let source = runtime.active_playback_source(snapshot, &request.media_id)?;
    let catalog = current_runtime_track_catalog(&source)?;
    let previous_preference = runtime_preference_update(&catalog);
    let (update, selection) = match request.kind {
        RequestedTrackKind::Audio => {
            let key = request.track_key.as_deref().ok_or_else(|| {
                LoadFailure::new("invalid_track_key", "An audio track is required")
            })?;
            let track = catalog
                .audio
                .iter()
                .find(|track| track.key == key)
                .ok_or_else(|| {
                    LoadFailure::new(
                        "audio_track_unavailable",
                        "The selected audio track is no longer available",
                    )
                })?;
            let mpv_track = track.mpv_id.ok_or_else(|| {
                LoadFailure::new(
                    "audio_track_unavailable",
                    "The selected audio track has no native player identifier",
                )
            })?;
            (
                PlaybackTrackPreferenceUpdate {
                    audio_track_key: Some(key.to_string()),
                    ..PlaybackTrackPreferenceUpdate::default()
                },
                TrackSelection::Audio {
                    mpv_track,
                    key: key.to_string(),
                },
            )
        }
        RequestedTrackKind::Subtitle => {
            let Some(key) = request.track_key.as_deref() else {
                let update = PlaybackTrackPreferenceUpdate {
                    subtitle_enabled: Some(false),
                    ..PlaybackTrackPreferenceUpdate::default()
                };
                runtime.ensure_generation(snapshot.generation)?;
                runtime
                    .api
                    .update_playback_preference(&snapshot.session, &request.media_id, &update)
                    .map_err(|error| api_failure("preference_update_failed", &error))?;
                runtime.ensure_generation(snapshot.generation)?;
                let result = runtime.apply_track_selection(
                    snapshot,
                    &request.media_id,
                    TrackSelection::SubtitleOff,
                );
                if result.is_err() {
                    rollback_track_preference(
                        runtime,
                        snapshot,
                        &request.media_id,
                        &previous_preference,
                    );
                }
                return result;
            };
            let source_track = source.subtitles.iter().find(|track| track.key == key);
            let update = PlaybackTrackPreferenceUpdate {
                subtitle_enabled: Some(true),
                subtitle_track_key: Some(key.to_string()),
                ..PlaybackTrackPreferenceUpdate::default()
            };
            let selection = if let Some(track) = source_track.filter(|track| track.is_external) {
                TrackSelection::SubtitleExternal {
                    subtitle: prepare_external_subtitle_track(runtime, snapshot, track)?,
                    key: key.to_string(),
                }
            } else {
                let track = catalog
                    .subtitles
                    .iter()
                    .find(|candidate| !candidate.external && candidate.key == key)
                    .ok_or_else(|| {
                        LoadFailure::new(
                            "subtitle_track_unavailable",
                            "The selected subtitle track is no longer available",
                        )
                    })?;
                let mpv_track = track.mpv_id.ok_or_else(|| {
                    LoadFailure::new(
                        "subtitle_track_unavailable",
                        "The selected subtitle track has no native player identifier",
                    )
                })?;
                TrackSelection::SubtitleEmbedded {
                    mpv_track,
                    key: key.to_string(),
                }
            };
            (update, selection)
        }
    };

    runtime.ensure_generation(snapshot.generation)?;
    runtime
        .api
        .update_playback_preference(&snapshot.session, &request.media_id, &update)
        .map_err(|error| api_failure("preference_update_failed", &error))?;
    runtime.ensure_generation(snapshot.generation)?;
    // Record that the user explicitly changed the track preference so the
    // first-frame reconcile worker will not re-apply or correct it.
    runtime.bump_preference_revision(snapshot);
    match runtime.apply_track_selection(snapshot, &request.media_id, selection) {
        Ok(payload) => Ok(payload),
        Err(failure) => {
            rollback_track_preference(runtime, snapshot, &request.media_id, &previous_preference);
            Err(failure)
        }
    }
}

fn runtime_preference_update(catalog: &RuntimeTrackCatalog) -> PlaybackTrackPreferenceUpdate {
    let audio_track_key = catalog
        .audio
        .iter()
        .find(|track| track.selected)
        .map(|track| track.key.clone());
    let subtitle_track_key = catalog
        .subtitles
        .iter()
        .find(|track| track.selected)
        .map(|track| track.key.clone());
    PlaybackTrackPreferenceUpdate {
        subtitle_enabled: Some(subtitle_track_key.is_some()),
        subtitle_track_key,
        audio_track_key,
    }
}

fn rollback_track_preference(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    media_id: &str,
    previous: &PlaybackTrackPreferenceUpdate,
) {
    if runtime.ensure_generation(snapshot.generation).is_err() {
        return;
    }
    if let Err(error) =
        runtime
            .api
            .update_playback_preference(&snapshot.session, media_id, previous)
    {
        log_error(&format!(
            "MediaStation track preference rollback failed: code={}",
            api_error_code(&error)
        ));
    }
}

fn reconcile_runtime_track_preference(
    runtime: &MediaStationRuntime,
    reconcile: RuntimeTrackReconcile,
) {
    if let Err(failure) = reconcile_runtime_track_preference_inner(runtime, &reconcile) {
        log_error(&format!(
            "MediaStation runtime track reconciliation failed: media_id={} code={}",
            reconcile.source.media_id, failure.code
        ));
    }
}

fn reconcile_runtime_track_preference_inner(
    runtime: &MediaStationRuntime,
    reconcile: &RuntimeTrackReconcile,
) -> Result<(), LoadFailure> {
    runtime.active_playback_source(&reconcile.snapshot, &reconcile.source.media_id)?;
    // If the user saved a track preference after playback loaded, the
    // reconcile worker must not re-apply the load-time preference or write a
    // correction over the user's explicit choice.
    if runtime.active_preference_revision(&reconcile.snapshot)? != reconcile.preference_revision {
        return Ok(());
    }
    let catalog = current_runtime_track_catalog(&reconcile.source)?;
    let baseline = runtime_preference_update(&catalog);
    let mut correction = PlaybackTrackPreferenceUpdate::default();

    if let Some(key) = reconcile.preference.audio_track_key.as_deref() {
        if let Some(track) = catalog.audio.iter().find(|track| track.key == key) {
            if !track.selected {
                let mpv_track = track.mpv_id.ok_or_else(|| {
                    LoadFailure::new(
                        "audio_track_unavailable",
                        "The preferred audio track has no native player identifier",
                    )
                })?;
                if runtime
                    .apply_track_selection(
                        &reconcile.snapshot,
                        &reconcile.source.media_id,
                        TrackSelection::Audio {
                            mpv_track,
                            key: key.to_string(),
                        },
                    )
                    .is_err()
                {
                    correction.audio_track_key = baseline
                        .audio_track_key
                        .clone()
                        .or_else(|| catalog.audio.first().map(|track| track.key.clone()));
                    log_error(&format!(
                        "MediaStation preferred audio track fallback: media_id={} key={}",
                        reconcile.source.media_id, key
                    ));
                }
            }
        } else if reconcile.preference.configured {
            correction.audio_track_key = baseline
                .audio_track_key
                .clone()
                .or_else(|| catalog.audio.first().map(|track| track.key.clone()));
            log_error(&format!(
                "MediaStation saved audio track unavailable: media_id={} key={}",
                reconcile.source.media_id, key
            ));
        }
    }

    if reconcile.preference.subtitle_enabled {
        let requested_key = reconcile.preference.subtitle_track_key.as_deref();
        if let Some(key) = requested_key {
            let already_selected = catalog
                .subtitles
                .iter()
                .any(|track| track.key == key && track.selected);
            if !already_selected {
                match runtime_subtitle_selection(
                    runtime,
                    &reconcile.snapshot,
                    &reconcile.source,
                    &catalog,
                    key,
                ) {
                    Ok(selection) => {
                        if runtime
                            .apply_track_selection(
                                &reconcile.snapshot,
                                &reconcile.source.media_id,
                                selection,
                            )
                            .is_err()
                        {
                            correction_from_subtitle_baseline(&mut correction, &baseline);
                            log_error(&format!(
                                "MediaStation preferred subtitle fallback: media_id={} key={}",
                                reconcile.source.media_id, key
                            ));
                        }
                    }
                    Err(_) => {
                        // The saved subtitle key no longer matches the live
                        // track catalog (stream indexes can shift between
                        // loads). Keep whatever mpv has selected — the load
                        // plan already fell back to the container default —
                        // and correct the preference to the actual state.
                        correction_from_subtitle_baseline(&mut correction, &baseline);
                        log_error(&format!(
                            "MediaStation saved subtitle unavailable: media_id={} key={}",
                            reconcile.source.media_id, key
                        ));
                    }
                }
            }
        } else if reconcile.preference.configured {
            correction.subtitle_enabled = Some(false);
        }
    } else if baseline.subtitle_enabled == Some(true) {
        runtime.apply_track_selection(
            &reconcile.snapshot,
            &reconcile.source.media_id,
            TrackSelection::SubtitleOff,
        )?;
    }

    if preference_update_has_values(&correction) {
        runtime.ensure_generation(reconcile.snapshot.generation)?;
        runtime
            .api
            .update_playback_preference(
                &reconcile.snapshot.session,
                &reconcile.source.media_id,
                &correction,
            )
            .map_err(|error| api_failure("preference_correction_failed", &error))?;
        log_debug(&format!(
            "MediaStation runtime track preference corrected: media_id={}",
            reconcile.source.media_id
        ));
    }

    Ok(())
}

fn runtime_subtitle_selection(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    source: &PlaybackSource,
    catalog: &RuntimeTrackCatalog,
    key: &str,
) -> Result<TrackSelection, LoadFailure> {
    if let Some(track) = source
        .subtitles
        .iter()
        .find(|track| track.is_external && track.key == key)
    {
        return Ok(TrackSelection::SubtitleExternal {
            subtitle: prepare_external_subtitle_track(runtime, snapshot, track)?,
            key: key.to_string(),
        });
    }
    let track = catalog
        .subtitles
        .iter()
        .find(|track| !track.external && track.key == key)
        .ok_or_else(|| {
            LoadFailure::new(
                "subtitle_track_unavailable",
                "The preferred subtitle track is no longer available",
            )
        })?;
    let mpv_track = track.mpv_id.ok_or_else(|| {
        LoadFailure::new(
            "subtitle_track_unavailable",
            "The preferred subtitle track has no native player identifier",
        )
    })?;
    Ok(TrackSelection::SubtitleEmbedded {
        mpv_track,
        key: key.to_string(),
    })
}

fn correction_from_subtitle_baseline(
    correction: &mut PlaybackTrackPreferenceUpdate,
    baseline: &PlaybackTrackPreferenceUpdate,
) {
    correction.subtitle_enabled = baseline.subtitle_enabled;
    correction.subtitle_track_key = baseline.subtitle_track_key.clone();
}

fn preference_update_has_values(update: &PlaybackTrackPreferenceUpdate) -> bool {
    update.subtitle_enabled.is_some()
        || update.subtitle_track_key.is_some()
        || update.audio_track_key.is_some()
}

struct LoadRequest {
    request_id: String,
    media_id: String,
    start_ms: u64,
    interpolation_mode: InterpolationMode,
    interpolation_model: InterpolationModel,
}

fn parse_request(args: Option<&ListValue>) -> Result<LoadRequest, (String, LoadFailure)> {
    let Some(args) = args else {
        return Err((
            String::new(),
            LoadFailure::new("invalid_request", "Request arguments are missing"),
        ));
    };
    let request_id =
        if args.size() > 0 && args.get_type(0).as_ref() == &sys::cef_value_type_t::VTYPE_STRING {
            list_string(args, 0)
        } else {
            String::new()
        };
    if !valid_identifier(&request_id, MAX_REQUEST_ID_LEN) {
        return Err((
            request_id,
            LoadFailure::new(
                "invalid_request_id",
                "The request identifier is empty or invalid",
            ),
        ));
    }
    if args.size() < 3 || args.get_type(1).as_ref() != &sys::cef_value_type_t::VTYPE_STRING {
        return Err((
            request_id,
            LoadFailure::new("invalid_media_id", "The media identifier is missing"),
        ));
    }
    let media_id = list_string(args, 1);
    if !valid_identifier(&media_id, MAX_MEDIA_ID_LEN) {
        return Err((
            request_id,
            LoadFailure::new(
                "invalid_media_id",
                "The media identifier is empty or invalid",
            ),
        ));
    }
    let start_ms = parse_start_ms(args).map_err(|failure| (request_id.clone(), failure))?;
    let interpolation_mode =
        parse_load_interpolation_mode(args).map_err(|failure| (request_id.clone(), failure))?;
    let interpolation_model =
        parse_load_interpolation_model(args).map_err(|failure| (request_id.clone(), failure))?;
    Ok(LoadRequest {
        request_id,
        media_id,
        start_ms,
        interpolation_mode,
        interpolation_model,
    })
}

fn parse_start_ms(args: &ListValue) -> Result<u64, LoadFailure> {
    let value_type = args.get_type(2);
    let value = if value_type.as_ref() == &sys::cef_value_type_t::VTYPE_INT {
        f64::from(args.int(2))
    } else if value_type.as_ref() == &sys::cef_value_type_t::VTYPE_DOUBLE {
        args.double(2)
    } else {
        return Err(LoadFailure::new(
            "invalid_start_position",
            "The start position must be a non-negative number",
        ));
    };
    let maximum = (i64::MAX / 1000) as f64;
    if !value.is_finite() || value < 0.0 || value > maximum {
        return Err(LoadFailure::new(
            "invalid_start_position",
            "The start position is outside the supported range",
        ));
    }
    Ok(value.round() as u64)
}

fn parse_load_interpolation_mode(args: &ListValue) -> Result<InterpolationMode, LoadFailure> {
    if args.size() < 4 || args.get_type(3).as_ref() != &sys::cef_value_type_t::VTYPE_STRING {
        return Err(LoadFailure::new(
            "frame_interpolation_mode_invalid",
            "The playback frame interpolation mode is missing",
        ));
    }
    parse_load_interpolation_mode_value(&list_string(args, 3))
}

fn parse_load_interpolation_mode_value(value: &str) -> Result<InterpolationMode, LoadFailure> {
    match value {
        "off" => Ok(InterpolationMode::Off),
        "2x" => Ok(InterpolationMode::X2),
        _ => Err(LoadFailure::new(
            "frame_interpolation_mode_invalid",
            "The playback frame interpolation mode is invalid",
        )),
    }
}

fn parse_load_interpolation_model(args: &ListValue) -> Result<InterpolationModel, LoadFailure> {
    if args.size() < 5 || args.get_type(4).as_ref() != &sys::cef_value_type_t::VTYPE_STRING {
        return Err(LoadFailure::new(
            "frame_interpolation_model_invalid",
            "The RIFE model selection is missing",
        ));
    }
    parse_interpolation_model_value(&list_string(args, 4))
}

fn parse_interpolation_model_value(value: &str) -> Result<InterpolationModel, LoadFailure> {
    InterpolationModel::parse(value).ok_or_else(|| {
        LoadFailure::new(
            "frame_interpolation_model_invalid",
            "The RIFE model selection is invalid",
        )
    })
}

fn valid_identifier(value: &str, maximum_len: usize) -> bool {
    !value.trim().is_empty() && value.len() <= maximum_len && !value.chars().any(char::is_control)
}

fn execute_load(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    request: &LoadRequest,
) -> Result<Value, LoadFailure> {
    runtime.ensure_generation(snapshot.generation)?;
    let source = runtime
        .api
        .load_playback_source(&snapshot.session, &request.media_id)
        .map_err(|error| api_failure("playback_info_failed", &error))?;
    let audio_summary = source
        .audio_tracks
        .iter()
        .map(|track| {
            format!(
                "{}:{}:{}ch",
                track.key,
                track.codec.as_deref().unwrap_or("unknown"),
                track
                    .channel_count
                    .map_or_else(|| "?".to_string(), |channels| channels.to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    log_debug(&format!(
        "MediaStation PlaybackInfo tracks: media_id={} audio_count={} audio=[{}] subtitle_count={}",
        request.media_id,
        source.audio_tracks.len(),
        audio_summary,
        source.subtitles.len()
    ));
    let interpolation = frame_interpolation_plan(
        &source,
        request.interpolation_mode,
        request.interpolation_model,
    )?;
    log_debug(&format!(
        "MediaStation playback interpolation request: media_id={} mode={} model={}",
        request.media_id,
        request.interpolation_mode.as_str(),
        request.interpolation_model.as_str()
    ));
    if let Some(plan) = &interpolation {
        let display_fps = jfn_playback::ingest_driver::jfn_playback_display_hz();
        log_debug(&format!(
            "RIFE frame interpolation planned: media_id={} mode={} source_fps={}/{} target_fps={} display_fps={} backend={} runtime={} model={} engine_key={} scale={} precision={} hwdec={} filter={}",
            request.media_id,
            plan.mode.as_str(),
            plan.source_fps_num,
            plan.source_fps_den,
            plan.target_fps,
            display_fps,
            plan.backend,
            plan.runtime_version,
            plan.model,
            plan.engine_key,
            plan.scale,
            plan.precision,
            plan.hwdec,
            plan.video_filter,
        ));
        let cadence_multiple = (display_fps / plan.target_fps).round().max(1.0);
        let cadence_target = plan.target_fps * cadence_multiple;
        let cadence_error = (display_fps - cadence_target).abs();
        if cadence_error > 0.5 {
            log_warn(&format!(
                "RIFE display cadence mismatch: display_fps={display_fps:.3} target_fps={:.3} nearest_multiple={cadence_multiple:.0} expected_display_fps={cadence_target:.3} error_hz={cadence_error:.3}; fixed-refresh presentation can judder when VRR is inactive",
                plan.target_fps,
            ));
        }
    }

    runtime.ensure_generation(snapshot.generation)?;
    let preference = runtime
        .api
        .load_playback_preference(&snapshot.session, &request.media_id)
        .map_err(|error| api_failure("preference_load_failed", &error))?;
    let plan = build_playback_track_plan(&source, &preference);

    // PlaybackInfo can omit embedded streams. Correcting an unknown key here
    // would overwrite a valid runtime-track preference before mpv has parsed
    // the container, so defer validation until the first frame.
    let correction_status = if plan.preference_correction.is_some() {
        "deferred_runtime_tracks"
    } else {
        "not_needed"
    };
    let correction_error_code: Option<&'static str> = None;

    runtime.ensure_generation(snapshot.generation)?;
    let resolve_input = snapshot
        .session
        .playback_resolve_input(
            &request.media_id,
            source.url.clone(),
            runtime.user_agent.clone(),
        )
        .map_err(|error| playback_session_failure("resolve_input_failed", &error))?;
    let playback = runtime
        .resolver
        .resolve(&resolve_input)
        .map_err(|error| playback_session_failure("playback_resolve_failed", &error))?;

    runtime.ensure_generation(snapshot.generation)?;
    let prepared_subtitle = prepare_external_subtitle(runtime, snapshot, &source, &plan)?;
    let subtitle_delivery = if prepared_subtitle.is_some() {
        "isolated_local"
    } else if plan.subtitle_track != 0 {
        "embedded"
    } else {
        "disabled"
    };
    let subtitle_bytes = prepared_subtitle
        .as_ref()
        .map(|subtitle| subtitle.byte_count);
    let subtitle_redirect_count = prepared_subtitle
        .as_ref()
        .map(|subtitle| subtitle.redirect_count);
    let subtitle_target_host = prepared_subtitle
        .as_ref()
        .map(|subtitle| subtitle.target_host.clone());
    let header_fields = playback
        .request_headers
        .to_mpv_http_header_fields()
        .map_err(header_encoding_failure)?;
    // Serve the resolved CDN URL through the ureq-backed mediastation://
    // protocol instead of handing ffmpeg the direct CDN URL. The 115 cloud
    // CDN 403s ffmpeg's TLS fingerprint, so mpv reads the stream from ureq.
    let media_user_agent = playback
        .request_headers
        .get("user-agent")
        .unwrap_or("MediaStationGoWindows/0.1.0-dev");
    let playback_url = CString::new(jfn_mpv::stream_cb::build_uri(
        playback.resolved_url.as_str(),
        media_user_agent,
        playback.content_length,
    ))
    .map_err(|_| {
        LoadFailure::new(
            "invalid_playback_url",
            "The source playback URL contains an invalid byte",
        )
    })?;
    let header_fields = CString::new(header_fields).map_err(|_| {
        LoadFailure::new(
            "invalid_playback_headers",
            "The playback headers contain an invalid byte",
        )
    })?;
    let video_filter = interpolation
        .as_ref()
        .map(|plan| CString::new(plan.video_filter.as_str()))
        .transpose()
        .map_err(|_| {
            LoadFailure::new(
                "frame_interpolation_filter_invalid",
                "The generated frame interpolation filter contains an invalid byte",
            )
        })?;
    let interpolation_hwdec = interpolation
        .as_ref()
        .map(|plan| CString::new(plan.hwdec))
        .transpose()
        .map_err(|_| {
            LoadFailure::new(
                "frame_interpolation_hwdec_invalid",
                "The generated frame interpolation decoder mode is invalid",
            )
        })?;
    let options = JfnMpvLoadOptions {
        start_secs: request.start_ms as f64 / 1000.0,
        video_track: plan.video_track,
        audio_track: plan.audio_track,
        sub_track: plan.subtitle_track,
        external_audio_url: c"".as_ptr(),
        external_sub_url: prepared_subtitle
            .as_ref()
            .map_or(c"".as_ptr(), |subtitle| subtitle.path.as_ptr()),
        http_header_fields: header_fields.as_ptr(),
        video_filter: video_filter
            .as_ref()
            .map_or(c"".as_ptr(), |value| value.as_ptr()),
        hwdec: interpolation_hwdec
            .as_ref()
            .map_or(c"".as_ptr(), |value| value.as_ptr()),
        is_infinite_stream: false,
    };
    runtime.load_if_current(NativeLoadRequest {
        snapshot,
        source: &source,
        preference: &preference,
        media_id: &request.media_id,
        start_ms: request.start_ms,
        url: &playback_url,
        options: &options,
        subtitle: prepared_subtitle,
        interpolation: interpolation.clone(),
    })?;

    Ok(json!({
        "state": "queued",
        "deliveryMode": match playback.metrics.delivery_mode {
            DeliveryMode::Server => "server",
            DeliveryMode::DirectCdn => "direct_cdn",
        },
        "redirectCount": playback.metrics.redirect_count,
        "resolveMs": playback.metrics.resolve_ms,
        "probeStatus": playback.metrics.status_code,
        "targetHost": playback.metrics.target_host,
        "acceptsRanges": playback.metrics.accepts_ranges,
        "contentLength": playback.content_length,
        "serverCredentialQueryRemoved": source.server_credential_query_removed,
        "expirySource": match playback.metrics.expiry_source {
            SessionExpirySource::QueryTimestamp => "query_timestamp",
            SessionExpirySource::DefaultTtl => "default_ttl",
        },
        "reused": playback.metrics.reused,
        "audioTrackKey": plan.audio_track_key,
        "subtitleTrackKey": plan.subtitle_track_key,
        "subtitleEnabled": plan.subtitle_track_key.is_some(),
        "audioTracks": audio_tracks_payload(&source),
        "subtitleTracks": subtitle_tracks_payload(&source),
        "sourceVideo": source_video_payload(&source),
        "container": source.container,
        "bitrate": source.bitrate,
        "subtitleDelivery": subtitle_delivery,
        "subtitleBytes": subtitle_bytes,
        "subtitleRedirectCount": subtitle_redirect_count,
        "subtitleTargetHost": subtitle_target_host,
        "reportingAvailable": runtime.reporter.is_available(),
        "preferenceCorrection": correction_status,
        "preferenceCorrectionErrorCode": correction_error_code,
        "frameInterpolation": interpolation.as_ref().map(frame_interpolation_payload),
    }))
}

fn frame_interpolation_payload(plan: &InterpolationPlan) -> Value {
    json!({
        "enabled": true,
        "mode": plan.mode.as_str(),
        "sourceFps": plan.source_fps_num as f64 / plan.source_fps_den as f64,
        "targetFps": plan.target_fps,
        "hwdec": plan.hwdec,
        "backend": plan.backend,
        "runtimeVersion": plan.runtime_version,
        "modelId": plan.model_id,
        "model": plan.model,
        "modelSha256": plan.model_sha256,
        "engineKey": plan.engine_key,
        "enginePath": plan.engine_path,
        "scale": plan.scale,
        "precision": plan.precision,
        "videoFilter": plan.video_filter,
    })
}

fn audio_tracks_payload(source: &PlaybackSource) -> Value {
    Value::Array(
        source
            .audio_tracks
            .iter()
            .map(|track| {
                json!({
                    "key": track.key,
                    "label": track.label,
                    "language": track.language,
                    "codec": track.codec,
                    "channels": track.channel_count,
                })
            })
            .collect(),
    )
}

fn subtitle_tracks_payload(source: &PlaybackSource) -> Value {
    Value::Array(
        source
            .subtitles
            .iter()
            .map(|track| {
                json!({
                    "key": track.key,
                    "label": track.label,
                    "language": track.language,
                    "codec": track.codec,
                    "external": track.is_external,
                    "default": track.is_default,
                    "forced": track.is_forced,
                })
            })
            .collect(),
    )
}

fn current_runtime_track_catalog(
    source: &PlaybackSource,
) -> Result<RuntimeTrackCatalog, LoadFailure> {
    let node = jfn_mpv_get_property_node(c"track-list").map_err(|error| {
        log_error(&format!(
            "MediaStation runtime track-list read failed: code={}",
            error.code
        ));
        LoadFailure::new(
            "runtime_tracks_unavailable",
            "The player runtime track list is unavailable",
        )
    })?;
    runtime_track_catalog(source, &node)
}

fn runtime_track_catalog(
    source: &PlaybackSource,
    node: &jfn_mpv::Node,
) -> Result<RuntimeTrackCatalog, LoadFailure> {
    let tracks = node.as_array().ok_or_else(|| {
        LoadFailure::new(
            "runtime_tracks_invalid",
            "The player returned an invalid runtime track list",
        )
    })?;
    let parsed = tracks
        .iter()
        .filter_map(parse_mpv_runtime_track)
        .collect::<Vec<_>>();
    let mut catalog = RuntimeTrackCatalog::default();
    let mut audio_ordinal = 0usize;
    let mut embedded_subtitle_ordinal = 0usize;
    let mut external_subtitle_ordinal = 0usize;
    let mut matched_subtitle_keys = HashSet::new();

    for track in parsed {
        match track.kind {
            RuntimeTrackKind::Audio => {
                let server = source.audio_tracks.get(audio_ordinal);
                let key = server.map_or_else(
                    || runtime_fallback_track_key(&track, audio_ordinal),
                    |server| server.key.clone(),
                );
                catalog.audio.push(RuntimeTrack {
                    kind: RuntimeTrackKind::Audio,
                    key,
                    mpv_id: Some(track.id),
                    codec: track
                        .codec
                        .or_else(|| server.and_then(|value| value.codec.clone())),
                    language: track
                        .language
                        .or_else(|| server.and_then(|value| value.language.clone())),
                    label: track
                        .title
                        .or_else(|| server.and_then(|value| value.label.clone())),
                    channel_count: track
                        .channel_count
                        .map(u64::from)
                        .or_else(|| server.and_then(|value| value.channel_count)),
                    selected: track.selected,
                    external: track.external,
                    is_default: track.is_default,
                    is_forced: false,
                });
                audio_ordinal += 1;
            }
            RuntimeTrackKind::Subtitle => {
                let ordinal = if track.external {
                    let ordinal = external_subtitle_ordinal;
                    external_subtitle_ordinal += 1;
                    ordinal
                } else {
                    let ordinal = embedded_subtitle_ordinal;
                    embedded_subtitle_ordinal += 1;
                    ordinal
                };
                let server = source
                    .subtitles
                    .iter()
                    .filter(|candidate| candidate.is_external == track.external)
                    .nth(ordinal);
                let key = server.map_or_else(
                    || runtime_fallback_track_key(&track, ordinal),
                    |server| server.key.clone(),
                );
                if server.is_some() {
                    matched_subtitle_keys.insert(key.clone());
                }
                catalog.subtitles.push(RuntimeTrack {
                    kind: RuntimeTrackKind::Subtitle,
                    key,
                    mpv_id: Some(track.id),
                    codec: track
                        .codec
                        .or_else(|| server.and_then(|value| value.codec.clone())),
                    language: track
                        .language
                        .or_else(|| server.and_then(|value| value.language.clone())),
                    label: track
                        .title
                        .or_else(|| server.and_then(|value| value.label.clone())),
                    channel_count: None,
                    selected: track.selected,
                    external: track.external,
                    is_default: track.is_default || server.is_some_and(|value| value.is_default),
                    is_forced: track.is_forced || server.is_some_and(|value| value.is_forced),
                });
            }
        }
    }

    // Disabled external subtitles are not present in mpv until selected, but
    // remain valid server-backed choices that can be downloaded on demand.
    for track in source.subtitles.iter().filter(|track| track.is_external) {
        if matched_subtitle_keys.contains(&track.key) {
            continue;
        }
        catalog.subtitles.push(RuntimeTrack {
            kind: RuntimeTrackKind::Subtitle,
            key: track.key.clone(),
            mpv_id: None,
            codec: track.codec.clone(),
            language: track.language.clone(),
            label: track.label.clone(),
            channel_count: None,
            selected: false,
            external: true,
            is_default: track.is_default,
            is_forced: track.is_forced,
        });
    }

    Ok(catalog)
}

fn parse_mpv_runtime_track(node: &jfn_mpv::Node) -> Option<MpvRuntimeTrack> {
    let kind = match node.get("type")?.as_str()? {
        "audio" => RuntimeTrackKind::Audio,
        "sub" => RuntimeTrackKind::Subtitle,
        _ => return None,
    };
    let id = node.get("id")?.as_int()?;
    if id <= 0 {
        return None;
    }
    Some(MpvRuntimeTrack {
        kind,
        id,
        source_id: node.get("src-id").and_then(jfn_mpv::Node::as_int),
        ff_index: node.get("ff-index").and_then(jfn_mpv::Node::as_int),
        codec: runtime_node_string(node, "codec"),
        language: runtime_node_string(node, "lang"),
        title: runtime_node_string(node, "title"),
        channel_count: runtime_node_u32(node, "demux-channel-count"),
        sample_rate: runtime_node_u32(node, "demux-samplerate"),
        selected: runtime_node_flag(node, "selected"),
        external: runtime_node_flag(node, "external"),
        is_default: runtime_node_flag(node, "default"),
        is_forced: runtime_node_flag(node, "forced"),
    })
}

fn runtime_node_string(node: &jfn_mpv::Node, key: &str) -> Option<String> {
    node.get(key)
        .and_then(jfn_mpv::Node::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn runtime_node_u32(node: &jfn_mpv::Node, key: &str) -> Option<u32> {
    node.get(key)
        .and_then(jfn_mpv::Node::as_int)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn runtime_node_flag(node: &jfn_mpv::Node, key: &str) -> bool {
    node.get(key)
        .and_then(jfn_mpv::Node::as_flag)
        .unwrap_or(false)
}

fn runtime_fallback_track_key(track: &MpvRuntimeTrack, ordinal: usize) -> String {
    let kind = match track.kind {
        RuntimeTrackKind::Audio => "audio",
        RuntimeTrackKind::Subtitle => "subtitle",
    };
    let identity = [
        kind.to_string(),
        track
            .source_id
            .map_or_else(String::new, |value| value.to_string()),
        track
            .ff_index
            .map_or_else(String::new, |value| value.to_string()),
        track.codec.clone().unwrap_or_default(),
        track.language.clone().unwrap_or_default(),
        track.title.clone().unwrap_or_default(),
        track
            .channel_count
            .map_or_else(String::new, |value| value.to_string()),
        track
            .sample_rate
            .map_or_else(String::new, |value| value.to_string()),
        ordinal.to_string(),
    ]
    .join("\0");
    let digest = Sha256::digest(identity.as_bytes());
    format!("runtime:{kind}:{digest:x}")
}

fn runtime_tracks_payload(catalog: &RuntimeTrackCatalog) -> Value {
    json!({
        "audioTrackKey": catalog.audio.iter().find(|track| track.selected).map(|track| &track.key),
        "subtitleTrackKey": catalog.subtitles.iter().find(|track| track.selected).map(|track| &track.key),
        "subtitleEnabled": catalog.subtitles.iter().any(|track| track.selected),
        "audioTracks": catalog.audio.iter().map(runtime_audio_track_payload).collect::<Vec<_>>(),
        "subtitleTracks": catalog.subtitles.iter().map(runtime_subtitle_track_payload).collect::<Vec<_>>(),
    })
}

fn runtime_audio_track_payload(track: &RuntimeTrack) -> Value {
    json!({
        "key": track.key,
        "label": track.label,
        "language": track.language,
        "codec": track.codec,
        "channels": track.channel_count,
    })
}

fn runtime_subtitle_track_payload(track: &RuntimeTrack) -> Value {
    json!({
        "key": track.key,
        "label": track.label,
        "language": track.language,
        "codec": track.codec,
        "external": track.external,
        "default": track.is_default,
        "forced": track.is_forced,
    })
}

fn source_video_payload(source: &PlaybackSource) -> Value {
    let Some(video) = source.video.as_ref() else {
        return Value::Null;
    };
    json!({
        "codec": video.codec,
        "profile": video.profile,
        "level": video.level,
        "width": video.width,
        "height": video.height,
        "frameRate": video.frame_rate,
        "dynamicRange": video.dynamic_range,
        "rangeType": video.range_type,
        "colorSpace": video.color_space,
        "colorTransfer": video.color_transfer,
        "colorRange": video.color_range,
        "bitDepth": video.bit_depth,
        "maxCll": video.max_content_light_level,
        "maxFall": video.max_frame_average_light_level,
        "dolbyVisionProfile": video.dolby_vision_profile,
        "dolbyVisionLevel": video.dolby_vision_level,
        "dolbyVisionBaseLayerPresent": video.dolby_vision_base_layer_present,
        "dolbyVisionCompatibilityId": video.dolby_vision_compatibility_id,
    })
}

fn prepare_external_subtitle(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    source: &PlaybackSource,
    plan: &PlaybackTrackPlan,
) -> Result<Option<PreparedExternalSubtitle>, LoadFailure> {
    let Some(subtitle_url) = plan.external_subtitle_url.as_ref() else {
        return Ok(None);
    };
    let subtitle_key = plan.subtitle_track_key.as_deref().ok_or_else(|| {
        LoadFailure::new(
            "external_subtitle_selection_invalid",
            "The selected external subtitle is missing its stable track key",
        )
    })?;
    let track = source
        .subtitles
        .iter()
        .find(|track| {
            track.is_external
                && track.key == subtitle_key
                && track.url.as_ref() == Some(subtitle_url)
        })
        .ok_or_else(|| {
            LoadFailure::new(
                "external_subtitle_selection_invalid",
                "The selected external subtitle is no longer available",
            )
        })?;

    prepare_external_subtitle_track(runtime, snapshot, track).map(Some)
}

fn prepare_external_subtitle_track(
    runtime: &MediaStationRuntime,
    snapshot: &SessionSnapshot,
    track: &SubtitleTrack,
) -> Result<PreparedExternalSubtitle, LoadFailure> {
    let subtitle_url = track.url.as_ref().ok_or_else(|| {
        LoadFailure::new(
            "external_subtitle_selection_invalid",
            "The selected external subtitle has no source URL",
        )
    })?;
    runtime.ensure_generation(snapshot.generation)?;
    let download = runtime
        .api
        .download_external_subtitle(&snapshot.session, subtitle_url, MAX_EXTERNAL_SUBTITLE_BYTES)
        .map_err(|error| api_failure("external_subtitle_download_failed", &error))?;
    runtime.ensure_generation(snapshot.generation)?;
    store_external_subtitle(&download, track, subtitle_url)
}

fn store_external_subtitle(
    download: &ExternalSubtitleDownload,
    track: &SubtitleTrack,
    source_url: &Url,
) -> Result<PreparedExternalSubtitle, LoadFailure> {
    let directory = jfn_paths::cache_dir().join(SUBTITLE_CACHE_DIRECTORY);
    store_external_subtitle_in(download, track, source_url, &directory)
}

fn store_external_subtitle_in(
    download: &ExternalSubtitleDownload,
    track: &SubtitleTrack,
    source_url: &Url,
    directory: &Path,
) -> Result<PreparedExternalSubtitle, LoadFailure> {
    let suffix =
        subtitle_suffix(track, source_url, download.content_type.as_deref()).ok_or_else(|| {
            LoadFailure::new(
                "unsupported_external_subtitle_format",
                "The selected external subtitle format is not supported",
            )
        })?;
    fs::create_dir_all(directory).map_err(|error| subtitle_storage_failure(&error))?;
    let mut file = tempfile::Builder::new()
        .prefix(SUBTITLE_FILE_PREFIX)
        .suffix(suffix)
        .tempfile_in(directory)
        .map_err(|error| subtitle_storage_failure(&error))?;
    file.write_all(&download.bytes)
        .and_then(|_| file.flush())
        .map_err(|error| subtitle_storage_failure(&error))?;
    let path = file.path().to_str().ok_or_else(|| {
        log_error("MediaStation external subtitle cache path is not valid UTF-8");
        LoadFailure::new(
            "external_subtitle_storage_failed",
            "The external subtitle could not be stored safely",
        )
    })?;
    let path = CString::new(path).map_err(|_| {
        log_error("MediaStation external subtitle cache path contains an invalid byte");
        LoadFailure::new(
            "external_subtitle_storage_failed",
            "The external subtitle could not be stored safely",
        )
    })?;
    Ok(PreparedExternalSubtitle {
        _file: file,
        path,
        byte_count: download.bytes.len(),
        redirect_count: download.redirect_count,
        target_host: download.target_host.clone(),
    })
}

fn subtitle_suffix(
    track: &SubtitleTrack,
    source_url: &Url,
    content_type: Option<&str>,
) -> Option<&'static str> {
    Path::new(source_url.path())
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(subtitle_suffix_from_value)
        .or_else(|| track.codec.as_deref().and_then(subtitle_suffix_from_value))
        .or_else(|| {
            content_type
                .and_then(|value| value.split(';').next())
                .and_then(subtitle_suffix_from_value)
        })
}

fn subtitle_suffix_from_value(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "srt" | "subrip" | "text/subrip" | "application/x-subrip" | "mov_text" | "text" => {
            Some(".srt")
        }
        "ass" | "application/x-ass" => Some(".ass"),
        "ssa" | "application/x-ssa" => Some(".ssa"),
        "vtt" | "webvtt" | "text/vtt" => Some(".vtt"),
        "ttml" | "application/ttml+xml" => Some(".ttml"),
        "dfxp" | "application/ttaf+xml" => Some(".dfxp"),
        "sub" | "microdvd" | "dvdsub" | "vobsub" => Some(".sub"),
        "sup" | "pgs" | "pgssub" | "hdmv_pgs_subtitle" => Some(".sup"),
        "smi" | "sami" => Some(".smi"),
        "lrc" => Some(".lrc"),
        _ => None,
    }
}

fn subtitle_storage_failure(error: &std::io::Error) -> LoadFailure {
    log_error(&format!(
        "MediaStation external subtitle storage failed: kind={}",
        error.kind()
    ));
    LoadFailure::new(
        "external_subtitle_storage_failed",
        "The external subtitle could not be stored safely",
    )
}

fn session_changed() -> LoadFailure {
    LoadFailure::new(
        "session_changed",
        "The native MediaStation session changed before playback could start",
    )
}

fn api_failure(stage: &'static str, error: &ApiError) -> LoadFailure {
    log_error(&format!(
        "MediaStation API stage failed: stage={stage} code={}",
        api_error_code(error)
    ));
    LoadFailure::new(stage, "A MediaStation server request failed")
}

fn api_error_code(error: &ApiError) -> &'static str {
    match error {
        ApiError::InvalidInput { .. } => "invalid_input",
        ApiError::UnsupportedScheme { .. } => "unsupported_scheme",
        ApiError::EmbeddedCredentials { .. } => "embedded_credentials",
        ApiError::InvalidEndpoint { .. } => "invalid_endpoint",
        ApiError::Transport { .. } => "transport",
        ApiError::HttpStatus { .. } => "http_status",
        ApiError::InvalidJson { .. } => "invalid_json",
        ApiError::MissingField { .. } => "missing_field",
        ApiError::InvalidMediaUrl { .. } => "invalid_media_url",
        ApiError::RedirectLimitExceeded { .. } => "redirect_limit_exceeded",
        ApiError::RedirectMissingLocation { .. } => "redirect_missing_location",
        ApiError::RedirectLocationInvalid { .. } => "redirect_location_invalid",
        ApiError::RedirectSecurityDowngrade { .. } => "redirect_security_downgrade",
        ApiError::ResponseTooLarge { .. } => "response_too_large",
        ApiError::EmptyResponse { .. } => "empty_response",
        ApiError::EmptyPreferenceUpdate => "empty_preference_update",
    }
}

fn playback_session_failure(stage: &'static str, error: &PlaybackSessionError) -> LoadFailure {
    log_error(&format!(
        "MediaStation playback stage failed: stage={stage} code={}",
        playback_session_error_code(error)
    ));
    LoadFailure::new(stage, "The playback URL could not be resolved safely")
}

fn playback_session_error_code(error: &PlaybackSessionError) -> &'static str {
    match error {
        PlaybackSessionError::InvalidInput { .. } => "invalid_input",
        PlaybackSessionError::UnsupportedScheme { .. } => "unsupported_scheme",
        PlaybackSessionError::EmbeddedCredentials { .. } => "embedded_credentials",
        PlaybackSessionError::SourceOriginMismatch { .. } => "source_origin_mismatch",
        PlaybackSessionError::Transport { .. } => "transport",
        PlaybackSessionError::RedirectLimitExceeded { .. } => "redirect_limit_exceeded",
        PlaybackSessionError::RedirectMissingLocation { .. } => "redirect_missing_location",
        PlaybackSessionError::RedirectLocationInvalid { .. } => "redirect_location_invalid",
        PlaybackSessionError::RedirectSecurityDowngrade { .. } => "redirect_security_downgrade",
        PlaybackSessionError::UnexpectedStatus { .. } => "unexpected_status",
        PlaybackSessionError::RangeUnsupported { .. } => "range_unsupported",
        PlaybackSessionError::InvalidResponseHeader { .. } => "invalid_response_header",
    }
}

fn header_encoding_failure(error: HeaderEncodingError) -> LoadFailure {
    log_error(&format!("MediaStation playback headers rejected: {error}"));
    LoadFailure::new(
        "invalid_playback_headers",
        "The native player headers could not be encoded safely",
    )
}

fn log_mpv_error(error: &LoadError) {
    log_error(&format!("MediaStation native player load failed: {error}"));
}

fn dispatch_response(
    layer: &Arc<Inner>,
    request_id: &str,
    operation: &str,
    ok: bool,
    payload: Value,
) {
    if !post_renderer_message(
        Arc::clone(layer),
        "mediaStationResponse",
        vec![
            RendererValue::String(request_id.to_string()),
            RendererValue::String(operation.to_string()),
            RendererValue::Bool(ok),
            RendererValue::String(payload.to_string()),
        ],
    ) {
        log_error("MediaStation response could not be queued for the renderer");
    }
}

fn log_error(message: &str) {
    jfn_logging::log(jfn_logging::CATEGORY_CEF, jfn_logging::LEVEL_ERROR, message);
}

fn log_warn(message: &str) {
    jfn_logging::log(jfn_logging::CATEGORY_CEF, jfn_logging::LEVEL_WARN, message);
}

fn log_debug(message: &str) {
    jfn_logging::log(jfn_logging::CATEGORY_CEF, jfn_logging::LEVEL_DEBUG, message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use jfn_playback::{MediaMetadata, PlaybackSnapshot};
    use std::io::Read as _;
    use std::net::TcpListener;

    fn session(user_id: &str) -> MediaStationSession {
        session_at(
            Url::parse("https://media.example").expect("URL should parse"),
            user_id,
        )
    }

    fn session_at(base_url: Url, user_id: &str) -> MediaStationSession {
        MediaStationSession::new(
            base_url,
            user_id,
            "test-token",
            "MediaBrowser Client=\"MediaStation Windows\", Token=\"test-token\"",
        )
        .expect("session should be valid")
    }

    fn playback_source(base_url: &Url) -> PlaybackSource {
        PlaybackSource {
            media_id: "media-1".to_string(),
            url: base_url
                .join("/Videos/media-1/stream")
                .expect("media URL should resolve"),
            server_credential_query_removed: false,
            container: Some("mkv".to_string()),
            bitrate: Some(10_000_000),
            media_source_id: "source-1".to_string(),
            play_session_id: "play-session-1".to_string(),
            video: None,
            subtitles: Vec::new(),
            audio_tracks: Vec::new(),
        }
    }

    fn playback_event(kind: PlaybackEventKind, position_ms: u64) -> PlaybackEvent {
        PlaybackEvent {
            kind,
            flag: false,
            error_message: String::new(),
            snapshot: PlaybackSnapshot {
                position_us: i64::try_from(position_ms.saturating_mul(1000)).unwrap_or(i64::MAX),
                ..Default::default()
            },
            metadata: MediaMetadata::default(),
            artwork_uri: String::new(),
            can_go_next: false,
            can_go_prev: false,
        }
    }

    fn report_server(request_count: usize) -> (Url, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener.local_addr().expect("address should resolve");
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().expect("request should arrive");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    let read = stream
                        .read(&mut buffer)
                        .expect("request should be readable");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    let Some(header_end) = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|position| position + 4)
                    else {
                        continue;
                    };
                    let content_length = String::from_utf8_lossy(&request[..header_end])
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .or_else(|| line.strip_prefix("Content-Length: "))
                        })
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
                requests.push(String::from_utf8(request).expect("request should be UTF-8"));
                stream
                    .write_all(
                        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .expect("response should be writable");
            }
            requests
        });
        (
            Url::parse(&format!("http://{address}")).expect("URL should parse"),
            server,
        )
    }

    fn external_subtitle_track(url: Url, codec: &str) -> SubtitleTrack {
        SubtitleTrack {
            key: "stream:2".to_string(),
            stream_index: Some(2),
            url: Some(url),
            codec: Some(codec.to_string()),
            language: Some("zh-CN".to_string()),
            label: Some("Chinese".to_string()),
            is_default: true,
            is_forced: false,
            is_external: true,
        }
    }

    #[test]
    fn session_generation_invalidates_work_and_limits_loads() {
        let runtime = Arc::new(MediaStationRuntime::new().expect("runtime should initialize"));
        let first_session = session("user-1");
        let generation = runtime.configure_session(first_session.clone());
        assert_eq!(generation, 1);
        assert_eq!(runtime.configure_session(first_session), generation);

        let (_, guard) = runtime
            .begin_request("request-1")
            .expect("first request should start");
        let error = match runtime.begin_request("request-2") {
            Err(error) => error,
            Ok(_) => panic!("only one request may resolve at a time"),
        };
        assert_eq!(error.code, "load_request_in_progress");

        let next_generation = runtime.configure_session(session("user-2"));
        assert_ne!(generation, next_generation);
        let error = runtime
            .ensure_generation(generation)
            .expect_err("old work must be invalidated after a session switch");
        assert_eq!(error.code, "session_changed");

        drop(guard);
        runtime
            .begin_request("request-2")
            .expect("request slot should be released after completion");
    }

    #[test]
    fn authentication_requests_are_bounded_independently_from_loads() {
        let runtime = Arc::new(MediaStationRuntime::new().expect("runtime should initialize"));
        runtime.configure_session(session("user-1"));
        let (_, load_guard) = runtime
            .begin_request("load-1")
            .expect("load request should start");
        let auth_guard = runtime
            .begin_auth_request("auth-1")
            .expect("authentication should start alongside a load");

        let error = match runtime.begin_auth_request("auth-2") {
            Err(error) => error,
            Ok(_) => panic!("only one authentication request may be active"),
        };
        assert_eq!(error.code, "authentication_in_progress");

        drop(auth_guard);
        runtime
            .begin_auth_request("auth-2")
            .expect("authentication slot should be released");
        drop(load_guard);
    }

    #[test]
    fn playback_interpolation_mode_accepts_only_explicit_session_values() {
        assert_eq!(
            parse_load_interpolation_mode_value("off").expect("off should be valid"),
            InterpolationMode::Off
        );
        assert_eq!(
            parse_load_interpolation_mode_value("2x").expect("2x should be valid"),
            InterpolationMode::X2
        );
        for stale_or_invalid in ["auto", "60", "", "on"] {
            let error = parse_load_interpolation_mode_value(stale_or_invalid)
                .expect_err("only the playback control's explicit values are accepted");
            assert_eq!(error.code, "frame_interpolation_mode_invalid");
        }
    }

    #[test]
    fn playback_interpolation_model_accepts_only_bundled_models() {
        assert_eq!(
            parse_interpolation_model_value("rife-v4.26").expect("v4.26 should be valid"),
            InterpolationModel::RifeV426
        );
        assert_eq!(
            parse_interpolation_model_value("rife-v4.25-lite").expect("v4.25 Lite should be valid"),
            InterpolationModel::RifeV425Lite
        );
        assert_eq!(
            parse_interpolation_model_value("rife-v4.26-scale0.5")
                .expect("v4.26 scale=0.5 should be valid"),
            InterpolationModel::RifeV426Scale05
        );
        for invalid in ["", "auto", "rife-v4.26-heavy"] {
            let error = parse_interpolation_model_value(invalid)
                .expect_err("unbundled models must be rejected");
            assert_eq!(error.code, "frame_interpolation_model_invalid");
        }
    }

    #[test]
    fn stale_authentication_cannot_reconfigure_a_cleared_session() {
        let runtime = Arc::new(MediaStationRuntime::new().expect("runtime should initialize"));
        runtime.configure_session(session("user-1"));
        let guard = runtime
            .begin_auth_request("auth-1")
            .expect("authentication should start");
        runtime.clear_session();
        let replacement = session("user-2");
        let stored = StoredSession::new(
            replacement.base_url.clone(),
            replacement.user_id.clone(),
            "Replacement User",
            replacement.access_token_secret(),
        )
        .expect("stored session should be valid");

        let error = runtime
            .commit_persisted_session(
                guard.generation,
                replacement,
                "Replacement User".to_string(),
                &stored,
            )
            .expect_err("stale authentication must not commit");

        assert_eq!(error.code, "session_changed");
        assert_eq!(runtime.session_status_payload()["configured"], false);
    }

    #[test]
    fn session_status_contains_only_non_sensitive_account_fields() {
        let runtime = MediaStationRuntime::new().expect("runtime should initialize");
        runtime.configure_session_profile(session("user-1"), "Test User".to_string(), true);

        let payload = runtime.session_status_payload();
        let text = payload.to_string();

        assert_eq!(payload["configured"], true);
        assert_eq!(payload["persisted"], true);
        assert_eq!(payload["userId"], "user-1");
        assert_eq!(payload["userName"], "Test User");
        assert!(!text.contains("test-token"));
        assert!(!text.to_ascii_lowercase().contains("authorization"));
    }

    #[test]
    fn catalog_and_image_requests_enforce_bounded_concurrency() {
        let runtime = Arc::new(MediaStationRuntime::new().expect("runtime should initialize"));
        runtime.configure_session(session("user-1"));

        let mut catalog_guards = Vec::new();
        for index in 0..MAX_ACTIVE_CATALOG_REQUESTS {
            let (_, guard) = runtime
                .begin_catalog_request(&format!("catalog-{index}"))
                .expect("catalog request inside limit should start");
            catalog_guards.push(guard);
        }
        let catalog_error = runtime
            .begin_catalog_request("catalog-over-limit")
            .err()
            .expect("catalog request above limit should fail");
        assert_eq!(catalog_error.code, "catalog_request_limit");
        catalog_guards.pop();
        runtime
            .begin_catalog_request("catalog-after-drop")
            .expect("dropping a catalog guard should release capacity");

        let mut image_guards = Vec::new();
        for index in 0..MAX_ACTIVE_IMAGE_REQUESTS {
            let (_, guard) = runtime
                .begin_image_request(&format!("image-{index}"))
                .expect("image request inside limit should start");
            image_guards.push(guard);
        }
        let image_error = runtime
            .begin_image_request("image-over-limit")
            .err()
            .expect("image request above limit should fail");
        assert_eq!(image_error.code, "image_request_limit");
        image_guards.pop();
        runtime
            .begin_image_request("image-after-drop")
            .expect("dropping an image guard should release capacity");
    }

    #[test]
    fn image_request_rejects_unsupported_type_and_width() {
        let type_error = parse_image_descriptor(
            r#"{"key":"item:logo:tag","itemId":"item","type":"logo","tag":"tag"}"#,
            360,
        )
        .expect_err("unsupported image type should fail");
        assert_eq!(type_error.code, "invalid_image_request");

        for width in [79, 2_001] {
            let width_error = parse_image_descriptor(
                r#"{"key":"item:primary:tag","itemId":"item","type":"primary","tag":"tag"}"#,
                width,
            )
            .expect_err("out-of-range width should fail");
            assert_eq!(width_error.code, "invalid_image_width");
        }
    }

    #[test]
    fn media_card_payload_contains_only_whitelisted_descriptors() {
        let image = MediaImageRef {
            item_id: "item-1".to_string(),
            image_type: MediaImageType::Primary,
            image_index: None,
            tag: "image-tag".to_string(),
        };
        let card = MediaCard {
            id: "item-1".to_string(),
            title: "Example".to_string(),
            media_type: "Movie".to_string(),
            overview: "Overview".to_string(),
            collection_type: None,
            year: Some(2026),
            duration_ms: 90_000,
            resume_position_ms: 10_000,
            played: false,
            index_number: None,
            parent_index_number: None,
            parent_id: None,
            season_id: None,
            series_id: None,
            series_name: None,
            community_rating: Some(8.0),
            official_rating: None,
            genres: vec!["Drama".to_string()],
            video_codec: Some("hevc".to_string()),
            video_profile: Some("Main 10".to_string()),
            video_width: Some(3_840),
            video_height: Some(2_160),
            dynamic_range: Some("HDR10".to_string()),
            primary_image: Some(image),
            landscape_image: None,
            backdrop_image: None,
        };

        let text = media_card_payload(&card).to_string().to_ascii_lowercase();
        assert!(!text.contains("authorization"));
        assert!(!text.contains("token"));
        assert!(!text.contains("https://"));
        assert!(!text.contains("http://"));
    }

    #[test]
    fn playback_track_payload_contains_descriptors_without_urls() {
        let mut source = playback_source(&Url::parse("https://media.example").expect("URL"));
        source.audio_tracks.push(jfn_mediastation::AudioTrack {
            key: "stream:1".to_string(),
            stream_index: Some(1),
            codec: Some("eac3".to_string()),
            language: Some("eng".to_string()),
            label: Some("English Atmos".to_string()),
            channel_count: Some(8),
        });
        source.subtitles.push(SubtitleTrack {
            key: "stream:2".to_string(),
            stream_index: Some(2),
            url: Some(
                Url::parse("https://media.example/subtitle.ass?api_key=secret")
                    .expect("subtitle URL"),
            ),
            codec: Some("ass".to_string()),
            language: Some("zho".to_string()),
            label: Some("Chinese Simplified".to_string()),
            is_default: true,
            is_forced: false,
            is_external: true,
        });

        let payload = json!({
            "audioTracks": audio_tracks_payload(&source),
            "subtitleTracks": subtitle_tracks_payload(&source),
        });
        let text = payload.to_string().to_ascii_lowercase();

        assert_eq!(payload["audioTracks"][0]["key"], "stream:1");
        assert_eq!(payload["subtitleTracks"][0]["key"], "stream:2");
        assert!(!text.contains("media.example"));
        assert!(!text.contains("api_key"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("url"));
    }

    #[allow(clippy::too_many_arguments)] // Mirrors the mpv track-list fields used by this test.
    fn mpv_track_node(
        kind: &str,
        id: i64,
        ff_index: i64,
        codec: &str,
        language: &str,
        title: &str,
        selected: bool,
        external: bool,
    ) -> jfn_mpv::Node {
        jfn_mpv::Node::Map(vec![
            ("type".to_string(), jfn_mpv::Node::String(kind.to_string())),
            ("id".to_string(), jfn_mpv::Node::Int(id)),
            ("ff-index".to_string(), jfn_mpv::Node::Int(ff_index)),
            (
                "codec".to_string(),
                jfn_mpv::Node::String(codec.to_string()),
            ),
            (
                "lang".to_string(),
                jfn_mpv::Node::String(language.to_string()),
            ),
            (
                "title".to_string(),
                jfn_mpv::Node::String(title.to_string()),
            ),
            ("selected".to_string(), jfn_mpv::Node::Flag(selected)),
            ("external".to_string(), jfn_mpv::Node::Flag(external)),
        ])
    }

    #[test]
    fn runtime_track_catalog_merges_server_keys_with_complete_mpv_tracks() {
        let base_url = Url::parse("https://media.example").expect("URL");
        let mut source = playback_source(&base_url);
        source.audio_tracks.push(jfn_mediastation::AudioTrack {
            key: "stream:7".to_string(),
            stream_index: Some(7),
            codec: Some("aac".to_string()),
            language: Some("eng".to_string()),
            label: Some("Server audio".to_string()),
            channel_count: Some(2),
        });
        source.subtitles.extend([
            SubtitleTrack {
                key: "stream:14".to_string(),
                stream_index: Some(14),
                url: None,
                codec: Some("ass".to_string()),
                language: Some("zho".to_string()),
                label: Some("Server embedded".to_string()),
                is_default: false,
                is_forced: false,
                is_external: false,
            },
            external_subtitle_track(
                base_url
                    .join("/Subtitles/external.srt?token=private")
                    .expect("subtitle URL"),
                "subrip",
            ),
        ]);
        source.subtitles[1].key = "stream:99".to_string();
        source.subtitles[1].stream_index = Some(99);

        let node = jfn_mpv::Node::Array(vec![
            mpv_track_node("audio", 2, 7, "aac", "eng", "Stereo", true, false),
            mpv_track_node("audio", 8, 8, "ac3", "jpn", "5.1", false, false),
            mpv_track_node("audio", 13, 9, "truehd", "zho", "Atmos", false, false),
            mpv_track_node("sub", 20, 14, "ass", "zho", "Chinese", true, false),
            mpv_track_node("sub", 27, 15, "subrip", "eng", "English", false, false),
            mpv_track_node("sub", 35, 99, "subrip", "zho", "External", false, true),
        ]);

        let catalog = runtime_track_catalog(&source, &node).expect("catalog should parse");
        let repeated = runtime_track_catalog(&source, &node).expect("catalog should be stable");

        assert_eq!(catalog.audio.len(), 3);
        assert_eq!(
            catalog
                .audio
                .iter()
                .filter_map(|track| track.mpv_id)
                .collect::<Vec<_>>(),
            vec![2, 8, 13]
        );
        assert_eq!(catalog.audio[0].key, "stream:7");
        assert!(catalog.audio[1].key.starts_with("runtime:audio:"));
        assert!(catalog.audio[2].key.starts_with("runtime:audio:"));
        assert_eq!(catalog.audio[1].key, repeated.audio[1].key);
        assert_eq!(catalog.audio[2].key, repeated.audio[2].key);

        assert_eq!(catalog.subtitles.len(), 3);
        assert_eq!(catalog.subtitles[0].key, "stream:14");
        assert!(catalog.subtitles[1].key.starts_with("runtime:subtitle:"));
        assert_eq!(catalog.subtitles[2].key, "stream:99");
        assert!(!catalog.subtitles[0].external);
        assert!(catalog.subtitles[2].external);

        let payload = runtime_tracks_payload(&catalog);
        let text = payload.to_string().to_ascii_lowercase();
        assert!(payload["audioTracks"][0].get("id").is_none());
        assert!(payload["audioTracks"][0].get("mpvId").is_none());
        assert!(!text.contains("media.example"));
        assert!(!text.contains("private"));
        assert!(!text.contains("token"));
    }

    #[test]
    fn playback_event_payload_does_not_expose_native_error_or_urls() {
        let mut event = playback_event(PlaybackEventKind::Error, 42_000);
        event.error_message =
            "failed https://cdn.example/video.mkv?token=private-secret".to_string();
        event.artwork_uri = "https://media.example/image?api_key=private-secret".to_string();

        let payload = playback_event_payload(&event);
        let text = payload.to_string().to_ascii_lowercase();
        assert_eq!(payload["kind"], "error");
        assert_eq!(payload["positionMs"], 42_000);
        assert!(!text.contains("private-secret"));
        assert!(!text.contains("cdn.example"));
        assert!(!text.contains("media.example"));
        assert!(!text.contains("error_message"));
        assert!(!text.contains("artwork"));
        assert!(payload["errorCode"].is_null());
    }

    #[test]
    fn playback_event_payload_exposes_only_known_interpolation_failure_code() {
        let mut event = playback_event(PlaybackEventKind::Error, 42_000);
        event.error_message = RIFE_FILTER_INACTIVE.to_string();

        let payload = playback_event_payload(&event);
        assert_eq!(payload["errorCode"], RIFE_FILTER_INACTIVE);
    }

    #[test]
    fn interpolation_filter_health_fails_explicitly_without_fallback() {
        let mut seen = false;
        let mut missing = 0;
        assert!(!interpolation_filter_failed(&mut seen, &mut missing, None));
        assert!(!interpolation_filter_failed(&mut seen, &mut missing, None));
        assert!(interpolation_filter_failed(&mut seen, &mut missing, None));

        seen = false;
        missing = 0;
        assert!(!interpolation_filter_failed(
            &mut seen,
            &mut missing,
            Some("initializing"),
        ));
        assert!(seen);
        assert!(interpolation_filter_failed(&mut seen, &mut missing, None));

        assert!(interpolation_filter_failed(
            &mut seen,
            &mut missing,
            Some("unexpected"),
        ));
    }

    #[test]
    fn native_authorization_rejects_header_injection() {
        let error = native_authorization_header(Some("token\r\nInjected: value"))
            .expect_err("control characters must fail");

        assert_eq!(error.code, "invalid_access_token");
    }

    #[test]
    fn playback_reports_follow_first_frame_and_keep_final_position() {
        let base_url = Url::parse("https://media.example").expect("URL should parse");
        let snapshot = SessionSnapshot {
            session: session_at(base_url.clone(), "user-1"),
            generation: 7,
        };
        let source = playback_source(&base_url);
        let mut state = RuntimeState {
            session: Some(snapshot.session.clone()),
            session_profile: None,
            generation: snapshot.generation,
            active_request_ids: HashSet::new(),
            active_auth_request_ids: HashSet::new(),
            active_catalog_request_ids: HashSet::new(),
            active_image_request_ids: HashSet::new(),
            active_subtitle: None,
            active_interpolation: None,
            active_report: None,
        };
        let preference = PlaybackTrackPreference {
            configured: false,
            subtitle_enabled: false,
            subtitle_track_key: None,
            audio_track_key: None,
        };
        assert!(
            activate_playback_report(&mut state, &snapshot, &source, &preference, 250).is_none()
        );
        let now = Instant::now();

        assert!(
            plan_playback_reports(
                &mut state,
                &playback_event(PlaybackEventKind::PositionChanged, 500),
                now,
            )
            .is_empty()
        );
        let playing = plan_playback_reports(
            &mut state,
            &playback_event(PlaybackEventKind::Started, 600),
            now,
        );
        assert_eq!(playing.len(), 1);
        assert!(matches!(playing[0].kind, ReportKind::Playing));
        assert_eq!(playing[0].position_ms, 600);

        assert!(
            plan_playback_reports(
                &mut state,
                &playback_event(PlaybackEventKind::PositionChanged, 9_000),
                now + Duration::from_secs(9),
            )
            .is_empty()
        );
        let periodic = plan_playback_reports(
            &mut state,
            &playback_event(PlaybackEventKind::PositionChanged, 10_000),
            now + PROGRESS_REPORT_INTERVAL,
        );
        assert!(matches!(
            periodic[0].kind,
            ReportKind::Progress { paused: false }
        ));

        let paused = plan_playback_reports(
            &mut state,
            &playback_event(PlaybackEventKind::Paused, 11_000),
            now + Duration::from_secs(11),
        );
        assert!(matches!(
            paused[0].kind,
            ReportKind::Progress { paused: true }
        ));
        let resumed = plan_playback_reports(
            &mut state,
            &playback_event(PlaybackEventKind::Started, 11_000),
            now + Duration::from_secs(12),
        );
        assert!(matches!(
            resumed[0].kind,
            ReportKind::Progress { paused: false }
        ));

        let seeked = plan_playback_reports(
            &mut state,
            &playback_event(PlaybackEventKind::Seeked, 40_000),
            now + Duration::from_secs(13),
        );
        assert_eq!(seeked[0].position_ms, 40_000);
        let stopped = plan_playback_reports(
            &mut state,
            &playback_event(PlaybackEventKind::Canceled, 0),
            now + Duration::from_secs(14),
        );
        assert!(matches!(stopped[0].kind, ReportKind::Stopped));
        assert_eq!(stopped[0].position_ms, 40_000);
        assert!(state.active_report.is_none());
    }

    #[test]
    fn playback_report_worker_preserves_protocol_order() {
        let (base_url, server) = report_server(3);
        let api = MediaStationApiClient::new("MediaStationWindows/0.1")
            .expect("client should initialize");
        let reporter = PlaybackReportDispatcher::new(api);
        assert!(reporter.is_available());
        let session = session_at(base_url.clone(), "user-1");
        let source = playback_source(&base_url);
        for (kind, position_ms) in [
            (ReportKind::Playing, 1_000),
            (ReportKind::Progress { paused: true }, 2_000),
            (ReportKind::Stopped, 3_000),
        ] {
            reporter
                .enqueue(ReportJob {
                    generation: 1,
                    session: session.clone(),
                    source: source.clone(),
                    kind,
                    position_ms,
                })
                .expect("report should queue");
        }
        reporter.shutdown();
        let requests = server.join().expect("server thread should finish");

        assert!(requests[0].starts_with("POST /Sessions/Playing HTTP/1.1"));
        assert!(requests[1].starts_with("POST /Sessions/Playing/Progress HTTP/1.1"));
        assert!(requests[2].starts_with("POST /Sessions/Playing/Stopped HTTP/1.1"));
        let bodies = requests
            .iter()
            .map(|request| {
                serde_json::from_str::<Value>(
                    request
                        .split("\r\n\r\n")
                        .nth(1)
                        .expect("request body should exist"),
                )
                .expect("request body should be JSON")
            })
            .collect::<Vec<_>>();
        assert_eq!(bodies[0]["PositionTicks"], 10_000_000_u64);
        assert_eq!(bodies[1]["PositionTicks"], 20_000_000_u64);
        assert_eq!(bodies[1]["IsPaused"], true);
        assert_eq!(bodies[2]["PositionTicks"], 30_000_000_u64);
        assert_eq!(bodies[2]["IsPaused"], true);
    }

    #[test]
    fn external_subtitle_suffix_uses_only_supported_formats() {
        let url =
            Url::parse("https://media.example/subtitles/1/Stream.ass").expect("URL should parse");
        let track = external_subtitle_track(url.clone(), "subrip");

        assert_eq!(subtitle_suffix(&track, &url, None), Some(".ass"));

        let unknown_url =
            Url::parse("https://media.example/subtitles/1/Stream").expect("URL should parse");
        let unknown_track = external_subtitle_track(unknown_url.clone(), "unknown");
        assert_eq!(
            subtitle_suffix(
                &unknown_track,
                &unknown_url,
                Some("text/vtt; charset=utf-8")
            ),
            Some(".vtt")
        );
        assert_eq!(subtitle_suffix(&unknown_track, &unknown_url, None), None);
    }

    #[test]
    fn external_subtitle_file_is_private_to_runtime_lifetime() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let url =
            Url::parse("https://media.example/subtitles/1/Stream.srt").expect("URL should parse");
        let track = external_subtitle_track(url.clone(), "subrip");
        let download = ExternalSubtitleDownload {
            bytes: b"1\n00:00:00,000 --> 00:00:01,000\nSubtitle\n".to_vec(),
            content_type: Some("application/x-subrip".to_string()),
            redirect_count: 0,
            target_host: "media.example".to_string(),
        };
        let prepared = store_external_subtitle_in(&download, &track, &url, directory.path())
            .expect("subtitle should be stored");
        let path = prepared._file.path().to_path_buf();
        assert!(path.exists());
        assert_eq!(
            fs::read(&path).expect("subtitle should be readable"),
            download.bytes
        );

        let runtime = MediaStationRuntime::new().expect("runtime should initialize");
        runtime.state.lock().active_subtitle = Some(prepared);
        runtime.release_playback_resources();

        assert!(!path.exists());
    }
}
