//! Secret redaction, ported from the v1 Codex parser.
//!
//! The two v1 patterns are reproduced exactly, quirks included, because parser
//! parity is measured against v1's output byte for byte. In particular the
//! second pattern's character classes contain a literal backslash and the
//! letter `s` rather than the whitespace class, and its `+srv` branch can only
//! match a literal backslash — that is what v1 does, so that is what this does.
//!
//! Applied to Codex turns only. Claude Code turns are not redacted by v1.

const KEYWORDS: [&str; 6] = ["token", "secret", "password", "api_key", "api-key", "apikey"];

pub fn redact(text: &str) -> String {
    let after_assignments = redact_assignments(text);
    redact_mongodb_urls(&after_assignments)
}

/// v1 pattern 1: `(?i)(token|secret|password|api[_-]?key|credential)([\"'\s:=]+)[^\"'\s,}]+`
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

/// v1 pattern 2: `mongodb(?:\\+srv)?://[^\\s\"']+` with no capture group, so the
/// replacement is the literal `secret<redacted>`.
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
    let after_scheme = start + "mongodb".len();

    // `(?:\\+srv)?` — one or more literal backslashes then `srv`. Greedy option
    // first, then the empty option, as the regex engine tries them.
    let mut cursor = after_scheme;
    let mut backslashes = cursor;
    while backslashes < chars.len() && chars[backslashes] == '\\' {
        backslashes += 1;
    }
    if backslashes > cursor && starts_with_at(chars, backslashes, "srv") {
        cursor = backslashes + 3;
    }
    if !starts_with_at(chars, cursor, "://") {
        // Retry without the optional group.
        cursor = after_scheme;
        if !starts_with_at(chars, cursor, "://") {
            return None;
        }
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
    !(c == '\\' || c == 's' || c == '"' || c == '\'')
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
        // The v1 class `[^\\s\"']+` excludes the letter `s`, so the match stops
        // at the `s` in `host`. Reproduced deliberately.
        assert_eq!(redact("see mongodb://host/db"), "see secret<redacted>st/db");
    }

    #[test]
    fn plus_srv_form_is_not_matched_because_v1_does_not_match_it() {
        let text = "mongodb+srv://cluster/db";
        assert_eq!(redact(text), text);
    }
}
