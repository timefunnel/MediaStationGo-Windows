//! MediaStationGo protocol and playback-session support.

mod api;
mod playback_plan;
mod playback_session;

pub use api::{
    ApiError, AudioTrack, AuthenticationResult, ExternalSubtitleDownload, MediaCard, MediaDetail,
    MediaHome, MediaImage, MediaImageRef, MediaImageType, MediaLibrarySection, MediaPage,
    MediaStationApiClient, MediaStationClientProfile, MediaStationConnectionProfile,
    MediaStationProxyMode, MediaStationSession, PlaybackPreferencePersistence, PlaybackSource,
    PlaybackTrackPreference, PlaybackTrackPreferenceState, PlaybackTrackPreferenceUpdate,
    SubtitleTrack, VideoStream,
};
pub use playback_plan::{
    PlaybackTrackPlan, TRACK_DISABLE, build_playback_track_plan, chinese_subtitle_preference_rank,
};
pub use playback_session::{
    DeliveryMode, HeaderEncodingError, HeaderMap, PlaybackSession, PlaybackSessionError,
    PlaybackSessionMetrics, PlaybackSessionResolver, ProbeRequest, ProbeResponse, ProbeTransport,
    ResolveInput, ServerAuth, SessionExpirySource, TransportError, UreqTransport,
};
