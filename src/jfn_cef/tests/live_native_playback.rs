use jfn_mediastation::{
    MediaCard, MediaStationApiClient, MediaStationClientProfile, MediaStationConnectionProfile,
    MediaStationProxyMode, MediaStationSession, PlaybackSessionResolver, UreqTransport,
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

#[allow(
    clippy::panic,
    reason = "an explicitly requested live test must fail when required configuration is missing"
)]
fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("required environment variable {name} is missing"))
}

fn live_connection_profile() -> MediaStationConnectionProfile {
    let client = match env::var("MEDIASTATION_LIVE_CLIENT_PROFILE").as_deref() {
        Ok("senplayer") => MediaStationClientProfile::SenPlayer,
        Ok("infuse") => MediaStationClientProfile::Infuse,
        Ok("mediastation_go") | Err(_) => MediaStationClientProfile::MediaStationGo,
        Ok(_) => {
            panic!("MEDIASTATION_LIVE_CLIENT_PROFILE must be mediastation_go, senplayer, or infuse")
        }
    };
    let proxy = match env::var("MEDIASTATION_LIVE_PROXY_MODE").as_deref() {
        Ok("system") => MediaStationProxyMode::System,
        Ok("direct") | Err(_) => MediaStationProxyMode::Direct,
        Ok(_) => panic!("MEDIASTATION_LIVE_PROXY_MODE must be direct or system"),
    };
    let profile = MediaStationConnectionProfile { client, proxy };
    assert!(profile.is_supported());
    profile
}

fn live_authorization(
    profile: MediaStationConnectionProfile,
    device_id: &str,
    token: Option<&str>,
) -> String {
    let client = profile.client.authorization_client();
    let version = profile.client.authorization_version("0.1");
    let mut authorization = format!(
        "MediaBrowser Client=\"{client}\", Device=\"Windows\", DeviceId=\"{device_id}\", Version=\"{version}\""
    );
    if let Some(token) = token {
        authorization.push_str(", Token=\"");
        authorization.push_str(token);
        authorization.push('"');
    }
    authorization
}

fn live_session(
    client: &MediaStationApiClient,
    profile: MediaStationConnectionProfile,
) -> MediaStationSession {
    let base_url = Url::parse(&required_env("MEDIASTATION_LIVE_BASE_URL"))
        .expect("live base URL should parse");
    if let (Ok(user_id), Ok(token), Ok(authorization)) = (
        env::var("MEDIASTATION_LIVE_USER_ID"),
        env::var("MEDIASTATION_LIVE_TOKEN"),
        env::var("MEDIASTATION_LIVE_AUTHORIZATION"),
    ) {
        return MediaStationSession::new_with_profile(
            base_url,
            user_id,
            token,
            authorization,
            profile,
        )
        .expect("live session should be valid");
    }
    let username = required_env("MEDIASTATION_LIVE_USERNAME");
    let password = required_env("MEDIASTATION_LIVE_PASSWORD");
    let device_id = "mediastation-live-native";
    let base_authorization = live_authorization(profile, device_id, None);
    let authenticated = client
        .authenticate_with_profile(
            &base_url,
            &username,
            &password,
            &base_authorization,
            profile,
        )
        .expect("live account should authenticate");
    let token = authenticated.access_token_secret().to_string();
    MediaStationSession::new_with_profile(
        authenticated.base_url,
        authenticated.user_id,
        token.clone(),
        live_authorization(profile, device_id, Some(&token)),
        profile,
    )
    .expect("authenticated live session should be valid")
}

fn live_media_id(api: &MediaStationApiClient, session: &MediaStationSession) -> String {
    if let Ok(media_id) = env::var("MEDIASTATION_LIVE_MEDIA_ID") {
        return media_id;
    }
    let home = api
        .load_home(session)
        .expect("live home catalog should load");
    if let Some(media_id) = home
        .resume
        .iter()
        .chain(home.latest.iter())
        .chain(
            home.latest_by_library
                .iter()
                .flat_map(|section| section.items.iter()),
        )
        .find(|card: &&MediaCard| card.is_playable())
        .map(|card| card.id.clone())
    {
        return media_id;
    }
    let series = home
        .resume
        .iter()
        .chain(home.latest.iter())
        .chain(
            home.latest_by_library
                .iter()
                .flat_map(|section| section.items.iter()),
        )
        .find(|card| card.media_type == "Series")
        .expect("live catalog should contain a playable media item or series");
    api.load_media_detail(session, &series.id)
        .expect("live series detail should load")
        .episodes
        .into_iter()
        .find(MediaCard::is_playable)
        .map(|card| card.id)
        .expect("live series detail should contain a playable episode")
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
    let user_agent = "MediaStationWindowsLivePlayback/0.1";
    let profile = live_connection_profile();
    let api = MediaStationApiClient::new(user_agent).expect("API client should initialize");
    let session = live_session(&api, profile);
    let media_id = live_media_id(&api, &session);
    let source = api
        .load_playback_source(&session, &media_id)
        .expect("live PlaybackInfo should load");
    let preference = api
        .load_playback_preference(&session, &media_id, &source)
        .expect("live PlaybackPreferences should load");
    let track_plan = build_playback_track_plan(&source, &preference.preference);
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
        .playback_resolve_input(
            &media_id,
            source.url.clone(),
            profile.client.user_agent(user_agent),
        )
        .expect("live resolve input should be valid");
    let playback = PlaybackSessionResolver::new(UreqTransport::new())
        .resolve(&input)
        .expect("live playback URL should resolve and pass the Range probe");
    let playback_proxy = api
        .playback_proxy_url(&session)
        .expect("live playback proxy should be available")
        .map(|value| CString::new(value).expect("live playback proxy should not contain NUL"));

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
        http_proxy: playback_proxy
            .as_ref()
            .map_or(c"".as_ptr(), |value| value.as_ptr()),
        video_filter: c"".as_ptr(),
        hwdec: c"".as_ptr(),
        subtitle_style_override: false,
        subtitle_font_size: 0.0,
        subtitle_position: 100.0,
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

    let subtitle_track_selected = if track_plan.subtitle_track != 0 {
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
            "configured subtitle should be selected within 5 seconds"
        );
        true
    } else {
        false
    };
    let external_subtitle_loaded = external_subtitle.is_some() && subtitle_track_selected;
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
            "subtitleTrackSelected": subtitle_track_selected,
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
