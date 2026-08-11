use jfn_mediastation::{
    DeliveryMode, MediaCard, MediaHome, MediaStationApiClient, MediaStationClientProfile,
    MediaStationConnectionProfile, MediaStationProxyMode, MediaStationSession,
    PlaybackSessionResolver, UreqTransport, build_playback_track_plan,
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

fn live_connection_profile() -> MediaStationConnectionProfile {
    let client = match env::var("MEDIASTATION_LIVE_CLIENT_PROFILE").as_deref() {
        Ok("senplayer") => MediaStationClientProfile::SenPlayer,
        Ok("infuse") => MediaStationClientProfile::Infuse,
        Ok("mediastation_go" | "mediastation_windows") | Err(_) => {
            MediaStationClientProfile::MediaStationWindows
        }
        Ok(_) => {
            panic!(
                "MEDIASTATION_LIVE_CLIENT_PROFILE must be mediastation_windows, senplayer, or infuse"
            )
        }
    };
    let proxy = match env::var("MEDIASTATION_LIVE_PROXY_MODE").as_deref() {
        Ok("system") => MediaStationProxyMode::System,
        Ok("direct") | Err(_) => MediaStationProxyMode::Direct,
        Ok(_) => panic!("MEDIASTATION_LIVE_PROXY_MODE must be direct or system"),
    };
    let profile = MediaStationConnectionProfile { client, proxy };
    assert!(
        profile.is_supported(),
        "live client and proxy mode combination is not supported"
    );
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

fn live_authenticated_session(
    client: &MediaStationApiClient,
    profile: MediaStationConnectionProfile,
    device_id: &str,
) -> MediaStationSession {
    let base_url = Url::parse(&required_env("MEDIASTATION_LIVE_BASE_URL"))
        .expect("live base URL should parse");
    let username = required_env("MEDIASTATION_LIVE_USERNAME");
    let password = required_env("MEDIASTATION_LIVE_PASSWORD");
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
    let authorization = live_authorization(
        profile,
        device_id,
        Some(authenticated.access_token_secret()),
    );
    let token = authenticated.access_token_secret().to_string();
    let mut session = MediaStationSession::new_with_profile(
        authenticated.base_url,
        authenticated.user_id,
        token,
        authorization,
        profile,
    )
    .expect("authenticated session should be valid");
    let extensions = client
        .load_protocol_extensions(&session)
        .expect("live server capabilities should load");
    session.set_protocol_extensions(extensions);
    session
}

fn live_detail_card(home: &MediaHome) -> MediaCard {
    home.resume
        .iter()
        .chain(home.latest.iter())
        .chain(
            home.latest_by_library
                .iter()
                .flat_map(|section| section.items.iter()),
        )
        .find(|card| {
            (card.is_playable() || card.media_type == "Series")
                && (card.primary_image.is_some()
                    || card.landscape_image.is_some()
                    || card.backdrop_image.is_some())
        })
        .or_else(|| {
            home.resume
                .iter()
                .chain(home.latest.iter())
                .chain(
                    home.latest_by_library
                        .iter()
                        .flat_map(|section| section.items.iter()),
                )
                .find(|card| card.is_playable() || card.media_type == "Series")
        })
        .cloned()
        .expect("live catalog should contain a playable item or series")
}

#[test]
#[ignore = "requires an explicitly configured live MediaStationGo or standard Emby account"]
fn exercises_live_catalog_image_subtitle_and_playback_contract() {
    let profile = live_connection_profile();
    let client = MediaStationApiClient::new("MediaStationWindowsLiveContract/0.1")
        .expect("API client should initialize");
    let session = live_authenticated_session(&client, profile, "mediastation-live-contract");
    let home = client
        .load_home(&session)
        .expect("live home catalog should load");
    assert!(
        !home.libraries.is_empty(),
        "live catalog should expose libraries"
    );

    let candidate = live_detail_card(&home);
    let search_term =
        env::var("MEDIASTATION_LIVE_SEARCH_TERM").unwrap_or_else(|_| candidate.title.clone());
    let search_results = client
        .search_media(&session, &search_term, 12)
        .expect("live search should load");
    let detail = client
        .load_media_detail(&session, &candidate.id)
        .expect("live media detail should load");
    let playback_card = if detail.item.is_playable() {
        detail.item.clone()
    } else {
        detail
            .episodes
            .iter()
            .find(|card| card.is_playable())
            .cloned()
            .expect("live series detail should contain a playable episode")
    };
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

    let source = client
        .load_playback_source(&session, &playback_card.id)
        .expect("live PlaybackInfo should load");
    let preference = client
        .load_playback_preference(&session, &playback_card.id, &source)
        .expect(
            "live playback preference probe should load or use the standard Emby session fallback",
        );
    let track_plan = build_playback_track_plan(&source, &preference.preference);
    let external_subtitle = track_plan.external_subtitle_url.as_ref().map(|url| {
        client
            .download_external_subtitle(&session, url, 16 * 1024 * 1024)
            .expect("live external subtitle should download")
    });
    let input = session
        .playback_resolve_input(
            &playback_card.id,
            source.url.clone(),
            profile
                .client
                .user_agent("MediaStationWindowsLiveContract/0.1"),
        )
        .expect("live playback resolve input should be valid");
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
        "LIVE_CONTRACT_SUMMARY={}",
        json!({
            "clientProfile": profile.client.as_str(),
            "proxyMode": profile.proxy.as_str(),
            "libraryCount": home.libraries.len(),
            "resumeCount": home.resume.len(),
            "latestCount": home.latest.len(),
            "searchSucceeded": true,
            "searchResultCount": search_results.len(),
            "detailMediaType": detail.item.media_type,
            "playbackMediaType": playback_card.media_type,
            "episodeCount": detail.episodes.len(),
            "imageBytes": image.bytes.len(),
            "subtitleTrackCount": source.subtitles.len(),
            "externalSubtitleDownloaded": external_subtitle.is_some(),
            "externalSubtitleBytes": external_subtitle.as_ref().map(|value| value.bytes.len()),
            "standardEmbyStream": source.standard_emby_stream,
            "deliveryMode": match playback.metrics.delivery_mode {
                DeliveryMode::Server => "server",
                DeliveryMode::DirectCdn => "direct_cdn",
            },
            "probeStatus": playback.metrics.status_code,
            "acceptsRanges": playback.metrics.accepts_ranges,
            "targetHost": playback.metrics.target_host,
            "serverCredentialQueryRemoved": source.server_credential_query_removed,
            "audioTrackKeyPresent": track_plan.audio_track_key.is_some(),
            "subtitleTrackKeyPresent": track_plan.subtitle_track_key.is_some(),
            "preferencePersistence": preference.persistence.as_str(),
        })
    );
}

#[test]
#[ignore = "requires an explicitly configured live MediaStationGo or standard Emby account"]
fn authenticates_live_account_without_printing_credentials() {
    let base_url = Url::parse(&required_env("MEDIASTATION_LIVE_BASE_URL"))
        .expect("live base URL should parse");
    let username = required_env("MEDIASTATION_LIVE_USERNAME");
    let password = required_env("MEDIASTATION_LIVE_PASSWORD");
    let client = MediaStationApiClient::new("MediaStationWindowsLiveAuth/0.1")
        .expect("API client should initialize");
    let profile = live_connection_profile();
    let authorization = live_authorization(profile, "mediastation-live-auth", None);

    let authenticated = client
        .authenticate_with_profile(&base_url, &username, &password, &authorization, profile)
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
#[ignore = "requires an explicitly configured live MediaStationGo or standard Emby account"]
fn loads_live_catalog_detail_and_image_without_exposing_session() {
    let base_url = Url::parse(&required_env("MEDIASTATION_LIVE_BASE_URL"))
        .expect("live base URL should parse");
    let username = required_env("MEDIASTATION_LIVE_USERNAME");
    let password = required_env("MEDIASTATION_LIVE_PASSWORD");
    let user_agent = "MediaStationWindowsLiveCatalog/0.1";
    let profile = live_connection_profile();
    let base_authorization = live_authorization(profile, "mediastation-live-catalog", None);
    let client = MediaStationApiClient::new(user_agent).expect("API client should initialize");

    let authenticated = client
        .authenticate_with_profile(
            &base_url,
            &username,
            &password,
            &base_authorization,
            profile,
        )
        .expect("live account should authenticate");
    let authorization = live_authorization(
        profile,
        "mediastation-live-catalog",
        Some(authenticated.access_token_secret()),
    );
    let session = MediaStationSession::new_with_profile(
        authenticated.base_url.clone(),
        authenticated.user_id.clone(),
        authenticated.access_token_secret(),
        authorization,
        profile,
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
#[ignore = "requires an explicitly configured live MediaStationGo or standard Emby account"]
fn resolves_live_playback_session_without_exposing_credentials() {
    let media_id = required_env("MEDIASTATION_LIVE_MEDIA_ID");
    let user_agent = "MediaStationWindowsLiveProbe/0.1";
    let profile = live_connection_profile();
    let session = MediaStationSession::new_with_profile(
        Url::parse(&required_env("MEDIASTATION_LIVE_BASE_URL"))
            .expect("live base URL should parse"),
        required_env("MEDIASTATION_LIVE_USER_ID"),
        required_env("MEDIASTATION_LIVE_TOKEN"),
        required_env("MEDIASTATION_LIVE_AUTHORIZATION"),
        profile,
    )
    .expect("live session should be valid");
    let api = MediaStationApiClient::new(user_agent).expect("API client should initialize");

    let source = api
        .load_playback_source(&session, &media_id)
        .expect("live PlaybackInfo should load");
    let preference = api
        .load_playback_preference(&session, &media_id, &source)
        .expect("live PlaybackPreferences should load");
    let track_plan = build_playback_track_plan(&source, &preference.preference);
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
            "preferencePersistence": preference.persistence.as_str(),
        })
    );
}
