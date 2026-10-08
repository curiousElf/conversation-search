//! ISO-8601 timestamps as both CLIs write them: `2026-02-03T09:00:00.000Z`, and
//! occasionally with an offset. Only the seconds matter for sorting, so
//! fractional digits are read and discarded.

/// Seconds since the Unix epoch, or `None` when the string is not a timestamp.
pub fn to_unix_seconds(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let number = |start: usize, len: usize| -> Option<i64> {
        text.get(start..start + len)?.parse::<i64>().ok()
    };
    if bytes[4] != b'-' || bytes[7] != b'-' || (bytes[10] != b'T' && bytes[10] != b' ') {
        return None;
    }
    let year = number(0, 4)?;
    let month = number(5, 2)?;
    let day = number(8, 2)?;
    let hour = number(11, 2)?;
    let minute = number(14, 2)?;
    let second = number(17, 2)?;

    let mut seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;

    // A trailing `+HH:MM` or `-HH:MM` shifts the instant; `Z` and a missing zone
    // are both treated as UTC.
    if let Some(offset_start) = text.rfind(['+', '-']).filter(|i| *i > 10) {
        let sign = if bytes[offset_start] == b'+' { -1 } else { 1 };
        let hours = number(offset_start + 1, 2).unwrap_or(0);
        let minutes = number(offset_start + 4, 2).unwrap_or(0);
        seconds += sign * (hours * 3600 + minutes * 60);
    }
    Some(seconds)
}

/// Days between 1970-01-01 and the given date. Howard Hinnant's civil calendar
/// algorithm; valid for any year in range.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_forms_both_clis_write() {
        assert_eq!(to_unix_seconds("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(to_unix_seconds("2026-02-03T09:00:00.000Z"), Some(1_770_109_200));
        assert_eq!(to_unix_seconds("2026-02-03T09:00:00Z"), Some(1_770_109_200));
    }

    #[test]
    fn offset_shifts_the_instant() {
        assert_eq!(
            to_unix_seconds("2026-02-03T14:30:00+05:30"),
            to_unix_seconds("2026-02-03T09:00:00Z")
        );
    }

    #[test]
    fn rejects_anything_that_is_not_a_timestamp() {
        assert_eq!(to_unix_seconds(""), None);
        assert_eq!(to_unix_seconds("not a date at all"), None);
    }
}
