//! Danmaku fetching for the Emby-compatible endpoint.
//!
//! The Windows client authenticates as an Emby client, so it reads danmaku from
//! `/emby/api/danmaku/:id` rather than from the web API. The server owns
//! matching, cleaning and the time axis offsets; this module only turns the
//! response into types the layout engine can use, and keeps the three server
//! states apart: matched, unmatched, service unavailable.

use serde_json::Value;

/// One comment, already normalised by the server.
#[derive(Debug, Clone, PartialEq)]
pub struct DanmakuComment {
    pub cid: String,
    /// Seconds on the media timeline, with the provider shift and the user
    /// offset already applied by the server.
    pub time: f64,
    pub mode: u8,
    pub color: u32,
    pub text: String,
}

/// A matched danmaku payload.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaDanmaku {
    pub media_id: String,
    pub source: String,
    pub episode_id: String,
    pub anime_title: String,
    pub episode_title: String,
    pub match_mode: String,
    pub provider_shift_seconds: f64,
    pub offset_seconds: f64,
    pub count: usize,
    pub total: usize,
    pub filtered: usize,
    pub skipped: usize,
    pub truncated: bool,
    pub comments: Vec<DanmakuComment>,
}

/// Server state for one media item.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaDanmakuState {
    Matched(Box<MediaDanmaku>),
    /// The server matched nothing; `message` explains what it tried.
    Unmatched { message: String },
    /// Danmaku is disabled, unconfigured, or the upstream is unreachable.
    /// Deliberately distinct from `Unmatched`: a configuration problem must not
    /// look like "this episode has no danmaku".
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DanmakuParseError {
    InvalidJson { message: String },
    MissingField { field: &'static str },
    InvalidField { field: &'static str },
    UnexpectedStatus { status: u16 },
}

impl std::fmt::Display for DanmakuParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson { message } => write!(formatter, "invalid danmaku JSON: {message}"),
            Self::MissingField { field } => write!(formatter, "danmaku response is missing {field}"),
            Self::InvalidField { field } => write!(formatter, "danmaku response has an invalid {field}"),
            Self::UnexpectedStatus { status } => {
                write!(formatter, "unexpected danmaku response status {status}")
            }
        }
    }
}

/// Query parameters for the danmaku endpoint.
pub fn danmaku_query(ch_convert: u8, offset_seconds: Option<f64>) -> Vec<(String, String)> {
    let mut query = vec![
        ("ch_convert".to_string(), ch_convert.min(2).to_string()),
        ("with_related".to_string(), "true".to_string()),
    ];
    if let Some(offset) = offset_seconds {
        query.push(("offset_seconds".to_string(), format!("{offset}")));
    }
    query
}

/// Turn an HTTP response into the server state it represents.
pub fn parse_danmaku_response(status: u16, body: &str) -> Result<MediaDanmakuState, DanmakuParseError> {
    match status {
        200 => Ok(MediaDanmakuState::Matched(Box::new(parse_matched(body)?))),
        404 => Ok(MediaDanmakuState::Unmatched { message: error_message(body) }),
        503 => Ok(MediaDanmakuState::Unavailable { message: error_message(body) }),
        other => Err(DanmakuParseError::UnexpectedStatus { status: other }),
    }
}

fn parse_matched(body: &str) -> Result<MediaDanmaku, DanmakuParseError> {
    let value: Value = serde_json::from_str(body).map_err(|error| DanmakuParseError::InvalidJson {
        message: error.to_string(),
    })?;
    let comments = value
        .get("comments")
        .ok_or(DanmakuParseError::MissingField { field: "comments" })?
        .as_array()
        .ok_or(DanmakuParseError::InvalidField { field: "comments" })?;
    let mut parsed = Vec::with_capacity(comments.len());
    for comment in comments {
        parsed.push(parse_comment(comment)?);
    }
    Ok(MediaDanmaku {
        media_id: string_field(&value, "media_id"),
        source: string_field(&value, "source"),
        episode_id: string_field(&value, "episode_id"),
        anime_title: string_field(&value, "anime_title"),
        episode_title: string_field(&value, "episode_title"),
        match_mode: string_field(&value, "match_mode"),
        provider_shift_seconds: number_field(&value, "provider_shift_seconds"),
        offset_seconds: number_field(&value, "offset_seconds"),
        count: usize_field(&value, "count", parsed.len()),
        total: usize_field(&value, "total", parsed.len()),
        filtered: usize_field(&value, "filtered", 0),
        skipped: usize_field(&value, "skipped", 0),
        truncated: value.get("truncated").and_then(Value::as_bool).unwrap_or(false),
        comments: parsed,
    })
}

fn parse_comment(value: &Value) -> Result<DanmakuComment, DanmakuParseError> {
    let text = value
        .get("m")
        .and_then(Value::as_str)
        .ok_or(DanmakuParseError::MissingField { field: "comments[].m" })?;
    let time = value
        .get("time")
        .and_then(Value::as_f64)
        .ok_or(DanmakuParseError::MissingField { field: "comments[].time" })?;
    // The server sends cid as a string because it exceeds the JS safe integer
    // range; accept a number too so a stale server cannot break playback.
    let cid = match value.get("cid") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    };
    let mode = value
        .get("mode")
        .and_then(Value::as_u64)
        .and_then(|mode| u8::try_from(mode).ok())
        .unwrap_or(1);
    let color = value
        .get("color")
        .and_then(Value::as_u64)
        .and_then(|color| u32::try_from(color).ok())
        .unwrap_or(0x00FF_FFFF)
        & 0x00FF_FFFF;
    Ok(DanmakuComment {
        cid,
        time,
        mode,
        color,
        text: text.to_string(),
    })
}

