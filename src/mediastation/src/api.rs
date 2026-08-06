use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

use crate::playback_session::{PlaybackSessionError, ResolveInput, ServerAuth};

const HEADER_EMBY_AUTHORIZATION: &str = "x-emby-authorization";
const HEADER_EMBY_TOKEN: &str = "x-emby-token";
const HEADER_CONTENT_LENGTH: &str = "content-length";
const HEADER_CONTENT_TYPE: &str = "content-type";
const HEADER_LOCATION: &str = "location";
const HEADER_USER_AGENT: &str = "user-agent";
const MAX_EXTERNAL_RESOURCE_REDIRECTS: usize = 6;
const CATALOG_FIELDS: &str = "Overview,RunTimeTicks,UserData,ImageTags,BackdropImageTags,ParentBackdropImageTags,ParentBackdropItemId,ParentLogoImageTag,ParentLogoItemId,PrimaryImageItemId,ProductionYear,CommunityRating,OfficialRating,Genres,People,MediaSources,SeriesId,SeriesName,SeasonId,ParentId,IndexNumber,ParentIndexNumber";
const MAX_CATALOG_PAGE_SIZE: usize = 100;
const MAX_DETAIL_EPISODES: usize = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaStationClientProfile {
    MediaStationGo,
    SenPlayer,
    Infuse,
}

