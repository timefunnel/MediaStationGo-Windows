use crate::{PlaybackSource, PlaybackTrackPreference, PlaybackTrackPreferenceUpdate};
use url::Url;

pub const TRACK_DISABLE: i64 = 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackTrackPlan {
    pub video_track: i64,
    pub audio_track: i64,
    pub subtitle_track: i64,
    pub external_subtitle_url: Option<Url>,
    pub audio_track_key: Option<String>,
    pub subtitle_track_key: Option<String>,
    pub preference_correction: Option<PlaybackTrackPreferenceUpdate>,
}

pub fn build_playback_track_plan(
    source: &PlaybackSource,
    preference: &PlaybackTrackPreference,
) -> PlaybackTrackPlan {
    let preferred_audio = preference.audio_track_key.as_deref().and_then(|key| {
        source
            .audio_tracks
            .iter()
            .position(|track| track.key == key)
    });
    let audio_index = preferred_audio.or_else(|| (!source.audio_tracks.is_empty()).then_some(0));
    let audio_track = audio_index
        .map(|index| index as i64 + 1)
        .unwrap_or(TRACK_DISABLE);
    let audio_track_key = audio_index.map(|index| source.audio_tracks[index].key.clone());

    let preferred_subtitle = preference
        .subtitle_track_key
        .as_deref()
        .and_then(|key| source.subtitles.iter().position(|track| track.key == key));
    let subtitle_index = if preference.subtitle_enabled {
        // A saved key that no longer matches this container falls back to
        // the default/forced track instead of silently disabling subtitles.
        preferred_subtitle.or_else(|| {
            source
                .subtitles
                .iter()
                .position(|track| track.is_default || track.is_forced)
        })
    } else {
        None
    };
    let selected_subtitle = subtitle_index.map(|index| &source.subtitles[index]);
    let external_subtitle_url = selected_subtitle.and_then(|track| track.url.clone());
    let subtitle_track = selected_subtitle
        .filter(|track| !track.is_external)
        .map(|selected| {
            source
                .subtitles
                .iter()
                .filter(|track| !track.is_external)
                .position(|track| track.key == selected.key)
                .map(|index| index as i64 + 1)
                .unwrap_or(TRACK_DISABLE)
        })
        .unwrap_or(TRACK_DISABLE);
    let subtitle_track_key = selected_subtitle.map(|track| track.key.clone());

    let mut correction = PlaybackTrackPreferenceUpdate::default();
    if preference.configured
        && preference.audio_track_key.as_deref() != audio_track_key.as_deref()
        && audio_track_key.is_some()
    {
        correction.audio_track_key = audio_track_key.clone();
    }
    if preference.configured
        && preference.subtitle_enabled
        && preference.subtitle_track_key.as_deref() != subtitle_track_key.as_deref()
    {
        if let Some(key) = &subtitle_track_key {
            correction.subtitle_track_key = Some(key.clone());
        } else {
            correction.subtitle_enabled = Some(false);
        }
    }
    let preference_correction = (correction.subtitle_enabled.is_some()
        || correction.subtitle_track_key.is_some()
        || correction.audio_track_key.is_some())
    .then_some(correction);

    PlaybackTrackPlan {
        video_track: 1,
        audio_track,
        subtitle_track,
        external_subtitle_url,
        audio_track_key,
        subtitle_track_key,
        preference_correction,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioTrack, SubtitleTrack};
    use url::Url;

    fn source() -> PlaybackSource {
        PlaybackSource {
            media_id: "media-1".to_string(),
            url: Url::parse("https://media.example/video").expect("URL should parse"),
            server_credential_query_removed: false,
            container: Some("mkv".to_string()),
            bitrate: None,
            media_source_id: "source-1".to_string(),
            play_session_id: "session-1".to_string(),
            video: None,
            subtitles: vec![
                SubtitleTrack {
                    key: "stream:2".to_string(),
                    stream_index: Some(2),
                    url: None,
                    codec: Some("ass".to_string()),
                    language: Some("eng".to_string()),
                    label: None,
                    is_default: true,
                    is_forced: false,
                    is_external: false,
                },
                SubtitleTrack {
                    key: "stream:3".to_string(),
                    stream_index: Some(3),
                    url: Some(
                        Url::parse("https://media.example/subtitle.srt").expect("URL should parse"),
                    ),
                    codec: Some("srt".to_string()),
                    language: Some("zho".to_string()),
                    label: None,
                    is_default: false,
                    is_forced: false,
                    is_external: true,
                },
            ],
            audio_tracks: vec![
                AudioTrack {
                    key: "stream:1".to_string(),
                    stream_index: Some(1),
                    codec: Some("aac".to_string()),
                    language: Some("eng".to_string()),
                    label: None,
                    channel_count: Some(2),
                },
                AudioTrack {
                    key: "stream:4".to_string(),
                    stream_index: Some(4),
                    codec: Some("truehd".to_string()),
                    language: Some("zho".to_string()),
                    label: None,
                    channel_count: Some(8),
                },
            ],
        }
    }

    #[test]
    fn stable_keys_map_to_type_relative_mpv_tracks() {
        let preference = PlaybackTrackPreference {
            configured: true,
            subtitle_enabled: true,
            subtitle_track_key: Some("stream:2".to_string()),
            audio_track_key: Some("stream:4".to_string()),
        };

        let plan = build_playback_track_plan(&source(), &preference);

        assert_eq!(plan.video_track, 1);
        assert_eq!(plan.audio_track, 2);
        assert_eq!(plan.subtitle_track, 1);
        assert_eq!(plan.external_subtitle_url, None);
        assert_eq!(plan.preference_correction, None);
    }

    #[test]
    fn external_subtitle_is_loaded_by_url() {
        let preference = PlaybackTrackPreference {
            configured: true,
            subtitle_enabled: true,
            subtitle_track_key: Some("stream:3".to_string()),
            audio_track_key: Some("stream:1".to_string()),
        };

        let plan = build_playback_track_plan(&source(), &preference);

        assert_eq!(plan.subtitle_track, TRACK_DISABLE);
        assert_eq!(
            plan.external_subtitle_url.as_ref().map(Url::as_str),
            Some("https://media.example/subtitle.srt")
        );
    }

    #[test]
    fn stale_saved_subtitle_falls_back_to_container_default() {
        let preference = PlaybackTrackPreference {
            configured: true,
            subtitle_enabled: true,
            subtitle_track_key: Some("missing-subtitle".to_string()),
            audio_track_key: Some("missing-audio".to_string()),
        };

        let plan = build_playback_track_plan(&source(), &preference);
        let correction = plan
            .preference_correction
            .expect("invalid preferences should be corrected");

        // The stale audio key falls back to the first audio track.
        assert_eq!(plan.audio_track, 1);
        assert_eq!(correction.audio_track_key.as_deref(), Some("stream:1"));
        // The stale subtitle key falls back to the container default track
        // (stream:2, internal eng) instead of disabling subtitles entirely.
        assert_eq!(plan.subtitle_track, 1);
        assert_eq!(plan.subtitle_track_key.as_deref(), Some("stream:2"));
        assert_eq!(plan.external_subtitle_url, None);
        assert_eq!(correction.subtitle_track_key.as_deref(), Some("stream:2"));
        assert_eq!(correction.subtitle_enabled, None);
    }

    #[test]
    fn stale_saved_subtitle_with_no_default_disables_subtitles() {
        let mut source = source();
        source.subtitles[0].is_default = false;
        source.subtitles[0].is_forced = false;
        let preference = PlaybackTrackPreference {
            configured: true,
            subtitle_enabled: true,
            subtitle_track_key: Some("missing-subtitle".to_string()),
            audio_track_key: None,
        };

        let plan = build_playback_track_plan(&source, &preference);
        let correction = plan
            .preference_correction
            .expect("invalid preferences should be corrected");

        assert_eq!(plan.subtitle_track, TRACK_DISABLE);
        assert_eq!(plan.subtitle_track_key, None);
        assert_eq!(correction.subtitle_enabled, Some(false));
    }
}