fn string_field(value: &Value, field: &'static str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number_field(value: &Value, field: &'static str) -> f64 {
    value.get(field).and_then(Value::as_f64).unwrap_or(0.0)
}

fn usize_field(value: &Value, field: &'static str, fallback: usize) -> usize {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|number| usize::try_from(number).ok())
        .unwrap_or(fallback)
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matched_body() -> String {
        serde_json::json!({
            "media_id": "media-1",
            "source": "dandanplay",
            "episode_id": "95410010",
            "anime_title": "进击的巨人",
            "episode_title": "第10话",
            "match_mode": "tmdb",
            "provider_shift_seconds": 2.0,
            "offset_seconds": -1.5,
            "count": 2,
            "total": 5,
            "filtered": 1,
            "skipped": 0,
            "truncated": true,
            "comments": [
                {
                    "cid": "1542278977442529280",
                    "p": "847.66,1,16020176,14b30012",
                    "m": "前方高能",
                    "time": 847.66,
                    "mode": 1,
                    "color": 16020176
                },
                { "cid": 42, "p": "1.0,5,16777215,u", "m": "顶部", "time": 1.0, "mode": 5, "color": 16777215 }
            ]
        })
        .to_string()
    }

    #[test]
    fn matched_response_keeps_server_fields_and_string_cid() {
        let state = parse_danmaku_response(200, &matched_body()).expect("parsed");
        let MediaDanmakuState::Matched(payload) = state else {
            panic!("expected a matched payload");
        };
        assert_eq!(payload.episode_id, "95410010");
        assert_eq!(payload.match_mode, "tmdb");
        assert_eq!(payload.provider_shift_seconds, 2.0);
        assert_eq!(payload.offset_seconds, -1.5);
        assert_eq!(payload.total, 5);
        assert_eq!(payload.filtered, 1);
        assert!(payload.truncated);
        assert_eq!(payload.comments.len(), 2);
        assert_eq!(payload.comments[0].cid, "1542278977442529280");
        assert!(payload.comments[0].cid.len() > 15, "cid must stay a string");
        assert_eq!(payload.comments[0].time, 847.66);
        assert_eq!(payload.comments[0].mode, 1);
        assert_eq!(payload.comments[0].color, 16020176);
        assert_eq!(payload.comments[1].cid, "42", "numeric cid still parses");
        assert_eq!(payload.comments[1].mode, 5);
    }

    #[test]
    fn empty_comment_list_is_still_a_match() {
        // "Matched but this episode has no comments" is a different thing from
        // "no danmaku library matched", and the caller shows them differently.
        let body = serde_json::json!({ "episode_id": "1", "comments": [] }).to_string();
        let state = parse_danmaku_response(200, &body).expect("parsed");
        let MediaDanmakuState::Matched(payload) = state else {
            panic!("expected a matched payload");
        };
        assert!(payload.comments.is_empty());
        assert_eq!(payload.count, 0);
    }

    #[test]
    fn unmatched_and_unavailable_stay_distinct() {
        let unmatched = parse_danmaku_response(
            404,
            r#"{"error":"danmaku has no match for this media: [...]","code":"danmaku_unmatched"}"#,
        )
        .expect("parsed");
        assert!(matches!(unmatched, MediaDanmakuState::Unmatched { .. }));
        let MediaDanmakuState::Unmatched { message } = unmatched else {
            unreachable!()
        };
        assert!(message.contains("no match"));

        let unavailable = parse_danmaku_response(
            503,
            r#"{"error":"danmaku is unavailable","code":"danmaku_unavailable"}"#,
        )
        .expect("parsed");
        assert!(matches!(unavailable, MediaDanmakuState::Unavailable { .. }));
    }

    #[test]
    fn unexpected_status_and_broken_bodies_are_errors_not_empty_danmaku() {
        assert_eq!(
            parse_danmaku_response(500, "boom"),
            Err(DanmakuParseError::UnexpectedStatus { status: 500 })
        );
        assert!(matches!(
            parse_danmaku_response(200, "not json"),
            Err(DanmakuParseError::InvalidJson { .. })
        ));
        assert_eq!(
            parse_danmaku_response(200, r#"{"episode_id":"1"}"#),
            Err(DanmakuParseError::MissingField { field: "comments" })
        );
        assert_eq!(
            parse_danmaku_response(200, r#"{"comments":{}}"#),
            Err(DanmakuParseError::InvalidField { field: "comments" })
        );
    }

    #[test]
    fn a_comment_without_time_or_text_is_rejected() {
        let body = serde_json::json!({ "comments": [{ "cid": "1", "time": 1.0 }] }).to_string();
        assert_eq!(
            parse_danmaku_response(200, &body),
            Err(DanmakuParseError::MissingField { field: "comments[].m" })
        );
    }

    #[test]
    fn query_carries_conversion_and_optional_offset() {
        assert_eq!(
            danmaku_query(1, None),
            vec![
                ("ch_convert".to_string(), "1".to_string()),
                ("with_related".to_string(), "true".to_string()),
            ]
        );
        let with_offset = danmaku_query(2, Some(-1.5));
        assert!(with_offset.contains(&("offset_seconds".to_string(), "-1.5".to_string())));
        // Out-of-range conversion is clamped instead of being sent as-is.
        assert_eq!(danmaku_query(9, None)[0].1, "2");
    }
}
