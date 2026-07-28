use jfn_mediastation::{
    MediaStationApiClient, MediaStationSession, PlaybackSessionResolver, UreqTransport,
    build_playback_track_plan,
};
use jfn_mpv::api::{
    JfnMpvLoadOptions, jfn_mpv_apply_pending_track_selection_and_play, jfn_mpv_free_string,
    jfn_mpv_get_property_string, jfn_mpv_load_file, jfn_mpv_set_muted, jfn_mpv_stop,
    wait_event_owned,
};
use jfn_mpv::boot::{DisplayBackend, JfnMpvBoot, jfn_mpv_handle_init, jfn_mpv_handle_terminate};
use jfn_mpv::{EndFileReason, Event, PropertyValue, sys};
use serde_json::json;
use std::env;
use std::ffi::{CStr, CString};
use std::io::Write as _;
use std::path::Path;
use std::ptr;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;
use url::Url;

const VIDEO_FRAME_INFO_OBSERVER: u64 = 0x004d_5346_5241_4d45;

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("required environment variable {name} is missing"))
}

struct MpvGuard;

impl Drop for MpvGuard {
    fn drop(&mut self) {
        jfn_mpv_stop();
        jfn_mpv_handle_terminate();
    }
}

struct LiveExternalSubtitle {
    _file: NamedTempFile,
    path: CString,
    byte_count: usize,
    redirect_count: usize,
    target_host: String,
}

