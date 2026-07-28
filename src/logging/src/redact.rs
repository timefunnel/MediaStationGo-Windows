//! Secret redaction for log output. Known token values and every HTTP(S) URL
//! query are overwritten in place so signed CDN links cannot enter logs.

struct PatternRule {
    needle: &'static [u8],
    terminators: &'static [u8],
}

const URL_TERMINATORS: &[u8] = b"&\"' \t\r\n;<>";
const FULL_URL_TERMINATORS: &[u8] = b"\"' \t\r\n;<>()[]{}";
const JSON_TERMINATORS: &[u8] = b"\"";
const HTTP_PREFIXES: &[&[u8]] = &[b"http://", b"https://"];

const RULES: &[PatternRule] = &[
    PatternRule {
        needle: b"api_key=",
        terminators: URL_TERMINATORS,
    },
    PatternRule {
        needle: b"X-MediaBrowser-Token%3D",
        terminators: URL_TERMINATORS,
    },
    PatternRule {
        needle: b"X-MediaBrowser-Token=",
        terminators: URL_TERMINATORS,
    },
    PatternRule {
        needle: b"ApiKey=",
        terminators: URL_TERMINATORS,
    },
    PatternRule {
        needle: b"AccessToken=",
        terminators: URL_TERMINATORS,
    },
    PatternRule {
        needle: b"AccessToken\":\"",
        terminators: JSON_TERMINATORS,
    },
];

fn find_subslice(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn find_token_end(buf: &[u8], from: usize, terminators: &[u8]) -> usize {
    buf[from..]
        .iter()
        .position(|c| terminators.contains(c))
        .map(|p| p + from)
        .unwrap_or(buf.len())
}

fn elide(buf: &mut [u8], rule: &PatternRule) {
    let mut start = 0;
    while let Some(pos) = find_subslice(buf, rule.needle, start) {
        let token_start = pos + rule.needle.len();
        let token_end = find_token_end(buf, token_start, rule.terminators);
        for b in &mut buf[token_start..token_end] {
            *b = b'x';
        }
        start = if token_end > token_start {
            token_end
        } else {
            token_start
        };
    }
}

fn find_url_query(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let url_start = HTTP_PREFIXES
        .iter()
        .filter_map(|prefix| find_subslice(buf, prefix, from))
        .min()?;
    let url_end = find_token_end(buf, url_start, FULL_URL_TERMINATORS);
    let query_start = buf[url_start..url_end]
        .iter()
        .position(|byte| *byte == b'?')
        .map(|position| url_start + position + 1)?;
    (query_start < url_end).then_some((query_start, url_end))
}

fn elide_url_queries(buf: &mut [u8]) {
    let mut start = 0;
    while let Some((query_start, url_end)) = find_url_query(buf, start) {
        for byte in &mut buf[query_start..url_end] {
            *byte = b'x';
        }
        start = url_end;
    }
}

pub fn contains_secret(buf: &[u8]) -> bool {
    for rule in RULES {
        if let Some(pos) = find_subslice(buf, rule.needle, 0) {
            let token_start = pos + rule.needle.len();
            if token_start < buf.len() && !rule.terminators.contains(&buf[token_start]) {
                return true;
            }
        }
    }
    find_url_query(buf, 0).is_some()
}

pub fn censor(buf: &mut [u8]) {
    for rule in RULES {
        elide(buf, rule);
    }
    elide_url_queries(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn censor_str(s: &str) -> String {
        let mut bytes = s.as_bytes().to_vec();
        censor(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[test]
    fn url_token() {
        assert_eq!(
            censor_str("/path?api_key=abc123&x=1"),
            "/path?api_key=xxxxxx&x=1"
        );
        assert!(contains_secret(b"/path?api_key=abc"));
    }

    #[test]
    fn json_token() {
        assert_eq!(
            censor_str("\"AccessToken\":\"abc\""),
            "\"AccessToken\":\"xxx\""
        );
    }

    #[test]
    fn empty_token() {
        assert_eq!(censor_str("api_key=&x=1"), "api_key=&x=1");
        assert!(!contains_secret(b"api_key=&x=1"));
    }

    #[test]
    fn header_encoded() {
        assert_eq!(
            censor_str("X-MediaBrowser-Token%3Dabcdef HTTP"),
            "X-MediaBrowser-Token%3Dxxxxxx HTTP"
        );
    }

    #[test]
    fn signed_cdn_query_is_fully_redacted() {
        let output = censor_str(
            "Opening https://cdn.example/video.mkv?t=123&u=456&k=private-signature next",
        );
        let redacted_query = output
            .strip_prefix("Opening https://cdn.example/video.mkv?")
            .and_then(|value| value.strip_suffix(" next"))
            .expect("redacted URL shape should be preserved");
        assert!(redacted_query.chars().all(|character| character == 'x'));
        assert!(contains_secret(
            b"Opening https://cdn.example/video.mkv?t=123&k=secret"
        ));
        assert!(!output.contains("private-signature"));
    }

    #[test]
    fn queryless_url_is_not_changed() {
        assert_eq!(
            censor_str("Opening https://cdn.example/video.mkv"),
            "Opening https://cdn.example/video.mkv"
        );
        assert!(!contains_secret(b"Opening https://cdn.example/video.mkv"));
    }

    #[test]
    fn no_pattern() {
        assert_eq!(censor_str("plain message"), "plain message");
        assert!(!contains_secret(b"plain message"));
    }
}