impl MediaStationClientProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MediaStationGo => "mediastation_go",
            Self::SenPlayer => "senplayer",
            Self::Infuse => "infuse",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "mediastation_go" => Some(Self::MediaStationGo),
            "senplayer" => Some(Self::SenPlayer),
            "infuse" => Some(Self::Infuse),
            _ => None,
        }
    }

    pub const fn server_type(self) -> &'static str {
        match self {
            Self::MediaStationGo => "mediastation_go",
            Self::SenPlayer | Self::Infuse => "standard_emby",
        }
    }

    pub const fn authorization_client(self) -> &'static str {
        match self {
            Self::MediaStationGo => "MediaStation Windows",
            Self::SenPlayer => "SenPlayer",
            Self::Infuse => "Infuse",
        }
    }

    pub const fn authorization_version(self, media_station_version: &'static str) -> &'static str {
        match self {
            Self::MediaStationGo => media_station_version,
            Self::SenPlayer | Self::Infuse => "1.0.0",
        }
    }

    pub fn user_agent<'a>(self, media_station_user_agent: &'a str) -> &'a str {
        match self {
            Self::MediaStationGo => media_station_user_agent,
            Self::SenPlayer => "SenPlayer/1.0.0",
            Self::Infuse => "Infuse/1.0.0",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaStationProxyMode {
    Direct,
    System,
}

impl MediaStationProxyMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::System => "system",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "direct" => Some(Self::Direct),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaStationConnectionProfile {
    pub client: MediaStationClientProfile,
    pub proxy: MediaStationProxyMode,
}

impl MediaStationConnectionProfile {
    pub const fn media_station_go() -> Self {
        Self {
            client: MediaStationClientProfile::MediaStationGo,
            proxy: MediaStationProxyMode::Direct,
        }
    }

    pub const fn standard_emby(client: MediaStationClientProfile) -> Option<Self> {
        match client {
            MediaStationClientProfile::MediaStationGo => None,
            MediaStationClientProfile::SenPlayer | MediaStationClientProfile::Infuse => {
                Some(Self {
                    client,
                    proxy: MediaStationProxyMode::Direct,
                })
            }
        }
    }

    pub const fn is_supported(self) -> bool {
        matches!(
            self.client,
            MediaStationClientProfile::MediaStationGo
                | MediaStationClientProfile::SenPlayer
                | MediaStationClientProfile::Infuse
        ) && matches!(
            self.proxy,
            MediaStationProxyMode::Direct | MediaStationProxyMode::System
        )
    }
}

impl Default for MediaStationConnectionProfile {
    fn default() -> Self {
        Self::media_station_go()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct MediaStationSession {
    pub base_url: Url,
    pub user_id: String,
    pub connection: MediaStationConnectionProfile,
    token: String,
    authorization: String,
}

impl MediaStationSession {
    pub fn new(
        base_url: Url,
        user_id: impl Into<String>,
        token: impl Into<String>,
        authorization: impl Into<String>,
    ) -> Result<Self, ApiError> {
        Self::new_with_profile(
            base_url,
            user_id,
            token,
            authorization,
            MediaStationConnectionProfile::media_station_go(),
        )
    }

    pub fn new_with_profile(
        base_url: Url,
        user_id: impl Into<String>,
        token: impl Into<String>,
        authorization: impl Into<String>,
        connection: MediaStationConnectionProfile,
    ) -> Result<Self, ApiError> {
        if !connection.is_supported() {
            return Err(ApiError::InvalidInput {
                field: "connection_profile",
                reason: "client and proxy mode combination is not supported".to_string(),
            });
        }
        validate_http_url("base_url", &base_url)?;
        if base_url.query().is_some() || base_url.fragment().is_some() {
            return Err(ApiError::InvalidInput {
                field: "base_url",
                reason: "query and fragment are not allowed".to_string(),
            });
        }
        let user_id = user_id.into();
        let token = token.into();
        let authorization = authorization.into();
        validate_non_empty("user_id", &user_id)?;
        validate_non_empty("token", &token)?;
        validate_non_empty("authorization", &authorization)?;
        Ok(Self {
            base_url,
            user_id,
            connection,
            token,
            authorization,
        })
    }

    pub fn playback_resolve_input(
        &self,
        media_id: impl Into<String>,
        source_url: Url,
        user_agent: impl Into<String>,
    ) -> Result<ResolveInput, PlaybackSessionError> {
        Ok(ResolveInput {
            user_id: self.user_id.clone(),
            media_id: media_id.into(),
            source_url,
            server_base_url: self.base_url.clone(),
            user_agent: user_agent.into(),
            auth: ServerAuth::new(self.token.clone(), self.authorization.clone())?,
            proxy_mode: self.connection.proxy,
        })
    }

    pub fn access_token_secret(&self) -> &str {
        &self.token
    }
}

impl fmt::Debug for MediaStationSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MediaStationSession")
            .field("base_url", &self.base_url)
            .field("user_id", &self.user_id)
            .field("connection", &self.connection)
            .field("token", &"<redacted>")
            .field("authorization", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct AuthenticationResult {
    pub base_url: Url,
    pub user_id: String,
    pub user_name: String,
    access_token: String,
}

impl AuthenticationResult {
    pub fn access_token_secret(&self) -> &str {
        &self.access_token
    }
}

impl fmt::Debug for AuthenticationResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticationResult")
            .field("base_url", &self.base_url)
            .field("user_id", &self.user_id)
            .field("user_name", &self.user_name)
            .field("access_token", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaImageType {
    Primary,
    Thumb,
    Backdrop,
    Logo,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaImageRef {
    pub item_id: String,
    pub image_type: MediaImageType,
    pub image_index: Option<u32>,
    pub tag: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaCard {
    pub id: String,
    pub title: String,
    pub media_type: String,
    pub overview: String,
    pub collection_type: Option<String>,
    pub year: Option<i64>,
    pub duration_ms: u64,
    pub resume_position_ms: u64,
    pub played: bool,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub parent_id: Option<String>,
    pub season_id: Option<String>,
    pub series_id: Option<String>,
    pub series_name: Option<String>,
    pub community_rating: Option<f64>,
    pub official_rating: Option<String>,
    pub genres: Vec<String>,
    pub video_codec: Option<String>,
    pub video_profile: Option<String>,
    pub video_width: Option<u64>,
    pub video_height: Option<u64>,
    pub dynamic_range: Option<String>,
    pub primary_image: Option<MediaImageRef>,
    pub landscape_image: Option<MediaImageRef>,
    pub backdrop_image: Option<MediaImageRef>,
    pub logo_image: Option<MediaImageRef>,
}

impl MediaCard {
    pub fn is_playable(&self) -> bool {
        matches!(
            self.media_type.as_str(),
            "Movie" | "Episode" | "Video" | "MusicVideo"
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaHome {
    pub libraries: Vec<MediaCard>,
    pub resume: Vec<MediaCard>,
    pub latest: Vec<MediaCard>,
    pub latest_by_library: Vec<MediaLibrarySection>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaLibrarySection {
    pub library: MediaCard,
    pub items: Vec<MediaCard>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaPage {
    pub items: Vec<MediaCard>,
    pub start_index: usize,
    pub total_record_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaDetail {
    pub item: MediaCard,
    pub episodes: Vec<MediaCard>,
    pub people: Vec<MediaPerson>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPerson {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub person_type: Option<String>,
    pub primary_image: Option<MediaImageRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaLibraryFilters {
    pub item_types: Vec<String>,
    pub genres: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaImage {
    pub bytes: Vec<u8>,
    pub content_type: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlaybackSource {
    pub media_id: String,
    pub url: Url,
    pub standard_emby_stream: bool,
    pub server_credential_query_removed: bool,
    pub container: Option<String>,
    pub bitrate: Option<u64>,
    pub media_source_id: Option<String>,
    pub play_session_id: Option<String>,
    pub default_audio_stream_index: Option<i64>,
    pub default_subtitle_stream_index: Option<i64>,
    pub video: Option<VideoStream>,
    pub subtitles: Vec<SubtitleTrack>,
    pub audio_tracks: Vec<AudioTrack>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoStream {
    pub dynamic_range: Option<String>,
    pub range_type: Option<String>,
    pub codec: Option<String>,
    pub profile: Option<String>,
    pub level: Option<i64>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub frame_rate: Option<f64>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_range: Option<String>,
    pub bit_depth: Option<u64>,
    pub max_content_light_level: Option<u64>,
    pub max_frame_average_light_level: Option<u64>,
    pub dolby_vision_profile: Option<i64>,
    pub dolby_vision_level: Option<i64>,
    pub dolby_vision_base_layer_present: Option<bool>,
    pub dolby_vision_compatibility_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleTrack {
    pub key: String,
    pub stream_index: Option<i64>,
    pub url: Option<Url>,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub label: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_external: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalSubtitleDownload {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub redirect_count: usize,
    pub target_host: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioTrack {
    pub key: String,
    pub stream_index: Option<i64>,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub label: Option<String>,
    pub channel_count: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackTrackPreference {
    pub configured: bool,
    pub subtitle_enabled: bool,
    pub subtitle_track_key: Option<String>,
    pub audio_track_key: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackTrackPreferenceUpdate {
    pub subtitle_enabled: Option<bool>,
    pub subtitle_track_key: Option<String>,
    pub audio_track_key: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackPreferencePersistence {
    ServerExtension,
    SessionOnly,
}

impl PlaybackPreferencePersistence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ServerExtension => "server_extension",
            Self::SessionOnly => "session_only",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackTrackPreferenceState {
    pub preference: PlaybackTrackPreference,
    pub persistence: PlaybackPreferencePersistence,
}

#[derive(Clone)]
pub struct MediaStationApiClient {
    direct_agent: ureq::Agent,
    system_proxy_agent: Arc<Mutex<Option<CachedSystemProxyAgent>>>,
    user_agent: String,
}

struct CachedSystemProxyAgent {
    proxy_url: String,
    agent: ureq::Agent,
}

impl MediaStationApiClient {
    pub fn new(user_agent: impl Into<String>) -> Result<Self, ApiError> {
        let user_agent = user_agent.into();
        validate_non_empty("user_agent", &user_agent)?;
        Ok(Self {
            direct_agent: build_api_agent(None),
            system_proxy_agent: Arc::new(Mutex::new(None)),
            user_agent,
        })
    }

    fn agent_for_profile(
        &self,
        profile: MediaStationConnectionProfile,
    ) -> Result<ureq::Agent, ApiError> {
        match profile.proxy {
            MediaStationProxyMode::Direct => Ok(self.direct_agent.clone()),
            MediaStationProxyMode::System => {
                let proxy = ureq::Proxy::try_from_env().ok_or(ApiError::SystemProxyUnavailable)?;
                let proxy_url = proxy.uri().to_string();
                let mut cached = self.system_proxy_agent.lock();
                if let Some(cached) = cached.as_ref()
                    && cached.proxy_url == proxy_url
                {
                    return Ok(cached.agent.clone());
                }
                let agent = build_api_agent(Some(proxy));
                *cached = Some(CachedSystemProxyAgent {
                    proxy_url,
                    agent: agent.clone(),
                });
                Ok(agent)
            }
        }
    }

    fn agent_for_session(&self, session: &MediaStationSession) -> Result<ureq::Agent, ApiError> {
        self.agent_for_profile(session.connection)
    }

    fn user_agent_for_profile(&self, profile: MediaStationConnectionProfile) -> &str {
        profile.client.user_agent(&self.user_agent)
    }

    pub fn playback_proxy_url(
        &self,
        session: &MediaStationSession,
    ) -> Result<Option<String>, ApiError> {
        match session.connection.proxy {
            MediaStationProxyMode::Direct => Ok(None),
            MediaStationProxyMode::System => {
                let proxy = ureq::Proxy::try_from_env().ok_or(ApiError::SystemProxyUnavailable)?;
                Ok(Some(proxy.uri().to_string()))
            }
        }
    }

    pub fn authenticate_with_profile(
        &self,
        base_url: &Url,
        username: &str,
        password: &str,
        authorization: &str,
        profile: MediaStationConnectionProfile,
    ) -> Result<AuthenticationResult, ApiError> {
        if !profile.is_supported() {
            return Err(ApiError::InvalidInput {
                field: "connection_profile",
                reason: "client and proxy mode combination is not supported".to_string(),
            });
        }
        validate_http_url("base_url", base_url)?;
        if base_url.query().is_some() || base_url.fragment().is_some() {
            return Err(ApiError::InvalidInput {
                field: "base_url",
                reason: "query and fragment are not allowed".to_string(),
            });
        }
        validate_non_empty("username", username)?;
        validate_non_empty("authorization", authorization)?;

        let url = endpoint(base_url, &["Users", "AuthenticateByName"])?;
        let payload = json!({
            "Username": username.trim(),
            "Pw": password,
        });
        let agent = self.agent_for_profile(profile)?;
        let response = agent
            .post(url.as_str())
            .header(HEADER_USER_AGENT, self.user_agent_for_profile(profile))
            .header(HEADER_EMBY_AUTHORIZATION, authorization)
            .header("content-type", "application/json; charset=utf-8")
            .send(payload.to_string())
            .map_err(|error| transport_error(&url, error))?;
        let payload = read_json_response(&url, response)?;
        let access_token = required_string(payload.get("AccessToken"), "AccessToken")?;
        let user = payload
            .get("User")
            .and_then(Value::as_object)
            .ok_or(ApiError::MissingField { field: "User" })?;
        let user_id = required_string(user.get("Id"), "User.Id")?;
        let user_name =
            optional_string(user.get("Name")).unwrap_or_else(|| username.trim().to_string());

        Ok(AuthenticationResult {
            base_url: base_url.clone(),
            user_id,
            user_name,
            access_token,
        })
    }

    pub fn authenticate(
        &self,
        base_url: &Url,
        username: &str,
        password: &str,
        authorization: &str,
    ) -> Result<AuthenticationResult, ApiError> {
        self.authenticate_with_profile(
            base_url,
            username,
            password,
            authorization,
            MediaStationConnectionProfile::media_station_go(),
        )
    }

    pub fn load_home(&self, session: &MediaStationSession) -> Result<MediaHome, ApiError> {
        let libraries_url = endpoint(&session.base_url, &["Users", &session.user_id, "Views"])?;
        let libraries = parse_media_cards(&self.get_json(session, &libraries_url)?)?;

        let mut resume_url = resume_endpoint(session)?;
        resume_url
            .query_pairs_mut()
            .append_pair("UserId", &session.user_id)
            .append_pair("Limit", "24")
            .append_pair("Fields", CATALOG_FIELDS);
        let resume = parse_media_cards(&self.get_json(session, &resume_url)?)?;

        let latest_by_library = libraries
            .iter()
            .map(|library| {
                self.load_latest_items(session, &library.id, 18)
                    .map(|items| MediaLibrarySection {
                        library: library.clone(),
                        items,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen_latest = std::collections::HashSet::new();
        let latest = latest_by_library
            .iter()
            .flat_map(|section| section.items.iter())
            .filter(|item| seen_latest.insert(item.id.clone()))
            .take(30)
            .cloned()
            .collect();

        Ok(MediaHome {
            libraries,
            resume,
            latest,
            latest_by_library,
        })
    }

    fn load_latest_items(
        &self,
        session: &MediaStationSession,
        parent_id: &str,
        limit: usize,
    ) -> Result<Vec<MediaCard>, ApiError> {
        if session.connection.client == MediaStationClientProfile::MediaStationGo {
            return self
                .load_library_page(session, parent_id, 0, limit)
                .map(|page| page.items);
        }
        validate_identifier("parent_id", parent_id)?;
        validate_page(0, limit)?;
        let mut url = endpoint(
            &session.base_url,
            &["Users", &session.user_id, "Items", "Latest"],
        )?;
        url.query_pairs_mut()
            .append_pair("ParentId", parent_id)
            .append_pair("Limit", &limit.to_string())
            .append_pair("Fields", CATALOG_FIELDS);
        parse_media_cards(&self.get_json(session, &url)?)
    }

    pub fn load_library_page(
        &self,
        session: &MediaStationSession,
        parent_id: &str,
        start_index: usize,
        limit: usize,
    ) -> Result<MediaPage, ApiError> {
        self.load_library_page_filtered(session, parent_id, start_index, limit, None, None)
    }

    pub fn load_library_page_filtered(
        &self,
        session: &MediaStationSession,
        parent_id: &str,
        start_index: usize,
        limit: usize,
        item_type: Option<&str>,
        genre: Option<&str>,
    ) -> Result<MediaPage, ApiError> {
        validate_identifier("parent_id", parent_id)?;
        validate_page(start_index, limit)?;
        if let Some(item_type) = item_type {
            validate_catalog_item_type(item_type)?;
        }
        if let Some(genre) = genre {
            validate_filter_value("genre", genre)?;
        }
        let mut url = endpoint(&session.base_url, &["Items"])?;
        let mut query = url.query_pairs_mut();
        query
            .append_pair("UserId", &session.user_id)
            .append_pair("ParentId", parent_id)
            .append_pair("StartIndex", &start_index.to_string())
            .append_pair("Limit", &limit.to_string())
            .append_pair("SortBy", "DateCreated")
            .append_pair("SortOrder", "Descending")
            .append_pair("Fields", CATALOG_FIELDS);
        if session.connection.client != MediaStationClientProfile::MediaStationGo {
            query.append_pair("Recursive", "true");
            if item_type.is_none() {
                query.append_pair("IncludeItemTypes", "Movie,Series,Video,MusicVideo,BoxSet");
            }
        }
        if let Some(item_type) = item_type {
            query.append_pair("IncludeItemTypes", item_type);
        }
        if let Some(genre) = genre {
            query.append_pair("Genres", genre.trim());
        }
        drop(query);
        parse_media_page(&self.get_json(session, &url)?, start_index)
    }

    pub fn load_library_filters(
        &self,
        session: &MediaStationSession,
        parent_id: &str,
        collection_type: Option<&str>,
    ) -> Result<MediaLibraryFilters, ApiError> {
        validate_identifier("parent_id", parent_id)?;
        let item_types = library_item_types(collection_type);
        let mut url = endpoint(&session.base_url, &["Genres"])?;
        url.query_pairs_mut()
            .append_pair("UserId", &session.user_id)
            .append_pair("ParentId", parent_id)
            .append_pair("Recursive", "true")
            .append_pair("IncludeItemTypes", &item_types.join(","));
        let payload = self.get_json(session, &url)?;
        let items = payload
            .get("Items")
            .and_then(Value::as_array)
            .ok_or(ApiError::MissingField { field: "Items" })?;
        let genres = items
            .iter()
            .map(|item| {
                item.get("Name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .ok_or(ApiError::MissingField {
                        field: "Genre.Name",
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MediaLibraryFilters { item_types, genres })
    }

    pub fn load_person_page(
        &self,
        session: &MediaStationSession,
        person_id: &str,
        start_index: usize,
        limit: usize,
    ) -> Result<MediaPage, ApiError> {
        validate_identifier("person_id", person_id)?;
        validate_page(start_index, limit)?;
        let mut url = endpoint(&session.base_url, &["Items"])?;
        url.query_pairs_mut()
            .append_pair("UserId", &session.user_id)
            .append_pair("PersonIds", person_id)
            .append_pair("Recursive", "true")
            .append_pair("IncludeItemTypes", "Movie,Series,Video,MusicVideo")
            .append_pair("StartIndex", &start_index.to_string())
            .append_pair("Limit", &limit.to_string())
            .append_pair("SortBy", "ProductionYear,SortName")
            .append_pair("SortOrder", "Descending")
            .append_pair("Fields", CATALOG_FIELDS);
        parse_media_page(&self.get_json(session, &url)?, start_index)
    }

    pub fn search_media(
        &self,
        session: &MediaStationSession,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MediaCard>, ApiError> {
        validate_non_empty("query", query)?;
        if limit == 0 || limit > MAX_CATALOG_PAGE_SIZE {
            return Err(ApiError::InvalidInput {
                field: "limit",
                reason: format!("must be between 1 and {MAX_CATALOG_PAGE_SIZE}"),
            });
        }
        let mut url = endpoint(&session.base_url, &["Items"])?;
        url.query_pairs_mut()
            .append_pair("UserId", &session.user_id)
            .append_pair("SearchTerm", query.trim())
            .append_pair("Recursive", "true")
            .append_pair("IncludeItemTypes", "Movie,Series,Episode,Video,MusicVideo")
            .append_pair("Limit", &limit.to_string())
            .append_pair("Fields", CATALOG_FIELDS);
        parse_media_cards(&self.get_json(session, &url)?)
    }

    pub fn load_media_detail(
        &self,
        session: &MediaStationSession,
        media_id: &str,
    ) -> Result<MediaDetail, ApiError> {
        validate_identifier("media_id", media_id)?;
        let mut url = media_detail_endpoint(session, media_id)?;
        let mut detail_query = url.query_pairs_mut();
        if session.connection.client == MediaStationClientProfile::MediaStationGo {
            detail_query.append_pair("UserId", &session.user_id);
        }
        detail_query.append_pair("Fields", CATALOG_FIELDS);
        drop(detail_query);
        let detail_payload = self.get_json(session, &url)?;
        let item = parse_media_card(&detail_payload)?;
        let people = parse_media_people(&detail_payload)?;
        let mut episodes = if item.media_type == "Series" {
            let mut episodes_url = series_episodes_endpoint(session, media_id)?;
            let mut episodes_query = episodes_url.query_pairs_mut();
            episodes_query
                .append_pair("UserId", &session.user_id)
                .append_pair("Limit", &MAX_DETAIL_EPISODES.to_string())
                .append_pair("SortBy", "ParentIndexNumber,IndexNumber,SortName")
                .append_pair("SortOrder", "Ascending")
                .append_pair("Fields", CATALOG_FIELDS);
            if session.connection.client == MediaStationClientProfile::MediaStationGo {
                episodes_query
                    .append_pair("ParentId", media_id)
                    .append_pair("Recursive", "true")
                    .append_pair("IncludeItemTypes", "Episode");
            }
            drop(episodes_query);
            parse_media_cards(&self.get_json(session, &episodes_url)?)?
        } else {
            Vec::new()
        };
        inherit_episode_landscape_images(&item, &mut episodes);
        Ok(MediaDetail {
            item,
            episodes,
            people,
        })
    }

    pub fn download_media_image(
        &self,
        session: &MediaStationSession,
        image: &MediaImageRef,
        max_width: u32,
        maximum_bytes: usize,
    ) -> Result<MediaImage, ApiError> {
        validate_identifier("image_item_id", &image.item_id)?;
        validate_non_empty("image_tag", &image.tag)?;
        if !(80..=2_000).contains(&max_width) {
            return Err(ApiError::InvalidInput {
                field: "max_width",
                reason: "must be between 80 and 2000".to_string(),
            });
        }
        if maximum_bytes == 0 {
            return Err(ApiError::InvalidInput {
                field: "maximum_bytes",
                reason: "must be greater than zero".to_string(),
            });
        }
        let image_type = match image.image_type {
            MediaImageType::Primary => "Primary".to_string(),
            MediaImageType::Thumb => "Thumb".to_string(),
            MediaImageType::Backdrop => format!("Backdrop/{}", image.image_index.unwrap_or(0)),
            MediaImageType::Logo => "Logo".to_string(),
        };
        let mut url = endpoint_from_path(
            &session.base_url,
            &format!("Items/{}/Images/{image_type}", image.item_id),
        )?;
        url.query_pairs_mut()
            .append_pair("tag", &image.tag)
            .append_pair("maxWidth", &max_width.to_string())
            .append_pair("quality", "86");
        let agent = self.agent_for_session(session)?;
        let mut response = agent
            .get(url.as_str())
            .header(
                HEADER_USER_AGENT,
                self.user_agent_for_profile(session.connection),
            )
            .header(HEADER_EMBY_TOKEN, &session.token)
            .header(HEADER_EMBY_AUTHORIZATION, &session.authorization)
            .call()
            .map_err(|error| transport_error(&url, error))?;
        let status_code = response.status().as_u16();
        if !(200..300).contains(&status_code) {
            return Err(ApiError::HttpStatus {
                url: redacted_url(&url),
                status_code,
                message: format!("HTTP {status_code}"),
            });
        }
        if response
            .headers()
            .get(HEADER_CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|length| length > maximum_bytes as u64)
        {
            return Err(ApiError::ResponseTooLarge {
                url: redacted_url(&url),
                maximum_bytes,
            });
        }
        let content_type = response
            .headers()
            .get(HEADER_CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(normalize_image_content_type)
            .ok_or_else(|| ApiError::InvalidInput {
                field: "image_content_type",
                reason: "server returned an unsupported image format".to_string(),
            })?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(maximum_bytes.saturating_add(1) as u64)
            .read_to_vec()
            .map_err(|error| match error {
                ureq::Error::BodyExceedsLimit(_) => ApiError::ResponseTooLarge {
                    url: redacted_url(&url),
                    maximum_bytes,
                },
                other => ApiError::Transport {
                    url: redacted_url(&url),
                    message: other.to_string(),
                },
            })?;
        if bytes.is_empty() {
            return Err(ApiError::EmptyResponse {
                url: redacted_url(&url),
            });
        }
        Ok(MediaImage {
            bytes,
            content_type,
        })
    }

    pub fn load_playback_source(
        &self,
        session: &MediaStationSession,
        media_id: &str,
    ) -> Result<PlaybackSource, ApiError> {
        validate_non_empty("media_id", media_id)?;
        let mut url = endpoint(&session.base_url, &["Items", media_id, "PlaybackInfo"])?;
        url.query_pairs_mut()
            .append_pair("UserId", &session.user_id)
            .append_pair("AutoOpenLiveStream", "false");
        let payload = self.get_json(session, &url)?;
        parse_playback_source(session, media_id, &payload)
    }

    pub fn report_playing(
        &self,
        session: &MediaStationSession,
        source: &PlaybackSource,
        position_ms: u64,
        paused: bool,
    ) -> Result<(), ApiError> {
        self.report_event(session, "Sessions/Playing", source, position_ms, paused)
    }

    pub fn report_progress(
        &self,
        session: &MediaStationSession,
        source: &PlaybackSource,
        position_ms: u64,
        paused: bool,
    ) -> Result<(), ApiError> {
        self.report_event(
            session,
            "Sessions/Playing/Progress",
            source,
            position_ms,
            paused,
        )
    }

    pub fn report_stopped(
        &self,
        session: &MediaStationSession,
        source: &PlaybackSource,
        position_ms: u64,
    ) -> Result<(), ApiError> {
        self.report_event(
            session,
            "Sessions/Playing/Stopped",
            source,
            position_ms,
            true,
        )
    }

    pub fn load_playback_preference(
        &self,
        session: &MediaStationSession,
        media_id: &str,
        source: &PlaybackSource,
    ) -> Result<PlaybackTrackPreferenceState, ApiError> {
        validate_non_empty("media_id", media_id)?;
        let url = endpoint(
            &session.base_url,
            &["Items", media_id, "PlaybackPreferences"],
        )?;
        match self.get_json(session, &url) {
            Ok(payload) => Ok(PlaybackTrackPreferenceState {
                preference: parse_preference(&payload)?,
                persistence: PlaybackPreferencePersistence::ServerExtension,
            }),
            Err(ApiError::HttpStatus {
                status_code: 404, ..
            }) => Ok(PlaybackTrackPreferenceState {
                preference: standard_emby_preference(source),
                persistence: PlaybackPreferencePersistence::SessionOnly,
            }),
            Err(error) => Err(error),
        }
    }

    pub fn update_playback_preference(
        &self,
        session: &MediaStationSession,
        media_id: &str,
        update: &PlaybackTrackPreferenceUpdate,
    ) -> Result<PlaybackTrackPreference, ApiError> {
        validate_non_empty("media_id", media_id)?;
        if update.subtitle_enabled.is_none()
            && update.subtitle_track_key.is_none()
            && update.audio_track_key.is_none()
        {
            return Err(ApiError::EmptyPreferenceUpdate);
        }
        let mut payload = Map::new();
        if let Some(value) = update.subtitle_enabled {
            payload.insert("subtitle_enabled".to_string(), Value::Bool(value));
        }
        if let Some(value) = &update.subtitle_track_key {
            payload.insert(
                "subtitle_track_key".to_string(),
                Value::String(value.clone()),
            );
        }
        if let Some(value) = &update.audio_track_key {
            payload.insert("audio_track_key".to_string(), Value::String(value.clone()));
        }
        let url = endpoint(
            &session.base_url,
            &["Items", media_id, "PlaybackPreferences"],
        )?;
        parse_preference(&self.put_json(session, &url, &Value::Object(payload))?)
    }

    pub fn download_external_subtitle(
        &self,
        session: &MediaStationSession,
        url: &Url,
        maximum_bytes: usize,
    ) -> Result<ExternalSubtitleDownload, ApiError> {
        if maximum_bytes == 0 {
            return Err(ApiError::InvalidInput {
                field: "maximum_bytes",
                reason: "must be greater than zero".to_string(),
            });
        }
        validate_http_url("subtitle_url", url)?;

        let mut current = url.clone();
        let mut redirect_count = 0;
        let agent = self.agent_for_session(session)?;
        loop {
            reject_cross_origin_private_query(session, &current)?;
            let mut request = agent.get(current.as_str()).header(
                HEADER_USER_AGENT,
                self.user_agent_for_profile(session.connection),
            );
            if same_origin(&session.base_url, &current) {
                request = request
                    .header(HEADER_EMBY_TOKEN, &session.token)
                    .header(HEADER_EMBY_AUTHORIZATION, &session.authorization);
            }
            let mut response = request
                .call()
                .map_err(|error| transport_error(&current, error))?;
            let status_code = response.status().as_u16();
            if is_redirect(status_code) {
                if redirect_count >= MAX_EXTERNAL_RESOURCE_REDIRECTS {
                    return Err(ApiError::RedirectLimitExceeded {
                        url: redacted_url(&current),
                    });
                }
                let location = response
                    .headers()
                    .get(HEADER_LOCATION)
                    .ok_or_else(|| ApiError::RedirectMissingLocation {
                        url: redacted_url(&current),
                        status_code,
                    })?
                    .to_str()
                    .map_err(|error| ApiError::RedirectLocationInvalid {
                        url: redacted_url(&current),
                        message: error.to_string(),
                    })?;
                let next =
                    current
                        .join(location)
                        .map_err(|error| ApiError::RedirectLocationInvalid {
                            url: redacted_url(&current),
                            message: error.to_string(),
                        })?;
                validate_http_url("subtitle_url", &next)?;
                if current.scheme() == "https" && next.scheme() != "https" {
                    return Err(ApiError::RedirectSecurityDowngrade {
                        from: redacted_url(&current),
                        to: redacted_url(&next),
                    });
                }
                reject_cross_origin_private_query(session, &next)?;
                current = next;
                redirect_count += 1;
                continue;
            }
            if !(200..300).contains(&status_code) {
                return Err(ApiError::HttpStatus {
                    url: redacted_url(&current),
                    status_code,
                    message: format!("HTTP {status_code}"),
                });
            }

            if response
                .headers()
                .get(HEADER_CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|length| length > maximum_bytes as u64)
            {
                return Err(ApiError::ResponseTooLarge {
                    url: redacted_url(&current),
                    maximum_bytes,
                });
            }
            let content_type = response
                .headers()
                .get(HEADER_CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let read_limit = maximum_bytes.saturating_add(1);
            let bytes = response
                .body_mut()
                .with_config()
                .limit(read_limit as u64)
                .read_to_vec()
                .map_err(|error| match error {
                    ureq::Error::BodyExceedsLimit(_) => ApiError::ResponseTooLarge {
                        url: redacted_url(&current),
                        maximum_bytes,
                    },
                    other => ApiError::Transport {
                        url: redacted_url(&current),
                        message: other.to_string(),
                    },
                })?;
            if bytes.len() > maximum_bytes {
                return Err(ApiError::ResponseTooLarge {
                    url: redacted_url(&current),
                    maximum_bytes,
                });
            }
            if bytes.is_empty() {
                return Err(ApiError::EmptyResponse {
                    url: redacted_url(&current),
                });
            }
            return Ok(ExternalSubtitleDownload {
                bytes,
                content_type,
                redirect_count,
                target_host: current.host_str().unwrap_or_default().to_string(),
            });
        }
    }

    fn report_event(
        &self,
        session: &MediaStationSession,
        path: &str,
        source: &PlaybackSource,
        position_ms: u64,
        paused: bool,
    ) -> Result<(), ApiError> {
        let url = endpoint_from_path(&session.base_url, path)?;
        let mut payload = Map::from_iter([
            ("ItemId".to_string(), Value::String(source.media_id.clone())),
            (
                "PositionTicks".to_string(),
                Value::from(position_ms.saturating_mul(10_000)),
            ),
            ("IsPaused".to_string(), Value::Bool(paused)),
            ("CanSeek".to_string(), Value::Bool(true)),
            (
                "PlayMethod".to_string(),
                Value::String("DirectPlay".to_string()),
            ),
            (
                "RepeatMode".to_string(),
                Value::String("RepeatNone".to_string()),
            ),
        ]);
        if let Some(media_source_id) = &source.media_source_id {
            payload.insert(
                "MediaSourceId".to_string(),
                Value::String(media_source_id.clone()),
            );
        }
        if let Some(play_session_id) = &source.play_session_id {
            payload.insert(
                "PlaySessionId".to_string(),
                Value::String(play_session_id.clone()),
            );
        }
        self.post_json_empty(session, &url, &Value::Object(payload))
    }

    fn get_json(&self, session: &MediaStationSession, url: &Url) -> Result<Value, ApiError> {
        let agent = self.agent_for_session(session)?;
        let response = agent
            .get(url.as_str())
            .header(
                HEADER_USER_AGENT,
                self.user_agent_for_profile(session.connection),
            )
            .header(HEADER_EMBY_TOKEN, &session.token)
            .header(HEADER_EMBY_AUTHORIZATION, &session.authorization)
            .call()
            .map_err(|error| transport_error(url, error))?;
        read_json_response(url, response)
    }

    fn put_json(
        &self,
        session: &MediaStationSession,
        url: &Url,
        payload: &Value,
    ) -> Result<Value, ApiError> {
        let agent = self.agent_for_session(session)?;
        let response = agent
            .put(url.as_str())
            .header(
                HEADER_USER_AGENT,
                self.user_agent_for_profile(session.connection),
            )
            .header(HEADER_EMBY_TOKEN, &session.token)
            .header(HEADER_EMBY_AUTHORIZATION, &session.authorization)
            .header("content-type", "application/json; charset=utf-8")
            .send(payload.to_string())
            .map_err(|error| transport_error(url, error))?;
        read_json_response(url, response)
    }

    fn post_json_empty(
        &self,
        session: &MediaStationSession,
        url: &Url,
        payload: &Value,
    ) -> Result<(), ApiError> {
        let agent = self.agent_for_session(session)?;
        let mut response = agent
            .post(url.as_str())
            .header(
                HEADER_USER_AGENT,
                self.user_agent_for_profile(session.connection),
            )
            .header(HEADER_EMBY_TOKEN, &session.token)
            .header(HEADER_EMBY_AUTHORIZATION, &session.authorization)
            .header("content-type", "application/json; charset=utf-8")
            .send(payload.to_string())
            .map_err(|error| transport_error(url, error))?;
        let status_code = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|error| ApiError::Transport {
                url: redacted_url(url),
                message: error.to_string(),
            })?;
        ensure_success(url, status_code, &body)
    }
}

fn build_api_agent(proxy: Option<ureq::Proxy>) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(30)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(20)))
        .proxy(proxy)
        .build();
    ureq::Agent::new_with_config(config)
}

fn inherit_episode_landscape_images(series: &MediaCard, episodes: &mut [MediaCard]) {
    let Some(fallback) = series
        .backdrop_image
        .clone()
        .or_else(|| series.landscape_image.clone())
        .or_else(|| series.primary_image.clone())
    else {
        return;
    };
    for episode in episodes {
        if episode.landscape_image.is_none() {
            episode.landscape_image = Some(fallback.clone());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiError {
    InvalidInput {
        field: &'static str,
        reason: String,
    },
    UnsupportedScheme {
        field: &'static str,
        scheme: String,
    },
    EmbeddedCredentials {
        field: &'static str,
    },
    InvalidEndpoint {
        message: String,
    },
    SystemProxyUnavailable,
    Transport {
        url: String,
        message: String,
    },
    HttpStatus {
        url: String,
        status_code: u16,
        message: String,
    },
    InvalidJson {
        url: String,
        message: String,
    },
    MissingField {
        field: &'static str,
    },
    InvalidMediaUrl {
        value: String,
        message: String,
    },
    RedirectLimitExceeded {
        url: String,
    },
    RedirectMissingLocation {
        url: String,
        status_code: u16,
    },
    RedirectLocationInvalid {
        url: String,
        message: String,
    },
    RedirectSecurityDowngrade {
        from: String,
        to: String,
    },
    ResponseTooLarge {
        url: String,
        maximum_bytes: usize,
    },
    EmptyResponse {
        url: String,
    },
    EmptyPreferenceUpdate,
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field, reason } => write!(formatter, "invalid {field}: {reason}"),
            Self::UnsupportedScheme { field, scheme } => {
                write!(formatter, "unsupported {field} scheme: {scheme}")
            }
            Self::EmbeddedCredentials { field } => {
                write!(formatter, "embedded credentials are not allowed in {field}")
            }
            Self::InvalidEndpoint { message } => {
                write!(formatter, "invalid API endpoint: {message}")
            }
            Self::SystemProxyUnavailable => {
                formatter.write_str("the configured system HTTP proxy is unavailable")
            }
            Self::Transport { url, message } => {
                write!(formatter, "API request failed for {url}: {message}")
            }
            Self::HttpStatus {
                url,
                status_code,
                message,
            } => write!(
                formatter,
                "API request {url} returned HTTP {status_code}: {message}"
            ),
            Self::InvalidJson { url, message } => {
                write!(
                    formatter,
                    "API response from {url} is invalid JSON: {message}"
                )
            }
            Self::MissingField { field } => write!(formatter, "API response is missing {field}"),
            Self::InvalidMediaUrl { value, message } => {
                write!(formatter, "invalid media URL {value:?}: {message}")
            }
            Self::RedirectLimitExceeded { url } => {
                write!(formatter, "API redirect limit exceeded for {url}")
            }
            Self::RedirectMissingLocation { url, status_code } => write!(
                formatter,
                "API redirect from {url} returned HTTP {status_code} without Location"
            ),
            Self::RedirectLocationInvalid { url, message } => {
                write!(
                    formatter,
                    "API redirect from {url} has invalid Location: {message}"
                )
            }
            Self::RedirectSecurityDowngrade { from, to } => {
                write!(
                    formatter,
                    "API redirect from {from} to {to} downgrades HTTPS"
                )
            }
            Self::ResponseTooLarge { url, maximum_bytes } => write!(
                formatter,
                "API response from {url} exceeds the {maximum_bytes}-byte limit"
            ),
            Self::EmptyResponse { url } => {
                write!(formatter, "API response from {url} is empty")
            }
            Self::EmptyPreferenceUpdate => {
                formatter.write_str("playback preference update has no fields")
            }
        }
    }
}

impl std::error::Error for ApiError {}

fn parse_media_cards(payload: &Value) -> Result<Vec<MediaCard>, ApiError> {
    let items = match payload {
        Value::Array(items) => items.as_slice(),
        Value::Object(object) => object
            .get("Items")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .ok_or(ApiError::MissingField { field: "Items" })?,
        _ => return Err(ApiError::MissingField { field: "Items" }),
    };
    items.iter().map(parse_media_card).collect()
}

fn parse_media_page(payload: &Value, requested_start: usize) -> Result<MediaPage, ApiError> {
    let object = payload.as_object().ok_or(ApiError::MissingField {
        field: "Items response",
    })?;
    let items = parse_media_cards(payload)?;
    let start_index = object
        .get("StartIndex")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(requested_start);
    let total_record_count = object
        .get("TotalRecordCount")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| start_index.saturating_add(items.len()));
    if start_index != requested_start
        || start_index.saturating_add(items.len()) > total_record_count
    {
        return Err(ApiError::InvalidInput {
            field: "catalog_page",
            reason: "server pagination metadata is inconsistent".to_string(),
        });
    }
    Ok(MediaPage {
        items,
        start_index,
        total_record_count,
    })
}

fn parse_media_card(value: &Value) -> Result<MediaCard, ApiError> {
    let item = value.as_object().ok_or(ApiError::MissingField {
        field: "media item",
    })?;
    let id = required_string(item.get("Id"), "Id")?;
    let title = required_string(item.get("Name"), "Name")?;
    let media_type = required_string(item.get("Type"), "Type")?;
    let user_data = item.get("UserData").and_then(Value::as_object);
    let duration_ms = ticks_to_milliseconds(item.get("RunTimeTicks"));
    let resume_position_ms = user_data
        .and_then(|data| data.get("PlaybackPositionTicks"))
        .map_or(0, |value| ticks_to_milliseconds(Some(value)));
    let image_tags = item.get("ImageTags").and_then(Value::as_object);
    let primary_tag = image_tags.and_then(|tags| optional_string(tags.get("Primary")));
    let thumb_tag = image_tags.and_then(|tags| optional_string(tags.get("Thumb")));
    let own_logo_tag = image_tags.and_then(|tags| optional_string(tags.get("Logo")));
    let parent_logo_tag = optional_string(item.get("ParentLogoImageTag"));
    let image_owner_id =
        optional_string(item.get("PrimaryImageItemId")).unwrap_or_else(|| id.clone());
    let primary_image = primary_tag.map(|tag| MediaImageRef {
        item_id: image_owner_id.clone(),
        image_type: MediaImageType::Primary,
        image_index: None,
        tag,
    });
    let own_backdrop_tag = first_array_string(item.get("BackdropImageTags"));
    let parent_backdrop_tag = first_array_string(item.get("ParentBackdropImageTags"));
    let parent_backdrop_owner = optional_string(item.get("ParentBackdropItemId"))
        .or_else(|| optional_string(item.get("SeriesId")))
        .unwrap_or_else(|| image_owner_id.clone());
    let backdrop_image = own_backdrop_tag
        .map(|tag| MediaImageRef {
            item_id: image_owner_id.clone(),
            image_type: MediaImageType::Backdrop,
            image_index: Some(0),
            tag,
        })
        .or_else(|| {
            parent_backdrop_tag.map(|tag| MediaImageRef {
                item_id: parent_backdrop_owner,
                image_type: MediaImageType::Backdrop,
                image_index: Some(0),
                tag,
            })
        });
    let thumb_image = thumb_tag.map(|tag| MediaImageRef {
        item_id: image_owner_id,
        image_type: MediaImageType::Thumb,
        image_index: None,
        tag,
    });
    let logo_owner_id = if own_logo_tag.is_some() {
        id.clone()
    } else {
        optional_string(item.get("ParentLogoItemId"))
            .or_else(|| optional_string(item.get("SeriesId")))
            .unwrap_or_else(|| id.clone())
    };
    let logo_image = own_logo_tag.or(parent_logo_tag).map(|tag| MediaImageRef {
        item_id: logo_owner_id,
        image_type: MediaImageType::Logo,
        image_index: None,
        tag,
    });
    let landscape_image = if media_type == "CollectionFolder" {
        primary_image.clone()
    } else {
        backdrop_image
            .clone()
            .or(thumb_image)
            .or_else(|| primary_image.clone())
    };
    let video = first_catalog_video_stream(item);

    Ok(MediaCard {
        id,
        title,
        media_type,
        overview: optional_string(item.get("Overview")).unwrap_or_default(),
        collection_type: optional_string(item.get("CollectionType")),
        year: item.get("ProductionYear").and_then(Value::as_i64),
        duration_ms,
        resume_position_ms,
        played: user_data
            .and_then(|data| data.get("Played"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        index_number: item.get("IndexNumber").and_then(Value::as_i64),
        parent_index_number: item.get("ParentIndexNumber").and_then(Value::as_i64),
        parent_id: optional_string(item.get("ParentId")),
        season_id: optional_string(item.get("SeasonId")),
        series_id: optional_string(item.get("SeriesId")),
        series_name: optional_string(item.get("SeriesName")),
        community_rating: positive_f64(item.get("CommunityRating")),
        official_rating: optional_string(item.get("OfficialRating")),
        genres: item
            .get("Genres")
            .and_then(Value::as_array)
            .map(|genres| {
                genres
                    .iter()
                    .filter_map(|genre| optional_string(Some(genre)))
                    .collect()
            })
            .unwrap_or_default(),
        video_codec: video.and_then(|stream| optional_string(stream.get("Codec"))),
        video_profile: video.and_then(|stream| optional_string(stream.get("Profile"))),
        video_width: video
            .and_then(|stream| stream.get("Width"))
            .and_then(Value::as_u64)
            .filter(|value| *value > 0),
        video_height: video
            .and_then(|stream| stream.get("Height"))
            .and_then(Value::as_u64)
            .filter(|value| *value > 0),
        dynamic_range: video.and_then(catalog_dynamic_range),
        primary_image,
        landscape_image,
        backdrop_image,
        logo_image,
    })
}

fn parse_media_people(payload: &Value) -> Result<Vec<MediaPerson>, ApiError> {
    let Some(people) = payload.get("People") else {
        return Ok(Vec::new());
    };
    let people = people
        .as_array()
        .ok_or(ApiError::MissingField { field: "People" })?;
    people
        .iter()
        .map(|value| {
            let person = value
                .as_object()
                .ok_or(ApiError::MissingField { field: "Person" })?;
            let id = required_string(person.get("Id"), "Person.Id")?;
            let name = required_string(person.get("Name"), "Person.Name")?;
            let primary_image =
                optional_string(person.get("PrimaryImageTag")).map(|tag| MediaImageRef {
                    item_id: id.clone(),
                    image_type: MediaImageType::Primary,
                    image_index: None,
                    tag,
                });
            Ok(MediaPerson {
                id,
                name,
                role: optional_string(person.get("Role")),
                person_type: optional_string(person.get("Type")),
                primary_image,
            })
        })
        .collect()
}

fn library_item_types(collection_type: Option<&str>) -> Vec<String> {
    let types: &[&str] = match collection_type
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "movies" => &["Movie"],
        "tvshows" => &["Series"],
        "musicvideos" => &["MusicVideo"],
        "homevideos" => &["Video"],
        "boxsets" => &["BoxSet"],
        _ => &["Movie", "Series", "Video", "MusicVideo"],
    };
    types.iter().map(|value| (*value).to_string()).collect()
}

fn validate_catalog_item_type(value: &str) -> Result<(), ApiError> {
    match value {
        "Movie" | "Series" | "Video" | "MusicVideo" | "BoxSet" => Ok(()),
        _ => Err(ApiError::InvalidInput {
            field: "item_type",
            reason: "is not a supported video catalog item type".to_string(),
        }),
    }
}

fn validate_filter_value(field: &'static str, value: &str) -> Result<(), ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(ApiError::InvalidInput {
            field,
            reason: "must contain 1 to 128 visible characters".to_string(),
        });
    }
    Ok(())
}

fn first_catalog_video_stream(item: &Map<String, Value>) -> Option<&Map<String, Value>> {
    item.get("MediaStreams")
        .and_then(Value::as_array)
        .and_then(|streams| {
            streams.iter().find_map(|stream| {
                let stream = stream.as_object()?;
                stream
                    .get("Type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("Video"))
                    .then_some(stream)
            })
        })
        .or_else(|| {
            item.get("MediaSources")
                .and_then(Value::as_array)
                .and_then(|sources| sources.first())
                .and_then(Value::as_object)
                .and_then(first_catalog_video_stream)
        })
}

fn catalog_dynamic_range(stream: &Map<String, Value>) -> Option<String> {
    let declared = optional_string(stream.get("VideoRange")).unwrap_or_default();
    let range_type = optional_string(stream.get("VideoRangeType")).unwrap_or_default();
    let display = optional_string(stream.get("DisplayTitle")).unwrap_or_default();
    let description = format!("{declared} {range_type} {display}").to_ascii_lowercase();
    if description.contains("dolby vision") || description.contains("dovi") {
        Some("Dolby Vision".to_string())
    } else if description.contains("hdr10+") || description.contains("hdr10plus") {
        Some("HDR10+".to_string())
    } else if description.contains("hdr10") {
        Some("HDR10".to_string())
    } else if description.contains("hlg") {
        Some("HLG".to_string())
    } else if declared.eq_ignore_ascii_case("HDR") {
        Some("HDR".to_string())
    } else if declared.eq_ignore_ascii_case("SDR") {
        Some("SDR".to_string())
    } else {
        None
    }
}

fn first_array_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .and_then(|value| optional_string(Some(value)))
}

fn ticks_to_milliseconds(value: Option<&Value>) -> u64 {
    value
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
        })
        .unwrap_or(0)
        / 10_000
}

fn normalize_image_content_type(value: &str) -> Option<String> {
    let content_type = value.split(';').next()?.trim().to_ascii_lowercase();
    matches!(
        content_type.as_str(),
        "image/jpeg" | "image/png" | "image/webp" | "image/avif"
    )
    .then_some(content_type)
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(ApiError::InvalidInput {
            field,
            reason: "must be a non-empty identifier of at most 256 bytes".to_string(),
        });
    }
    Ok(())
}

fn validate_page(start_index: usize, limit: usize) -> Result<(), ApiError> {
    if start_index > 1_000_000 {
        return Err(ApiError::InvalidInput {
            field: "start_index",
            reason: "must not exceed 1000000".to_string(),
        });
    }
    if limit == 0 || limit > MAX_CATALOG_PAGE_SIZE {
        return Err(ApiError::InvalidInput {
            field: "limit",
            reason: format!("must be between 1 and {MAX_CATALOG_PAGE_SIZE}"),
        });
    }
    Ok(())
}

fn parse_playback_source(
    session: &MediaStationSession,
    media_id: &str,
    payload: &Value,
) -> Result<PlaybackSource, ApiError> {
    let sources = payload
        .get("MediaSources")
        .and_then(Value::as_array)
        .ok_or(ApiError::MissingField {
            field: "MediaSources",
        })?;
    let source = sources
        .iter()
        .filter_map(Value::as_object)
        .find(|source| optional_string(source.get("DirectStreamUrl")).is_some())
        .or_else(|| {
            sources.iter().filter_map(Value::as_object).find(|source| {
                source
                    .get("SupportsDirectPlay")
                    .is_none_or(|value| value.as_bool() != Some(false))
            })
        })
        .ok_or(ApiError::MissingField {
            field: "MediaSources[0].SupportsDirectPlay",
        })?;
    let media_source_id = optional_string(source.get("Id"));
    let play_session_id = optional_string(payload.get("PlaySessionId"));
    let direct_stream_url = optional_string(source.get("DirectStreamUrl"));
    let standard_emby_stream = direct_stream_url.is_none();
    let (url, server_credential_query_removed) = if let Some(raw_url) = direct_stream_url {
        let url = resolve_server_url(&session.base_url, &raw_url)?;
        validate_http_url("media_url", &url)?;
        sanitize_server_resource_url(session, "media_url", url)?
    } else {
        (
            standard_emby_stream_url(
                session,
                media_id,
                source,
                media_source_id.as_deref(),
                play_session_id.as_deref(),
            )?,
            false,
        )
    };
    let streams = source
        .get("MediaStreams")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let video = streams.iter().find_map(|stream| {
        stream
            .as_object()
            .filter(|stream| type_is(stream, "Video"))
            .map(parse_video_stream)
    });
    let mut subtitles = Vec::new();
    let mut audio_tracks = Vec::new();
    let mut subtitle_ordinal = 0;
    let mut audio_ordinal = 0;
    for stream in streams.iter().filter_map(Value::as_object) {
        if type_is(stream, "Subtitle") {
            if let Some(track) = parse_subtitle_track(session, stream, subtitle_ordinal)? {
                subtitles.push(track);
                subtitle_ordinal += 1;
            }
        } else if type_is(stream, "Audio") {
            audio_tracks.push(parse_audio_track(stream, audio_ordinal));
            audio_ordinal += 1;
        }
    }
    Ok(PlaybackSource {
        media_id: media_id.to_string(),
        url,
        standard_emby_stream,
        server_credential_query_removed,
        container: optional_string(source.get("Container")),
        bitrate: source.get("Bitrate").and_then(Value::as_u64),
        media_source_id,
        play_session_id,
        default_audio_stream_index: source
            .get("DefaultAudioStreamIndex")
            .and_then(Value::as_i64),
        default_subtitle_stream_index: source
            .get("DefaultSubtitleStreamIndex")
            .and_then(Value::as_i64),
        video,
        subtitles,
        audio_tracks,
    })
}

fn standard_emby_stream_url(
    session: &MediaStationSession,
    media_id: &str,
    source: &Map<String, Value>,
    media_source_id: Option<&str>,
    play_session_id: Option<&str>,
) -> Result<Url, ApiError> {
    let container = required_string(source.get("Container"), "MediaSource.Container")?;
    let mut url = endpoint(&session.base_url, &["Videos", media_id, "stream"])?;
    let mut query = url.query_pairs_mut();
    query
        .append_pair("Container", &container)
        .append_pair("Static", "true");
    if let Some(media_source_id) = media_source_id {
        query.append_pair("MediaSourceId", media_source_id);
    }
    if let Some(play_session_id) = play_session_id {
        query.append_pair("PlaySessionId", play_session_id);
    }
    drop(query);
    Ok(url)
}

fn parse_video_stream(stream: &Map<String, Value>) -> VideoStream {
    let declared_range = optional_string(stream.get("VideoRange"));
    let range_type = optional_string(stream.get("VideoRangeType"));
    let display_title = optional_string(stream.get("DisplayTitle"));
    let description = format!(
        "{} {} {}",
        declared_range.as_deref().unwrap_or_default(),
        range_type.as_deref().unwrap_or_default(),
        display_title.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();
    let dynamic_range = if description.contains("dolby vision") || description.contains("dovi") {
        Some("Dolby Vision".to_string())
    } else if description.contains("hdr10+") || description.contains("hdr10plus") {
        Some("HDR10+".to_string())
    } else if description.contains("hdr10") {
        Some("HDR10".to_string())
    } else if description.contains("hlg") {
        Some("HLG".to_string())
    } else if declared_range
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("hdr"))
    {
        Some("HDR".to_string())
    } else if declared_range
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("sdr"))
    {
        Some("SDR".to_string())
    } else {
        None
    };
    VideoStream {
        dynamic_range,
        range_type,
        codec: optional_string(stream.get("Codec")),
        profile: optional_string(stream.get("Profile")),
        level: stream.get("Level").and_then(Value::as_i64),
        width: stream.get("Width").and_then(Value::as_u64),
        height: stream.get("Height").and_then(Value::as_u64),
        frame_rate: positive_f64(stream.get("RealFrameRate"))
            .or_else(|| positive_f64(stream.get("AverageFrameRate"))),
        color_space: optional_string(stream.get("ColorSpace")),
        color_transfer: optional_string(stream.get("ColorTransfer")),
        color_range: optional_string(stream.get("ColorRange")),
        bit_depth: stream.get("BitDepth").and_then(Value::as_u64),
        max_content_light_level: stream
            .get("MaxContentLightLevel")
            .or_else(|| stream.get("MaxCLL"))
            .and_then(Value::as_u64),
        max_frame_average_light_level: stream
            .get("MaxFrameAverageLightLevel")
            .or_else(|| stream.get("MaxFALL"))
            .and_then(Value::as_u64),
        dolby_vision_profile: stream.get("VideoDoViProfile").and_then(Value::as_i64),
        dolby_vision_level: stream.get("VideoDoViLevel").and_then(Value::as_i64),
        dolby_vision_base_layer_present: stream.get("VideoBlPresentFlag").and_then(Value::as_bool),
        dolby_vision_compatibility_id: stream
            .get("VideoBlSignalCompatibilityId")
            .and_then(Value::as_i64),
    }
}

fn parse_subtitle_track(
    session: &MediaStationSession,
    stream: &Map<String, Value>,
    ordinal: usize,
) -> Result<Option<SubtitleTrack>, ApiError> {
    let is_external = stream
        .get("IsExternal")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let delivery_url = optional_string(stream.get("DeliveryUrl"));
    if is_external && delivery_url.is_none() {
        return Ok(None);
    }
    let url = delivery_url
        .map(|raw| {
            let url = resolve_server_url(&session.base_url, &raw)?;
            validate_http_url("subtitle_url", &url)?;
            sanitize_server_resource_url(session, "subtitle_url", url).map(|(url, _)| url)
        })
        .transpose()?;
    Ok(Some(SubtitleTrack {
        key: stable_stream_key(stream, "subtitle", ordinal),
        stream_index: stream.get("Index").and_then(Value::as_i64),
        url,
        codec: optional_string(stream.get("Codec")).map(|value| value.to_ascii_lowercase()),
        language: optional_string(stream.get("Language")),
        label: optional_string(stream.get("DisplayTitle")),
        is_default: stream
            .get("IsDefault")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        is_forced: stream
            .get("IsForced")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        is_external,
    }))
}

fn parse_audio_track(stream: &Map<String, Value>, ordinal: usize) -> AudioTrack {
    AudioTrack {
        key: stable_stream_key(stream, "audio", ordinal),
        stream_index: stream.get("Index").and_then(Value::as_i64),
        codec: optional_string(stream.get("Codec")),
        language: optional_string(stream.get("Language")),
        label: optional_string(stream.get("DisplayTitle")),
        channel_count: stream.get("Channels").and_then(Value::as_u64),
    }
}

fn stable_stream_key(stream: &Map<String, Value>, kind: &str, ordinal: usize) -> String {
    if let Some(index) = stream
        .get("Index")
        .and_then(Value::as_i64)
        .filter(|index| *index >= 0)
    {
        return format!("stream:{index}");
    }
    let identity = [
        kind.to_string(),
        optional_string(stream.get("Codec"))
            .unwrap_or_default()
            .to_ascii_lowercase(),
        optional_string(stream.get("Language"))
            .unwrap_or_default()
            .to_ascii_lowercase(),
        optional_string(stream.get("DisplayTitle")).unwrap_or_default(),
        optional_string(stream.get("Path"))
            .unwrap_or_default()
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string(),
        ordinal.to_string(),
    ]
    .join("\0");
    let digest = Sha256::digest(identity.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{kind}:{hex}")
}

fn parse_preference(payload: &Value) -> Result<PlaybackTrackPreference, ApiError> {
    let object = payload.as_object().ok_or(ApiError::MissingField {
        field: "PlaybackPreferences object",
    })?;
    Ok(PlaybackTrackPreference {
        configured: object
            .get("configured")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        subtitle_enabled: object
            .get("subtitle_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        subtitle_track_key: optional_string(object.get("subtitle_track_key")),
        audio_track_key: optional_string(object.get("audio_track_key")),
    })
}

fn standard_emby_preference(source: &PlaybackSource) -> PlaybackTrackPreference {
    let audio_track_key = source
        .default_audio_stream_index
        .and_then(|index| {
            source
                .audio_tracks
                .iter()
                .find(|track| track.stream_index == Some(index))
        })
        .map(|track| track.key.clone());
    let subtitle = source
        .default_subtitle_stream_index
        .and_then(|index| {
            source
                .subtitles
                .iter()
                .find(|track| track.stream_index == Some(index))
        })
        .or_else(|| {
            source
                .subtitles
                .iter()
                .find(|track| track.is_default || track.is_forced)
        });
    PlaybackTrackPreference {
        configured: false,
        subtitle_enabled: subtitle.is_some(),
        subtitle_track_key: subtitle.map(|track| track.key.clone()),
        audio_track_key,
    }
}

fn read_json_response(
    url: &Url,
    mut response: ureq::http::Response<ureq::Body>,
) -> Result<Value, ApiError> {
    let status_code = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| ApiError::Transport {
            url: redacted_url(url),
            message: error.to_string(),
        })?;
    ensure_success(url, status_code, &body)?;
    serde_json::from_str(&body).map_err(|error| ApiError::InvalidJson {
        url: redacted_url(url),
        message: error.to_string(),
    })
}

fn ensure_success(url: &Url, status_code: u16, body: &str) -> Result<(), ApiError> {
    if (200..300).contains(&status_code) {
        return Ok(());
    }
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| optional_string(value.get("Message")))
        .unwrap_or_else(|| format!("HTTP {status_code}"));
    Err(ApiError::HttpStatus {
        url: redacted_url(url),
        status_code,
        message,
    })
}

fn transport_error(url: &Url, error: ureq::Error) -> ApiError {
    ApiError::Transport {
        url: redacted_url(url),
        message: error.to_string(),
    }
}

fn endpoint(base_url: &Url, segments: &[&str]) -> Result<Url, ApiError> {
    let mut url = base_url.clone();
    url.set_query(None);
    url.set_fragment(None);
    let mut path = url
        .path_segments_mut()
        .map_err(|_| ApiError::InvalidEndpoint {
            message: "base URL cannot contain path segments".to_string(),
        })?;
    path.pop_if_empty();
    for segment in segments {
        path.push(segment);
    }
    drop(path);
    Ok(url)
}

fn endpoint_from_path(base_url: &Url, path: &str) -> Result<Url, ApiError> {
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    endpoint(base_url, &segments)
}

fn resume_endpoint(session: &MediaStationSession) -> Result<Url, ApiError> {
    match session.connection.client {
        MediaStationClientProfile::MediaStationGo => {
            endpoint(&session.base_url, &["Items", "Resume"])
        }
        MediaStationClientProfile::SenPlayer | MediaStationClientProfile::Infuse => endpoint(
            &session.base_url,
            &["Users", &session.user_id, "Items", "Resume"],
        ),
    }
}

fn media_detail_endpoint(session: &MediaStationSession, media_id: &str) -> Result<Url, ApiError> {
    match session.connection.client {
        MediaStationClientProfile::MediaStationGo => {
            endpoint(&session.base_url, &["Items", media_id])
        }
        MediaStationClientProfile::SenPlayer | MediaStationClientProfile::Infuse => endpoint(
            &session.base_url,
            &["Users", &session.user_id, "Items", media_id],
        ),
    }
}

fn series_episodes_endpoint(
    session: &MediaStationSession,
    series_id: &str,
) -> Result<Url, ApiError> {
    match session.connection.client {
        MediaStationClientProfile::MediaStationGo => endpoint(&session.base_url, &["Items"]),
        MediaStationClientProfile::SenPlayer | MediaStationClientProfile::Infuse => {
            endpoint(&session.base_url, &["Shows", series_id, "Episodes"])
        }
    }
}

fn resolve_server_url(base_url: &Url, raw: &str) -> Result<Url, ApiError> {
    if let Ok(absolute) = Url::parse(raw) {
        return Ok(absolute);
    }
    let mut directory_base = base_url.clone();
    if !raw.starts_with('/') && !directory_base.path().ends_with('/') {
        let path = format!("{}/", directory_base.path());
        directory_base.set_path(&path);
    }
    directory_base
        .join(raw)
        .map_err(|error| ApiError::InvalidMediaUrl {
            value: redact_url_text(raw),
            message: error.to_string(),
        })
}

fn required_string(value: Option<&Value>, field: &'static str) -> Result<String, ApiError> {
    optional_string(value).ok_or(ApiError::MissingField { field })
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn positive_f64(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn type_is(stream: &Map<String, Value>, expected: &str) -> bool {
    stream
        .get("Type")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        return Err(ApiError::InvalidInput {
            field,
            reason: "must not be empty".to_string(),
        });
    }
    Ok(())
}

fn validate_http_url(field: &'static str, url: &Url) -> Result<(), ApiError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::UnsupportedScheme {
            field,
            scheme: url.scheme().to_string(),
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::EmbeddedCredentials { field });
    }
    Ok(())
}

fn sanitize_server_resource_url(
    session: &MediaStationSession,
    field: &'static str,
    mut url: Url,
) -> Result<(Url, bool), ApiError> {
    let query_pairs = url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    if !query_pairs
        .iter()
        .any(|(name, _)| is_credential_query_name(name))
    {
        return Ok((url, false));
    }
    if !same_origin(&session.base_url, &url) {
        return Err(ApiError::InvalidInput {
            field,
            reason: "cross-origin credential query parameters are prohibited".to_string(),
        });
    }
    let retained = query_pairs
        .into_iter()
        .filter(|(name, _)| !is_credential_query_name(name))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !retained.is_empty() {
        url.query_pairs_mut().extend_pairs(retained);
    }
    Ok((url, true))
}

fn is_credential_query_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "api_key" | "apikey" | "access_token" | "token" | "x-emby-token"
    )
}

fn reject_cross_origin_private_query(
    session: &MediaStationSession,
    url: &Url,
) -> Result<(), ApiError> {
    if !same_origin(&session.base_url, url)
        && url
            .query_pairs()
            .any(|(name, _)| is_credential_query_name(&name))
    {
        return Err(ApiError::InvalidInput {
            field: "subtitle_url",
            reason: "cross-origin credential query parameters are prohibited".to_string(),
        });
    }
    Ok(())
}

fn is_redirect(status_code: u16) -> bool {
    matches!(status_code, 301 | 302 | 303 | 307 | 308)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

fn redacted_url(url: &Url) -> String {
    let mut redacted = url.clone();
    if redacted.query().is_some() {
        redacted.set_query(Some("<redacted>"));
    }
    redacted.set_fragment(None);
    redacted.to_string()
}

fn redact_url_text(value: &str) -> String {
    let end = value.find(['?', '#']).unwrap_or(value.len());
    if end == value.len() {
        value.to_string()
    } else {
        format!("{}?<redacted>", &value[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::thread;

    fn session(base_url: Url) -> MediaStationSession {
        session_with_profile(base_url, MediaStationConnectionProfile::media_station_go())
    }

    fn session_with_profile(
        base_url: Url,
        connection: MediaStationConnectionProfile,
    ) -> MediaStationSession {
        MediaStationSession::new_with_profile(
            base_url,
            "user-1",
            "secret-token",
            format!(
                "MediaBrowser Client=\"{}\", Token=\"secret-token\"",
                connection.client.authorization_client()
            ),
            connection,
        )
        .expect("session should be valid")
    }

    fn serve_once(response: String) -> (Url, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener.local_addr().expect("address should resolve");
        let handle = thread::spawn(move || {
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
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let content_length = String::from_utf8_lossy(&request)
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .or_else(|| line.strip_prefix("Content-Length: "))
                        })
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    let header_end = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|position| position + 4)
                        .unwrap_or(request.len());
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
            }
            stream
                .write_all(response.as_bytes())
                .expect("response should be writable");
            String::from_utf8(request).expect("request should be UTF-8")
        });
        (
            Url::parse(&format!("http://{address}")).expect("URL should parse"),
            handle,
        )
    }

    #[test]
    fn authenticate_posts_credentials_without_exposing_the_token() {
        let body = json!({
            "AccessToken": "returned-secret-token",
            "User": {
                "Id": "user-42",
                "Name": "Test User"
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (base_url, server) = serve_once(response);
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let result = client
            .authenticate(
                &base_url,
                " test-user ",
                "test-password",
                "MediaBrowser Client=\"MediaStation Windows\"",
            )
            .expect("authentication should parse");
        let request = server.join().expect("server thread should finish");
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("POST /Users/AuthenticateByName HTTP/1.1"));
        assert!(request_lower.contains("x-emby-authorization:"));
        assert!(!request_lower.contains("x-emby-token:"));
        let request_body = request
            .split("\r\n\r\n")
            .nth(1)
            .expect("request body should exist");
        let request_payload: Value =
            serde_json::from_str(request_body).expect("request body should be JSON");
        assert_eq!(request_payload["Username"], "test-user");
        assert_eq!(request_payload["Pw"], "test-password");
        assert_eq!(result.user_id, "user-42");
        assert_eq!(result.user_name, "Test User");
        assert_eq!(result.access_token_secret(), "returned-secret-token");
        let debug = format!("{result:?}");
        assert!(!debug.contains("returned-secret-token"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn authenticate_rejects_query_bearing_server_url_before_network() {
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");
        let url = Url::parse("https://media.example?api_key=secret").expect("URL should parse");

        let error = client
            .authenticate(&url, "user", "password", "authorization")
            .expect_err("query-bearing base URL must fail");

        assert!(matches!(
            error,
            ApiError::InvalidInput {
                field: "base_url",
                ..
            }
        ));
    }

    #[test]
    fn connection_profiles_keep_msg_direct_and_standard_emby_proxied() {
        let msg = MediaStationConnectionProfile::media_station_go();
        let proxied_msg = MediaStationConnectionProfile {
            client: MediaStationClientProfile::MediaStationGo,
            proxy: MediaStationProxyMode::System,
        };
        let senplayer =
            MediaStationConnectionProfile::standard_emby(MediaStationClientProfile::SenPlayer)
                .expect("SenPlayer should be supported");
        let infuse =
            MediaStationConnectionProfile::standard_emby(MediaStationClientProfile::Infuse)
                .expect("Infuse should be supported");

        assert_eq!(msg.proxy, MediaStationProxyMode::Direct);
        assert_eq!(
            msg.client.user_agent("MediaStationGoWindows/0.1"),
            "MediaStationGoWindows/0.1"
        );
        assert_eq!(senplayer.proxy, MediaStationProxyMode::Direct);
        assert_eq!(senplayer.client.user_agent("ignored"), "SenPlayer/1.0.0");
        assert_eq!(infuse.proxy, MediaStationProxyMode::Direct);
        assert_eq!(infuse.client.user_agent("ignored"), "Infuse/1.0.0");
        assert!(msg.is_supported());
        assert!(proxied_msg.is_supported());
        assert!(senplayer.is_supported());
        assert!(infuse.is_supported());
        assert!(
            MediaStationConnectionProfile::standard_emby(MediaStationClientProfile::MediaStationGo)
                .is_none()
        );
    }

    #[test]
    fn resume_endpoint_preserves_msg_path_and_uses_standard_emby_contract() {
        let base_url = Url::parse("https://media.example/emby").expect("URL should parse");
        let msg = session(base_url.clone());
        let standard = session_with_profile(
            base_url,
            MediaStationConnectionProfile::standard_emby(MediaStationClientProfile::SenPlayer)
                .expect("SenPlayer should be a standard Emby profile"),
        );

        assert_eq!(
            resume_endpoint(&msg)
                .expect("MSG endpoint should build")
                .path(),
            "/emby/Items/Resume"
        );
        assert_eq!(
            resume_endpoint(&standard)
                .expect("standard Emby endpoint should build")
                .path(),
            "/emby/Users/user-1/Items/Resume"
        );
    }

    #[test]
    fn detail_endpoints_preserve_msg_paths_and_use_standard_emby_contract() {
        let base_url = Url::parse("https://media.example/emby").expect("URL should parse");
        let msg = session(base_url.clone());
        let standard = session_with_profile(
            base_url,
            MediaStationConnectionProfile::standard_emby(MediaStationClientProfile::SenPlayer)
                .expect("SenPlayer should be a standard Emby profile"),
        );

        assert_eq!(
            media_detail_endpoint(&msg, "media-1")
                .expect("MSG detail endpoint should build")
                .path(),
            "/emby/Items/media-1"
        );
        assert_eq!(
            media_detail_endpoint(&standard, "media-1")
                .expect("standard Emby detail endpoint should build")
                .path(),
            "/emby/Users/user-1/Items/media-1"
        );
        assert_eq!(
            series_episodes_endpoint(&msg, "series-1")
                .expect("MSG episodes endpoint should build")
                .path(),
            "/emby/Items"
        );
        assert_eq!(
            series_episodes_endpoint(&standard, "series-1")
                .expect("standard Emby episodes endpoint should build")
                .path(),
            "/emby/Shows/series-1/Episodes"
        );
    }

    #[test]
    fn standard_emby_latest_uses_user_scoped_contract() {
        let body = json!([{
            "Id": "movie-1",
            "Name": "Latest Movie",
            "Type": "Movie"
        }])
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (base_url, server) = serve_once(response);
        let profile =
            MediaStationConnectionProfile::standard_emby(MediaStationClientProfile::SenPlayer)
                .expect("SenPlayer should be a standard Emby profile");
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let items = client
            .load_latest_items(&session_with_profile(base_url, profile), "library-1", 18)
            .expect("standard Emby latest items should load");
        let request = server.join().expect("server thread should finish");
        let target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request target should exist");
        let parsed =
            Url::parse(&format!("http://localhost{target}")).expect("request target should parse");
        let params = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(parsed.path(), "/Users/user-1/Items/Latest");
        assert_eq!(
            params.get("ParentId").map(|value| value.as_ref()),
            Some("library-1")
        );
        assert_eq!(params.get("Limit").map(|value| value.as_ref()), Some("18"));
        assert!(params.get("Fields").is_some());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].media_type, "Movie");
    }

    #[test]
    fn session_allows_explicit_system_proxy_without_changing_direct_defaults() {
        let session = MediaStationSession::new_with_profile(
            Url::parse("https://media.example").expect("URL should parse"),
            "user-1",
            "token",
            "authorization",
            MediaStationConnectionProfile {
                client: MediaStationClientProfile::MediaStationGo,
                proxy: MediaStationProxyMode::System,
            },
        )
        .expect("a system proxy may be selected explicitly");

        assert_eq!(
            MediaStationConnectionProfile::media_station_go().proxy,
            MediaStationProxyMode::Direct
        );
        assert_eq!(session.connection.proxy, MediaStationProxyMode::System);
    }

    #[test]
    fn media_card_exposes_image_references_without_urls() {
        let payload = json!({
            "Id": "episode-1",
            "Name": "Episode One",
            "Type": "Episode",
            "SeriesId": "series-1",
            "SeriesName": "Example Series",
            "ParentIndexNumber": 1,
            "IndexNumber": 2,
            "RunTimeTicks": 18_000_000_000_u64,
            "UserData": {
                "PlaybackPositionTicks": 6_000_000_000_u64,
                "Played": false
            },
            "ImageTags": {
                "Primary": "primary-tag",
                "Logo": "logo-tag"
            },
            "ParentBackdropImageTags": ["backdrop-tag"],
            "ParentBackdropItemId": "series-1",
            "MediaSources": [{
                "MediaStreams": [{
                    "Type": "Video",
                    "Codec": "hevc",
                    "Profile": "Main 10",
                    "Width": 3840,
                    "Height": 2160,
                    "VideoRange": "HDR",
                    "VideoRangeType": "HDR10"
                }]
            }]
        });

        let card = parse_media_card(&payload).expect("media card should parse");

        assert_eq!(card.resume_position_ms, 600_000);
        assert_eq!(card.duration_ms, 1_800_000);
        assert_eq!(card.parent_index_number, Some(1));
        assert_eq!(card.index_number, Some(2));
        assert_eq!(card.dynamic_range.as_deref(), Some("HDR10"));
        assert_eq!(card.video_width, Some(3840));
        assert_eq!(
            card.landscape_image,
            Some(MediaImageRef {
                item_id: "series-1".to_string(),
                image_type: MediaImageType::Backdrop,
                image_index: Some(0),
                tag: "backdrop-tag".to_string(),
            })
        );
        assert!(card.is_playable());
        assert_eq!(
            card.logo_image,
            Some(MediaImageRef {
                item_id: "episode-1".to_string(),
                image_type: MediaImageType::Logo,
                image_index: None,
                tag: "logo-tag".to_string(),
            })
        );
        assert!(!format!("{card:?}").contains("api_key"));
    }

    #[test]
    fn episode_landscape_fallback_uses_series_backdrop_without_overwriting_episode_art() {
        let series = parse_media_card(&json!({
            "Id": "series-1",
            "Name": "Example Series",
            "Type": "Series",
            "ImageTags": { "Primary": "series-primary" },
            "BackdropImageTags": ["series-backdrop"]
        }))
        .expect("series should parse");
        let missing_art = parse_media_card(&json!({
            "Id": "episode-1",
            "Name": "Episode One",
            "Type": "Episode"
        }))
        .expect("episode without art should parse");
        let own_art = parse_media_card(&json!({
            "Id": "episode-2",
            "Name": "Episode Two",
            "Type": "Episode",
            "ImageTags": { "Primary": "episode-primary" }
        }))
        .expect("episode with art should parse");
        let expected_own_art = own_art.landscape_image.clone();
        let mut episodes = vec![missing_art, own_art];

        inherit_episode_landscape_images(&series, &mut episodes);

        assert_eq!(episodes[0].landscape_image, series.backdrop_image);
        assert_eq!(episodes[1].landscape_image, expected_own_art);
    }

    #[test]
    fn episode_landscape_fallback_uses_series_primary_when_backdrop_is_absent() {
        let series = parse_media_card(&json!({
            "Id": "series-1",
            "Name": "Example Series",
            "Type": "Series",
            "ImageTags": { "Primary": "series-primary" }
        }))
        .expect("series should parse");
        let episode = parse_media_card(&json!({
            "Id": "episode-1",
            "Name": "Episode One",
            "Type": "Episode"
        }))
        .expect("episode should parse");
        let mut episodes = vec![episode];

        inherit_episode_landscape_images(&series, &mut episodes);

        assert_eq!(episodes[0].landscape_image, series.primary_image);
    }

    #[test]
    fn detail_people_keep_roles_and_primary_image_references() {
        let payload = json!({
            "People": [{
                "Id": "person-1",
                "Name": "Example Actor",
                "Role": "Lead",
                "Type": "Actor",
                "PrimaryImageTag": "person-image"
            }]
        });

        let people = parse_media_people(&payload).expect("people should parse");

        assert_eq!(people.len(), 1);
        assert_eq!(people[0].role.as_deref(), Some("Lead"));
        assert_eq!(people[0].person_type.as_deref(), Some("Actor"));
        assert_eq!(
            people[0].primary_image,
            Some(MediaImageRef {
                item_id: "person-1".to_string(),
                image_type: MediaImageType::Primary,
                image_index: None,
                tag: "person-image".to_string(),
            })
        );
    }

    #[test]
    fn library_page_sends_server_side_type_and_genre_filters() {
        let body = json!({
            "StartIndex": 0,
            "TotalRecordCount": 0,
            "Items": []
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let (base_url, server) = serve_once(response);
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        client
            .load_library_page_filtered(
                &session(base_url),
                "library-1",
                0,
                48,
                Some("Movie"),
                Some("科幻 动作"),
            )
            .expect("filtered library page should load");
        let request = server.join().expect("server thread should finish");
        let target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request target should exist");
        let parsed =
            Url::parse(&format!("http://localhost{target}")).expect("request target should parse");
        let params = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(
            params.get("IncludeItemTypes").map(|value| value.as_ref()),
            Some("Movie")
        );
        assert_eq!(
            params.get("Genres").map(|value| value.as_ref()),
            Some("科幻 动作")
        );
        assert_eq!(
            params.get("StartIndex").map(|value| value.as_ref()),
            Some("0")
        );
        assert_eq!(params.get("Limit").map(|value| value.as_ref()), Some("48"));
        assert!(params.get("Recursive").is_none());
    }

    #[test]
    fn standard_emby_library_page_recurses_without_returning_folder_nodes() {
        let body = json!({
            "StartIndex": 0,
            "TotalRecordCount": 0,
            "Items": []
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let (base_url, server) = serve_once(response);
        let profile =
            MediaStationConnectionProfile::standard_emby(MediaStationClientProfile::SenPlayer)
                .expect("SenPlayer should be a standard Emby profile");
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        client
            .load_library_page(&session_with_profile(base_url, profile), "library-1", 0, 48)
            .expect("standard Emby library page should load");
        let request = server.join().expect("server thread should finish");
        let target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request target should exist");
        let parsed =
            Url::parse(&format!("http://localhost{target}")).expect("request target should parse");
        let params = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(
            params.get("Recursive").map(|value| value.as_ref()),
            Some("true")
        );
        assert_eq!(
            params.get("IncludeItemTypes").map(|value| value.as_ref()),
            Some("Movie,Series,Video,MusicVideo,BoxSet")
        );
    }

    #[test]
    fn standard_emby_search_sends_items_query_and_parses_results() {
        let body = json!({
            "Items": [{
                "Id": "movie-1",
                "Name": "Search Hit",
                "Type": "Movie",
                "ImageTags": { "Primary": "poster-tag" }
            }]
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let (base_url, server) = serve_once(response);
        let profile =
            MediaStationConnectionProfile::standard_emby(MediaStationClientProfile::SenPlayer)
                .expect("SenPlayer should be a standard Emby profile");
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let results = client
            .search_media(&session_with_profile(base_url, profile), "matrix", 12)
            .expect("standard Emby search should parse");
        let request = server.join().expect("server thread should finish");
        let target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request target should exist");
        let parsed =
            Url::parse(&format!("http://localhost{target}")).expect("request target should parse");
        let params = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        let request_lower = request.to_ascii_lowercase();

        assert_eq!(
            params.get("UserId").map(|value| value.as_ref()),
            Some("user-1")
        );
        assert_eq!(
            params.get("SearchTerm").map(|value| value.as_ref()),
            Some("matrix")
        );
        assert_eq!(
            params.get("Recursive").map(|value| value.as_ref()),
            Some("true")
        );
        assert_eq!(
            params.get("IncludeItemTypes").map(|value| value.as_ref()),
            Some("Movie,Series,Episode,Video,MusicVideo")
        );
        assert_eq!(params.get("Limit").map(|value| value.as_ref()), Some("12"));
        assert!(params.get("Fields").is_some());
        assert!(request_lower.contains("x-emby-token: secret-token"));
        assert!(request_lower.contains("user-agent: senplayer/1.0.0"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "movie-1");
        assert_eq!(results[0].title, "Search Hit");
        assert_eq!(
            results[0].primary_image,
            Some(MediaImageRef {
                item_id: "movie-1".to_string(),
                image_type: MediaImageType::Primary,
                image_index: None,
                tag: "poster-tag".to_string(),
            })
        );
    }

    #[test]
    fn image_download_keeps_auth_native_and_bounds_the_response() {
        let image_bytes = b"not-a-real-jpeg";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            image_bytes.len(),
            String::from_utf8_lossy(image_bytes)
        );
        let (base_url, server) = serve_once(response);
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");
        let image = MediaImageRef {
            item_id: "item-1".to_string(),
            image_type: MediaImageType::Backdrop,
            image_index: Some(0),
            tag: "image-tag".to_string(),
        };

        let downloaded = client
            .download_media_image(&session(base_url), &image, 960, 1024)
            .expect("image should download");
        let request = server.join().expect("server thread should finish");
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("GET /Items/item-1/Images/Backdrop/0?"));
        assert!(request_lower.contains("x-emby-token: secret-token"));
        assert_eq!(downloaded.bytes, image_bytes);
        assert_eq!(downloaded.content_type, "image/jpeg");
    }

    #[test]
    fn catalog_page_rejects_inconsistent_server_counts() {
        let payload = json!({
            "StartIndex": 10,
            "TotalRecordCount": 10,
            "Items": [{
                "Id": "movie-1",
                "Name": "Movie",
                "Type": "Movie"
            }]
        });

        let error = parse_media_page(&payload, 10).expect_err("inconsistent page must fail");

        assert!(matches!(
            error,
            ApiError::InvalidInput {
                field: "catalog_page",
                ..
            }
        ));
    }

    #[test]
    fn playback_info_parses_hdr_and_stable_tracks() {
        let body = json!({
            "PlaySessionId": "play-session-1",
            "MediaSources": [{
                "Id": "source-1",
                "DirectStreamUrl": "/Videos/media-1/stream",
                "Container": "mkv",
                "Bitrate": 10000000,
                "MediaStreams": [
                    {
                        "Type": "Video",
                        "Index": 0,
                        "Codec": "hevc",
                        "Profile": "Main 10",
                        "Width": 3840,
                        "Height": 2160,
                        "RealFrameRate": 23.976,
                        "VideoRange": "HDR",
                        "VideoRangeType": "HDR10",
                        "ColorSpace": "bt2020nc",
                        "ColorTransfer": "smpte2084",
                        "ColorRange": "tv",
                        "BitDepth": 10,
                        "MaxCLL": 1000,
                        "MaxFALL": 400
                    },
                    {
                        "Type": "Audio",
                        "Index": 1,
                        "Codec": "eac3",
                        "DisplayTitle": "English Atmos",
                        "Channels": 8
                    },
                    {
                        "Type": "Audio",
                        "Index": 4,
                        "Codec": "ac3",
                        "Language": "zh-CN",
                        "DisplayTitle": "Chinese 5.1",
                        "Channels": 6
                    },
                    {
                        "Type": "Subtitle",
                        "Codec": "srt",
                        "Language": "zh-CN",
                        "DisplayTitle": "Chinese",
                        "Path": "/subs/chinese.srt",
                        "IsExternal": false
                    }
                ]
            }]
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (base_url, server) = serve_once(response);
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let source = client
            .load_playback_source(&session(base_url), "media-1")
            .expect("PlaybackInfo should parse");
        let request = server.join().expect("server thread should finish");
        let request_lower = request.to_ascii_lowercase();

        assert!(request.starts_with("GET /Items/media-1/PlaybackInfo?"));
        assert!(request_lower.contains("x-emby-token: secret-token"));
        assert_eq!(source.media_source_id.as_deref(), Some("source-1"));
        assert_eq!(source.play_session_id.as_deref(), Some("play-session-1"));
        assert_eq!(
            source.video.as_ref().and_then(|video| video.bit_depth),
            Some(10)
        );
        assert_eq!(
            source
                .video
                .as_ref()
                .and_then(|video| video.dynamic_range.as_deref()),
            Some("HDR10")
        );
        assert_eq!(source.audio_tracks.len(), 2);
        assert_eq!(source.audio_tracks[0].key, "stream:1");
        assert_eq!(source.audio_tracks[1].key, "stream:4");
        assert!(source.subtitles[0].key.starts_with("subtitle:"));
        assert_eq!(source.subtitles[0].key.len(), "subtitle:".len() + 64);
        assert!(!source.standard_emby_stream);
    }

    #[test]
    fn standard_emby_playback_info_builds_authenticated_static_stream_url() {
        let session = session(Url::parse("https://media.example/emby").expect("URL should parse"));
        let payload = json!({
            "PlaySessionId": "play-session-standard",
            "MediaSources": [
                {
                    "Id": "unsupported-source",
                    "Container": "iso",
                    "SupportsDirectPlay": false
                },
                {
                    "Id": "source-standard",
                    "Path": "D:\\Media\\Movie.mkv",
                    "Protocol": "File",
                    "Container": "mkv",
                    "SupportsDirectPlay": true,
                    "DefaultAudioStreamIndex": 4,
                    "DefaultSubtitleStreamIndex": 7,
                    "MediaStreams": [
                        { "Type": "Video", "Index": 0, "Codec": "hevc" },
                        { "Type": "Audio", "Index": 1, "Codec": "aac" },
                        { "Type": "Audio", "Index": 4, "Codec": "ac3" },
                        {
                            "Type": "Subtitle",
                            "Index": 7,
                            "Codec": "srt",
                            "IsDefault": true,
                            "IsExternal": false
                        }
                    ]
                }
            ]
        });

        let source = parse_playback_source(&session, "media-1", &payload)
            .expect("standard Emby PlaybackInfo should parse");
        let query = source
            .url
            .query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(source.url.path(), "/emby/Videos/media-1/stream");
        assert_eq!(query.get("Container").map(String::as_str), Some("mkv"));
        assert_eq!(query.get("Static").map(String::as_str), Some("true"));
        assert_eq!(
            query.get("MediaSourceId").map(String::as_str),
            Some("source-standard")
        );
        assert_eq!(
            query.get("PlaySessionId").map(String::as_str),
            Some("play-session-standard")
        );
        assert!(!source.url.as_str().contains("Movie.mkv"));
        assert!(source.standard_emby_stream);
        assert_eq!(source.media_source_id.as_deref(), Some("source-standard"));
        assert_eq!(source.default_audio_stream_index, Some(4));
        assert_eq!(source.default_subtitle_stream_index, Some(7));
    }

    #[test]
    fn standard_emby_rejects_sources_that_explicitly_disable_direct_play() {
        let session = session(Url::parse("https://media.example").expect("URL should parse"));
        let error = parse_playback_source(
            &session,
            "media-1",
            &json!({
                "MediaSources": [{
                    "Id": "transcode-only",
                    "Container": "mkv",
                    "SupportsDirectPlay": false
                }]
            }),
        )
        .expect_err("transcode-only standard sources must fail explicitly");

        assert!(matches!(
            error,
            ApiError::MissingField {
                field: "MediaSources[0].SupportsDirectPlay"
            }
        ));
    }

    #[test]
    fn standard_emby_without_preference_extension_uses_playback_info_defaults() {
        let response =
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
        let (base_url, server) = serve_once(response);
        let session = session(base_url.clone());
        let source = parse_playback_source(
            &session,
            "media-1",
            &json!({
                "MediaSources": [{
                    "Id": "source-1",
                    "Container": "mkv",
                    "SupportsDirectPlay": true,
                    "DefaultAudioStreamIndex": 4,
                    "DefaultSubtitleStreamIndex": 7,
                    "MediaStreams": [
                        { "Type": "Audio", "Index": 1, "Codec": "aac" },
                        { "Type": "Audio", "Index": 4, "Codec": "ac3" },
                        {
                            "Type": "Subtitle",
                            "Index": 7,
                            "Codec": "srt",
                            "IsExternal": false
                        }
                    ]
                }]
            }),
        )
        .expect("standard source should parse");
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let state = client
            .load_playback_preference(&session, "media-1", &source)
            .expect("missing extension should select explicit session-only preferences");
        let request = server.join().expect("server thread should finish");

        assert!(request.starts_with("GET /Items/media-1/PlaybackPreferences HTTP/1.1"));
        assert_eq!(
            state.persistence,
            PlaybackPreferencePersistence::SessionOnly
        );
        assert!(!state.preference.configured);
        assert_eq!(
            state.preference.audio_track_key.as_deref(),
            Some("stream:4")
        );
        assert!(state.preference.subtitle_enabled);
        assert_eq!(
            state.preference.subtitle_track_key.as_deref(),
            Some("stream:7")
        );
    }

    #[test]
    fn media_station_preference_extension_remains_server_persisted() {
        let body = json!({
            "configured": true,
            "subtitle_enabled": false,
            "audio_track_key": "stream:4"
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (base_url, server) = serve_once(response);
        let session = session(base_url.clone());
        let source = parse_playback_source(
            &session,
            "media-1",
            &json!({
                "MediaSources": [{
                    "Id": "source-1",
                    "DirectStreamUrl": "/Videos/media-1/stream"
                }]
            }),
        )
        .expect("extension source should parse");
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let state = client
            .load_playback_preference(&session, "media-1", &source)
            .expect("preference extension should parse");
        server.join().expect("server thread should finish");

        assert_eq!(
            state.persistence,
            PlaybackPreferencePersistence::ServerExtension
        );
        assert!(state.preference.configured);
        assert!(!state.preference.subtitle_enabled);
        assert_eq!(
            state.preference.audio_track_key.as_deref(),
            Some("stream:4")
        );
    }

    #[test]
    fn progress_report_uses_server_contract() {
        let response =
            "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
        let (base_url, server) = serve_once(response);
        let session = session(base_url.clone());
        let source = PlaybackSource {
            media_id: "media-1".to_string(),
            url: base_url,
            standard_emby_stream: false,
            server_credential_query_removed: false,
            container: None,
            bitrate: None,
            media_source_id: Some("source-1".to_string()),
            play_session_id: Some("play-session-1".to_string()),
            default_audio_stream_index: None,
            default_subtitle_stream_index: None,
            video: None,
            subtitles: Vec::new(),
            audio_tracks: Vec::new(),
        };
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        client
            .report_progress(&session, &source, 12_345, false)
            .expect("progress should report");
        let request = server.join().expect("server thread should finish");

        assert!(request.starts_with("POST /Sessions/Playing/Progress HTTP/1.1"));
        let body = request.split("\r\n\r\n").nth(1).expect("body should exist");
        let payload: Value = serde_json::from_str(body).expect("body should be JSON");
        assert_eq!(payload["PositionTicks"], 123_450_000_u64);
        assert_eq!(payload["PlayMethod"], "DirectPlay");
        assert_eq!(payload["IsPaused"], false);
    }

    #[test]
    fn external_subtitle_download_uses_auth_only_on_server_origin() {
        let subtitle_body = "1\n00:00:00,000 --> 00:00:01,000\nSubtitle\n";
        let target_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-subrip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{subtitle_body}",
            subtitle_body.len()
        );
        let (target_base, target_server) = serve_once(target_response);
        let redirect_response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {target_base}/subtitle.srt\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (server_base, redirect_server) = serve_once(redirect_response);
        let session = session(server_base.clone());
        let subtitle_url = server_base
            .join("/Videos/media-1/Subtitles/2/Stream.srt")
            .expect("subtitle URL should resolve");
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let download = client
            .download_external_subtitle(&session, &subtitle_url, 1024)
            .expect("subtitle should download");
        let server_request = redirect_server
            .join()
            .expect("redirect server thread should finish")
            .to_ascii_lowercase();
        let target_request = target_server
            .join()
            .expect("target server thread should finish")
            .to_ascii_lowercase();

        assert_eq!(download.bytes, subtitle_body.as_bytes());
        assert_eq!(download.redirect_count, 1);
        assert_eq!(
            download.content_type.as_deref(),
            Some("application/x-subrip")
        );
        assert!(server_request.contains("x-emby-token: secret-token"));
        assert!(server_request.contains("x-emby-authorization:"));
        assert!(!target_request.contains("x-emby-token:"));
        assert!(!target_request.contains("x-emby-authorization:"));
    }

    #[test]
    fn external_subtitle_download_rejects_oversized_body() {
        let response = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nhello".to_string();
        let (base_url, server) = serve_once(response);
        let session = session(base_url.clone());
        let subtitle_url = base_url
            .join("/subtitle.srt")
            .expect("subtitle URL should resolve");
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let error = client
            .download_external_subtitle(&session, &subtitle_url, 4)
            .expect_err("oversized subtitle should fail");
        server.join().expect("server thread should finish");

        assert!(matches!(
            error,
            ApiError::ResponseTooLarge {
                maximum_bytes: 4,
                ..
            }
        ));
    }

    #[test]
    fn external_subtitle_redirect_rejects_cross_origin_private_query() {
        let response = "HTTP/1.1 302 Found\r\nLocation: https://cdn.example/subtitle.srt?api_key=secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string();
        let (base_url, server) = serve_once(response);
        let session = session(base_url.clone());
        let subtitle_url = base_url
            .join("/subtitle.srt")
            .expect("subtitle URL should resolve");
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let error = client
            .download_external_subtitle(&session, &subtitle_url, 1024)
            .expect_err("cross-origin private query should fail");
        server.join().expect("server thread should finish");

        assert!(matches!(
            error,
            ApiError::InvalidInput {
                field: "subtitle_url",
                ..
            }
        ));
    }

    #[test]
    fn preference_update_rejects_empty_payload() {
        let session = session(Url::parse("https://media.example").expect("URL should parse"));
        let client =
            MediaStationApiClient::new("MediaStationWindows/0.1").expect("client should be valid");

        let error = client
            .update_playback_preference(
                &session,
                "media-1",
                &PlaybackTrackPreferenceUpdate::default(),
            )
            .expect_err("empty updates must fail");

        assert_eq!(error, ApiError::EmptyPreferenceUpdate);
    }

    #[test]
    fn session_debug_redacts_credentials() {
        let session = session(Url::parse("https://media.example").expect("URL should parse"));
        let debug = format!("{session:?}");

        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn relative_media_url_preserves_server_base_path() {
        let base = Url::parse("https://media.example/jellyfin").expect("URL should parse");

        let resolved =
            resolve_server_url(&base, "Videos/media-1/stream").expect("URL should resolve");

        assert_eq!(
            resolved.as_str(),
            "https://media.example/jellyfin/Videos/media-1/stream"
        );
    }

    #[test]
    fn server_base_url_rejects_query_credentials() {
        let error = MediaStationSession::new(
            Url::parse("https://media.example?api_key=secret").expect("URL should parse"),
            "user-1",
            "secret-token",
            "authorization",
        )
        .expect_err("query-bearing base URL must fail");

        assert!(matches!(
            error,
            ApiError::InvalidInput {
                field: "base_url",
                ..
            }
        ));
    }

    #[test]
    fn playback_info_strips_same_origin_credential_query() {
        let session = session(Url::parse("https://media.example").expect("URL should parse"));
        let payload = json!({
            "PlaySessionId": "play-session-1",
            "MediaSources": [{
                "Id": "source-1",
                "DirectStreamUrl": "/Videos/media-1/stream?static=true&api_key=secret"
            }]
        });

        let source = parse_playback_source(&session, "media-1", &payload)
            .expect("same-origin credential query should be removed");

        assert_eq!(
            source.url.as_str(),
            "https://media.example/Videos/media-1/stream?static=true"
        );
        assert!(source.server_credential_query_removed);
    }

    #[test]
    fn cross_origin_credential_query_is_rejected() {
        let session = session(Url::parse("https://media.example").expect("URL should parse"));
        let error = sanitize_server_resource_url(
            &session,
            "media_url",
            Url::parse("https://cdn.example/video?api_key=secret").expect("URL should parse"),
        )
        .expect_err("cross-origin credential query must be rejected");

        assert!(matches!(
            error,
            ApiError::InvalidInput {
                field: "media_url",
                ..
            }
        ));
    }
}
