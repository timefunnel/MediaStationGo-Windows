use jfn_mediastation::{
    DeliveryMode, MediaStationApiClient, MediaStationSession, PlaybackSessionResolver,
    UreqTransport, build_playback_track_plan,
};
use serde_json::json;
use std::env;
use url::Url;

#[allow(
    clippy::panic,
    reason = "an explicitly requested live test must fail when required configuration is missing"
)]
fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("required environment variable {name} is missing"))
}

#[test]
#[ignore = "requires an explicitly configured live MediaStationGo account"]
fn authenticates_live_account_without_printing_credentials() {
    let base_url = Url::parse(&required_env("MEDIASTATION_LIVE_BASE_URL"))
        .expect("live base URL should parse");
    let username = required_env("MEDIASTATION_LIVE_USERNAME");
    let password = required_env("MEDIASTATION_LIVE_PASSWORD");
    let client = MediaStationApiClient::new("MediaStationWindowsLiveAuth/0.1")
        .expect("API client should initialize");
    let authorization = "MediaBrowser Client=\"MediaStation Windows\", Device=\"Windows\", DeviceId=\"mediastation-live-auth\", Version=\"0.1\"";

    let authenticated = client
        .authenticate(&base_url, &username, &password, authorization)
        .expect("live account should authenticate");
    let debug = format!("{authenticated:?}");

    assert!(!authenticated.user_id.is_empty());
    assert!(!authenticated.user_name.is_empty());
    assert!(!authenticated.access_token_secret().is_empty());
    assert!(!debug.contains(authenticated.access_token_secret()));
    println!(
        "LIVE_AUTH_SUMMARY={}",
        json!({
            "authenticated": true,
            "userIdPresent": true,
            "userNamePresent": true,
            "tokenPresentInDebug": false,
        })
    );
}

#[test]
#[ignore = "requires an explicitly configured live MediaStationGo account"]
fn loads_live_catalog_detail_and_image_without_exposing_session() {
    let base_url = Url::parse(&required_env("MEDIASTATION_LIVE_BASE_URL"))
        .expect("live base URL should parse");
    let username = required_env("MEDIASTATION_LIVE_USERNAME");
    let password = required_env("MEDIASTATION_LIVE_PASSWORD");
    let user_agent = "MediaStationWindowsLiveCatalog/0.1";
    let authorization = "MediaBrowser Client=\"MediaStation Windows\", Device=\"Windows\", DeviceId=\"mediastation-live-catalog\", Version=\"0.1\"";
    let client = MediaStationApiClient::new(user_agent).expect("API client should initialize");

    let authenticated = client
        .authenticate(&base_url, &username, &password, authorization)
        .expect("live account should authenticate");
    let session = MediaStationSession::new(
        authenticated.base_url.clone(),
        authenticated.user_id.clone(),
        authenticated.access_token_secret(),
        authorization,
    )
    .expect("authenticated session should be valid");
    let home = client
        .load_home(&session)
        .expect("live home catalog should load");
    let candidate = home
        .resume
        .iter()
        .chain(home.latest.iter())
        .next()
        .expect("live resume or latest catalog should contain an item");
    let detail = client
        .load_media_detail(&session, &candidate.id)
        .expect("live media detail should load");
    let image_ref = detail
        .item
        .primary_image
        .as_ref()
        .or(detail.item.landscape_image.as_ref())
        .or(detail.item.backdrop_image.as_ref())
        .or(candidate.primary_image.as_ref())
        .or(candidate.landscape_image.as_ref())
        .or(candidate.backdrop_image.as_ref())
        .expect("live media item should expose an image descriptor");
    let image = client
        .download_media_image(&session, image_ref, 640, 4 * 1024 * 1024)
        .expect("live media image should download");

    assert!(!home.libraries.is_empty());
    assert!(!detail.item.media_type.is_empty());
    assert!(!image.bytes.is_empty());
    println!(
        "LIVE_CATALOG_SUMMARY={}",
        json!({
            "libraryCount": home.libraries.len(),
            "resumeCount": home.resume.len(),
            "latestCount": home.latest.len(),
            "detailMediaType": detail.item.media_type,
            "episodeCount": detail.episodes.len(),
            "imageBytes": image.bytes.len(),
        })
    );
}

#[test]
#[ignore = "requires an explicitly configured live MediaStationGo account"]
fn resolves_live_playback_session_without_exposing_credentials() {
    let media_id = required_env("MEDIASTATION_LIVE_MEDIA_ID");
    let user_agent = "MediaStationWindowsLiveProbe/0.1";
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
    let input = session
        .playback_resolve_input(&media_id, source.url.clone(), user_agent)
        .expect("live resolve input should be valid");
    let playback = PlaybackSessionResolver::new(UreqTransport::new())
        .resolve(&input)
        .expect("live playback URL should resolve and pass the Range probe");

    assert!((200..300).contains(&playback.metrics.status_code));
    if playback.metrics.delivery_mode == DeliveryMode::DirectCdn {
        assert!(playback.metrics.accepts_ranges);
        assert!(playback.request_headers.get("x-emby-token").is_none());
        assert!(
            playback
                .request_headers
                .get("x-emby-authorization")
                .is_none()
        );
    }

    println!(
        "LIVE_PROBE_SUMMARY={}",
        json!({
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
            "audioTrackKey": track_plan.audio_track_key,
            "subtitleTrackKey": track_plan.subtitle_track_key,
            "preferenceCorrectionNeeded": track_plan.preference_correction.is_some(),
        })
    );
}
