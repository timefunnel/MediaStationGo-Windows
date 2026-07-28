//! MediaStationGo protocol and playback-session support.

mod api;
mod playback_plan;
mod playback_session;

pub use api::{
    ApiError, AudioTrack, AuthenticationResult, ExternalSubtitleDownload, MediaCard, MediaDetail,
    MediaHome, MediaImage, MediaImageRef, MediaImageType, MediaLibrarySection, MediaPage,
    MediaStationApiClient, MediaStationSession, PlaybackSource, PlaybackTrackPreference,
    PlaybackTrackPreferenceUpdate, SubtitleTrack, VideoStream,
};
pub use playback_plan::{PlaybackTrackPlan, TRACK_DISABLE, build_playback_track_plan};
pub use playback_session::{
    DeliveryMode, HeaderEncodingError, HeaderMap, PlaybackSession, PlaybackSessionError,
    PlaybackSessionMetrics, PlaybackSessionResolver, ProbeRequest, ProbeResponse, ProbeTransport,
    ResolveInput, ServerAuth, SessionExpirySource, TransportError, UreqTransport,
};