#[test]
#[ignore = "opens a native window and requires an explicitly configured live account"]
fn decodes_first_live_video_frame_through_native_load_path() {
    let media_id = required_env("MEDIASTATION_LIVE_MEDIA_ID");
    let user_agent = "MediaStationWindowsLivePlayback/0.1";
    let session = MediaStationSession::new(
        Url::parse(&required_env("MEDIASTATION_LIVE_BASE_URL"))
            .expect("live base URL should parse"),
        required_env("MEDIASTATION_LIVE_USER_ID"),
        required_env("MEDIASTATION_LIVE_TOKEN"),
        required_env("MEDIASTATION_LIVE_AUTHORIZATION"),
    )
    .expect("live session should be valid");
    let api = MediaStationApiClient::new(user_agent).expect("API client should initialize");
    let source = api
        .load_playback_source(&session, &media_id)
        .expect("live PlaybackInfo should load");
    let preference = api
        .load_playback_preference(&session, &media_id)
        .expect("live PlaybackPreferences should load");
    let track_plan = build_playback_track_plan(&source, &preference);
    let external_subtitle = track_plan.external_subtitle_url.as_ref().map(|url| {
        let key = track_plan
            .subtitle_track_key
            .as_deref()
            .expect("external subtitle should have a stable key");
        let track = source
            .subtitles
            .iter()
            .find(|track| track.key == key && track.url.as_ref() == Some(url))
            .expect("selected external subtitle should exist");
        let download = api
            .download_external_subtitle(&session, url, 16 * 1024 * 1024)
            .expect("external subtitle should download through the native API");
        let suffix = live_subtitle_suffix(track.codec.as_deref(), url)
            .expect("external subtitle format should be supported");
        let mut file = tempfile::Builder::new()
            .prefix("mediastation-live-subtitle-")
            .suffix(suffix)
            .tempfile()
            .expect("external subtitle temporary file should be created");
        file.write_all(&download.bytes)
            .and_then(|_| file.flush())
            .expect("external subtitle temporary file should be written");
        let path = CString::new(
            file.path()
                .to_str()
                .expect("external subtitle path should be valid UTF-8"),
        )
        .expect("external subtitle path should not contain NUL");
        LiveExternalSubtitle {
            _file: file,
            path,
            byte_count: download.bytes.len(),
            redirect_count: download.redirect_count,
            target_host: download.target_host,
        }
    });
    let input = session
        .playback_resolve_input(&media_id, source.url.clone(), user_agent)
        .expect("live resolve input should be valid");
    let playback = PlaybackSessionResolver::new(UreqTransport::new())
        .resolve(&input)
        .expect("live playback URL should resolve and pass the Range probe");

    let boot = JfnMpvBoot {
        display_backend: DisplayBackend::Other as u8,
        hwdec: c"auto-safe".as_ptr(),
        user_agent: c"MediaStationWindowsLivePlayback/0.1".as_ptr(),
        audio_passthrough: ptr::null(),
        audio_exclusive: false,
        audio_channels: ptr::null(),
        geometry: c"960x540".as_ptr(),
        force_window_position: false,
        window_maximized_at_boot: false,
        mpv_log_level: c"warn".as_ptr(),
        client_side_decorations: false,
    };
    let raw = unsafe { jfn_mpv_handle_init(&boot) };
    assert!(!raw.is_null(), "libmpv should initialize");
    let _mpv_guard = MpvGuard;
    jfn_mpv_set_muted(true);

    let observe_result = unsafe {
        sys::mpv_observe_property(
            raw,
            VIDEO_FRAME_INFO_OBSERVER,
            c"video-frame-info".as_ptr(),
            sys::mpv_format::MPV_FORMAT_NODE,
        )
    };
    assert!(
        observe_result >= 0,
        "video-frame-info observation should start"
    );

    let playback_url = CString::new(playback.resolved_url.as_str())
        .expect("resolved playback URL should not contain NUL");
    let header_fields = CString::new(
        playback
            .request_headers
            .to_mpv_http_header_fields()
            .expect("playback headers should encode"),
    )
    .expect("playback headers should not contain NUL");
    let options = JfnMpvLoadOptions {
        start_secs: 0.0,
        video_track: track_plan.video_track,
        audio_track: track_plan.audio_track,
        sub_track: track_plan.subtitle_track,
        external_audio_url: c"".as_ptr(),
        external_sub_url: external_subtitle
            .as_ref()
            .map_or(c"".as_ptr(), |subtitle| subtitle.path.as_ptr()),
        http_header_fields: header_fields.as_ptr(),
        video_filter: c"".as_ptr(),
        hwdec: c"".as_ptr(),
        is_infinite_stream: false,
    };
    unsafe { jfn_mpv_load_file(playback_url.as_ptr(), &options) }
        .expect("native load request should queue");

    let deadline = Instant::now() + Duration::from_secs(45);
    let mut file_loaded = false;
    let mut first_frame = false;
    while Instant::now() < deadline && !first_frame {
        match wait_event_owned(0.5) {
            jfn_mpv::api::WaitEvent::Event(Event::FileLoaded) => {
                file_loaded = true;
                jfn_mpv_apply_pending_track_selection_and_play();
            }
            jfn_mpv::api::WaitEvent::Event(Event::PropertyChange {
                id: VIDEO_FRAME_INFO_OBSERVER,
                value,
                ..
            }) if !matches!(value, PropertyValue::None) => first_frame = true,
            jfn_mpv::api::WaitEvent::Event(Event::EndFile(reason)) => match reason {
                EndFileReason::Error(error) => panic!("native playback ended with error: {error}"),
                other => panic!("native playback ended before first frame: {other:?}"),
            },
            _ => {}
        }
    }
    assert!(
        file_loaded,
        "libmpv should report FileLoaded within 45 seconds"
    );
    assert!(
        first_frame,
        "libmpv should expose a decoded video frame within 45 seconds"
    );

    let external_subtitle_loaded = if external_subtitle.is_some() {
        let subtitle_deadline = Instant::now() + Duration::from_secs(5);
        let mut selected = false;
        while Instant::now() < subtitle_deadline && !selected {
            selected = mpv_property(c"sid")
                .is_some_and(|value| value != "0" && !value.eq_ignore_ascii_case("no"));
            if !selected {
                let _ = wait_event_owned(0.1);
            }
        }
        assert!(
            selected,
            "external subtitle should be selected within 5 seconds"
        );
        true
    } else {
        false
    };
    let reporting_verified = env::var("MEDIASTATION_LIVE_VERIFY_REPORTING").as_deref() == Ok("1");
    if reporting_verified {
        let restore_position_ms = required_env("MEDIASTATION_LIVE_RESTORE_POSITION_MS")
            .parse::<u64>()
            .expect("restore position should be an unsigned integer");
        api.report_playing(&session, &source, restore_position_ms, false)
            .expect("live Playing report should succeed");
        api.report_progress(&session, &source, restore_position_ms, false)
            .expect("live Progress report should succeed");
        api.report_stopped(&session, &source, restore_position_ms)
            .expect("live Stopped report should succeed");
    }

    let video = source.video.as_ref();
    println!(
        "LIVE_NATIVE_PLAYBACK_SUMMARY={}",
        json!({
            "sourceCodec": video.and_then(|value| value.codec.as_deref()),
            "sourceProfile": video.and_then(|value| value.profile.as_deref()),
            "sourceDynamicRange": video.and_then(|value| value.dynamic_range.as_deref()),
            "sourceBitDepth": video.and_then(|value| value.bit_depth),
            "sourceColorSpace": video.and_then(|value| value.color_space.as_deref()),
            "sourceColorTransfer": video.and_then(|value| value.color_transfer.as_deref()),
            "decodedCodec": mpv_property(c"video-codec"),
            "hardwareDecoder": mpv_property(c"hwdec-current"),
            "decodedPrimaries": mpv_property(c"video-params/primaries"),
            "decodedTransfer": mpv_property(c"video-params/gamma"),
            "decodedColorMatrix": mpv_property(c"video-params/colormatrix"),
            "externalSubtitleLoaded": external_subtitle_loaded,
            "externalSubtitleBytes": external_subtitle.as_ref().map(|value| value.byte_count),
            "externalSubtitleRedirectCount": external_subtitle.as_ref().map(|value| value.redirect_count),
            "externalSubtitleTargetHost": external_subtitle.as_ref().map(|value| value.target_host.as_str()),
            "reportingVerified": reporting_verified,
            "serverCredentialQueryRemoved": source.server_credential_query_removed,
            "targetHost": playback.metrics.target_host,
            "firstFrame": true,
        })
    );
}

fn live_subtitle_suffix(codec: Option<&str>, url: &Url) -> Option<&'static str> {
    Path::new(url.path())
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(live_subtitle_suffix_from_value)
        .or_else(|| codec.and_then(live_subtitle_suffix_from_value))
}

fn live_subtitle_suffix_from_value(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "srt" | "subrip" | "mov_text" | "text" => Some(".srt"),
        "ass" => Some(".ass"),
        "ssa" => Some(".ssa"),
        "vtt" | "webvtt" => Some(".vtt"),
        "ttml" => Some(".ttml"),
        "dfxp" => Some(".dfxp"),
        "sub" | "microdvd" | "dvdsub" | "vobsub" => Some(".sub"),
        "sup" | "pgs" | "pgssub" | "hdmv_pgs_subtitle" => Some(".sup"),
        "smi" | "sami" => Some(".smi"),
        "lrc" => Some(".lrc"),
        _ => None,
    }
}

fn mpv_property(name: &CStr) -> Option<String> {
    let value = unsafe { jfn_mpv_get_property_string(name.as_ptr()) };
    if value.is_null() {
        return None;
    }
    let output = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();
    unsafe { jfn_mpv_free_string(value) };
    (!output.is_empty()).then_some(output)
}
