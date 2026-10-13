//! Secret redaction for Codex turns, applied before indexing.
//!
//! Two patterns:
//! 1. A credential keyword followed by `:`/`=`/quotes/whitespace and a value:
//!    the keyword is kept and the value becomes `<redacted>`.
//! 2. A `mongodb://` or `mongodb+srv://` connection string, replaced whole by
//!    `secret<redacted>`.
//!
//! Claude Code turns are not redacted.

const KEYWORDS: [&str; 6] = ["token", "secret", "password", "api_key", "api-key", "apikey"];

pub fn redact(text: &str) -> String {
    let after_assignments = redact_assignments(text);
    redact_mongodb_urls(&after_assignments)
}

/// Pattern 1, as a regex: `(?i)(token|secret|password|api[_-]?key|credential)([\"'\s:=]+)[^\"'\s,}]+`
fn redact_assignments(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        match match_assignment(&chars, i) {
            Some((keyword_len, match_end)) => {
                let keyword: String = chars[i..i + keyword_len].iter().collect();
                out.push_str(&keyword);
                out.push_str("<redacted>");
                i = match_end;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// Returns (length of the keyword group, end of the whole match) if the pattern
/// matches starting exactly at `start`.
fn match_assignment(chars: &[char], start: usize) -> Option<(usize, usize)> {
    let keyword_len = match_keyword(chars, start)?;
    let separators_start = start + keyword_len;

    // `[\"'\s:=]+` is greedy, then `[^\"'\s,}]+` must match at least one char.
    // Give characters back one at a time, exactly as the regex engine backtracks.
    let mut separators_end = separators_start;
    while separators_end < chars.len() && is_separator(chars[separators_end]) {
        separators_end += 1;
    }
    let mut end = separators_end;
    while end > separators_start {
        if end < chars.len() && is_value(chars[end]) {
            let mut value_end = end;
            while value_end < chars.len() && is_value(chars[value_end]) {
                value_end += 1;
            }
            return Some((keyword_len, value_end));
        }
        end -= 1;
    }
    None
}

/// Alternation in regex order: `token|secret|password|api[_-]?key|credential`.
fn match_keyword(chars: &[char], start: usize) -> Option<usize> {
    for keyword in KEYWORDS {
        if matches_ignoring_case(chars, start, keyword) {
            return Some(keyword.chars().count());
        }
    }
    if matches_ignoring_case(chars, start, "credential") {
        return Some("credential".len());
    }
    None
}

fn matches_ignoring_case(chars: &[char], start: usize, needle: &str) -> bool {
    let mut i = start;
    for want in needle.chars() {
        if i >= chars.len() {
            return false;
        }
        if chars[i].to_ascii_lowercase() != want {
            return false;
        }
        i += 1;
    }
    true
}

fn is_separator(c: char) -> bool {
    c == '"' || c == '\'' || c == ':' || c == '=' || is_python_space(c)
}

fn is_value(c: char) -> bool {
    !(c == '"' || c == '\'' || c == ',' || c == '}' || is_python_space(c))
}

/// Python's `\s` for str patterns: the Unicode whitespace property plus the
/// four ASCII separator controls.
fn is_python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Pattern 2, as a regex: `mongodb(?:\+srv)?://[^\s\"']+`.
fn redact_mongodb_urls(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < chars.len() {
        match match_mongodb_url(&chars, i) {
            Some(end) => {
                out.push_str("secret<redacted>");
                i = end;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

fn match_mongodb_url(chars: &[char], start: usize) -> Option<usize> {
    if !starts_with_at(chars, start, "mongodb") {
        return None;
    }
    let mut cursor = start + "mongodb".len();
    if starts_with_at(chars, cursor, "+srv") {
        cursor += "+srv".len();
    }
    if !starts_with_at(chars, cursor, "://") {
        return None;
    }
    let mut end = cursor + 3;
    let body_start = end;
    while end < chars.len() && is_url_body(chars[end]) {
        end += 1;
    }
    if end == body_start {
        return None;
    }
    Some(end)
}

fn is_url_body(c: char) -> bool {
    !(c == '"' || c == '\'' || is_python_space(c))
}

fn starts_with_at(chars: &[char], start: usize, needle: &str) -> bool {
    let mut i = start;
    for want in needle.chars() {
        if i >= chars.len() || chars[i] != want {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_keeps_keyword_and_drops_value() {
        assert_eq!(redact("token=abc123"), "token<redacted>");
        assert_eq!(redact("API_KEY: \"xyz\""), "API_KEY<redacted>\"");
        assert_eq!(redact("no secrets here"), "no secrets here");
    }

    #[test]
    fn mongodb_url_is_replaced_wholesale() {
        assert_eq!(
            redact("see mongodb://user:pass@hosts.example.com:27017/db?ssl=true done"),
            "see secret<redacted> done"
        );
    }

    #[test]
    fn plus_srv_url_is_replaced_wholesale() {
        assert_eq!(
            redact("mongodb+srv://user:pass@cluster0.example.mongodb.net/db"),
            "secret<redacted>"
        );
    }

    #[test]
    fn mongodb_url_stops_at_quotes_and_whitespace() {
        assert_eq!(redact("uri=\"mongodb://h/db\" next"), "uri=\"secret<redacted>\" next");
        assert_eq!(redact("a mongodb://h/db\tb"), "a secret<redacted>\tb");
    }

    #[test]
    fn mongodb_without_a_url_is_left_alone() {
        assert_eq!(redact("mongodb is a database"), "mongodb is a database");
        assert_eq!(redact("mongodb:// alone"), "mongodb:// alone");
        assert_eq!(redact("mongodb+srv:/typo"), "mongodb+srv:/typo");
    }
}
