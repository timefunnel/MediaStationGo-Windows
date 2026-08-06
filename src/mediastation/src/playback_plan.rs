use crate::{PlaybackSource, PlaybackTrackPreference, PlaybackTrackPreferenceUpdate};
use url::Url;

pub const TRACK_DISABLE: i64 = 0;

fn descriptor_has_code(descriptor: &str, codes: &[&str]) -> bool {
    descriptor
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|token| codes.contains(&token))
}

fn chinese_subtitle_variant(track: &crate::SubtitleTrack) -> Option<u8> {
    let language = track
        .language
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .replace('_', "-");
    let label = track
        .label
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let descriptor = format!("{language} {label}");
    if ["zh-cn", "zh-sg", "zh-hans", "chs", "sc"]
        .iter()
        .any(|value| language == *value)
        || descriptor_has_code(&descriptor, &["chs", "sc"])
        || descriptor.contains("simplified chinese")
        || descriptor.contains("chinese simplified")
        || descriptor.contains("简体")
        || descriptor.contains("简中")
    {
        return Some(0);
    }
    if ["zh-tw", "zh-hk", "zh-mo", "zh-hant", "cht", "tc"]
        .iter()
        .any(|value| language == *value)
        || descriptor_has_code(&descriptor, &["cht", "tc"])
        || descriptor.contains("traditional chinese")
        || descriptor.contains("chinese traditional")
        || descriptor.contains("繁体")
        || descriptor.contains("繁中")
    {
        return Some(2);
    }
    if ["zh", "zho", "chi", "chinese", "cn"]
        .iter()
        .any(|value| language == *value)
        || descriptor_has_code(&descriptor, &["zh", "zho", "chi", "cn"])
        || descriptor.contains("chinese")
        || descriptor.contains("中文")
        || descriptor.contains("中字")
    {
        return Some(1);
    }
    None
}

fn preferred_chinese_subtitle(source: &PlaybackSource) -> Option<usize> {
    source
        .subtitles
        .iter()
        .enumerate()
        .filter_map(|(index, track)| {
            chinese_subtitle_variant(track).map(|variant| {
                (
                    index,
                    (
                        u8::from(track.is_forced),
                        u8::from(!track.is_default),
                        variant,
                        index,
                    ),
                )
            })
        })
        .min_by_key(|(_, rank)| *rank)
        .map(|(index, _)| index)
}

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
        // Chinese before the container default instead of silently disabling
        // subtitles or choosing a non-Chinese default on every episode.
        preferred_subtitle
            .or_else(|| preferred_chinese_subtitle(source))
            .or_else(|| {
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
            standard_emby_stream: false,
            server_credential_query_removed: false,
            container: Some("mkv".to_string()),
            bitrate: None,
            media_source_id: Some("source-1".to_string()),
            play_session_id: Some("session-1".to_string()),
            default_audio_stream_index: None,
            default_subtitle_stream_index: None,
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
    fn stale_saved_subtitle_falls_back_to_chinese_track() {
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
        // The stale subtitle key follows the Chinese track even though the
        // container marks an English subtitle as default.
        assert_eq!(plan.subtitle_track, TRACK_DISABLE);
        assert_eq!(plan.subtitle_track_key.as_deref(), Some("stream:3"));
        assert_eq!(
            plan.external_subtitle_url.as_ref().map(Url::as_str),
            Some("https://media.example/subtitle.srt")
        );
        assert_eq!(correction.subtitle_track_key.as_deref(), Some("stream:3"));
        assert_eq!(correction.subtitle_enabled, None);
    }

    #[test]
    fn unconfigured_preference_enables_best_chinese_subtitle() {
        let preference = PlaybackTrackPreference {
            configured: false,
            subtitle_enabled: true,
            subtitle_track_key: None,
            audio_track_key: None,
        };

        let plan = build_playback_track_plan(&source(), &preference);

        assert_eq!(plan.subtitle_track_key.as_deref(), Some("stream:3"));
        assert!(plan.external_subtitle_url.is_some());
        assert_eq!(plan.preference_correction, None);
    }

    #[test]
    fn unconfigured_preference_recognizes_chinese_label_code() {
        let mut source = source();
        source.subtitles[1].language = Some("und".to_string());
        source.subtitles[1].label = Some("CHS & ENG".to_string());
        let preference = PlaybackTrackPreference {
            configured: false,
            subtitle_enabled: true,
            subtitle_track_key: None,
            audio_track_key: None,
        };

        let plan = build_playback_track_plan(&source, &preference);

        assert_eq!(plan.subtitle_track_key.as_deref(), Some("stream:3"));
    }

    #[test]
    fn stale_saved_subtitle_with_no_default_disables_subtitles() {
        let mut source = source();
        source.subtitles[0].is_default = false;
        source.subtitles[0].is_forced = false;
        source.subtitles[1].language = Some("spa".to_string());
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
