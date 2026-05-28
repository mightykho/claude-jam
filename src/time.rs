//! Timestamp parsing and relative-time formatting.
//!
//! The TUI shows things like `2m ago` next to each session, computed from the
//! `updated_at` column SQLite writes via `CURRENT_TIMESTAMP` (`YYYY-MM-DD HH:MM:SS` in UTC).

/// Sessions inactive longer than this are rendered with the `💤` status emoji.
pub const STALE_THRESHOLD_SECS: i64 = 300;

/// Parse a SQLite `CURRENT_TIMESTAMP` string into seconds since the Unix epoch.
///
/// Returns `0` on any parsing failure — the caller treats that as "very stale,"
/// which is fine for a UI hint and avoids panicking on a malformed row.
pub fn parse_timestamp(updated_at: &str) -> i64 {
    let parts: Vec<&str> = updated_at.split(['-', ' ', ':']).collect();
    if parts.len() < 6 {
        return 0;
    }
    let (year, month, day, hour, min, sec) = match (
        parts[0].parse::<i64>(),
        parts[1].parse::<i64>(),
        parts[2].parse::<i64>(),
        parts[3].parse::<i64>(),
        parts[4].parse::<i64>(),
        parts[5].parse::<i64>(),
    ) {
        (Ok(y), Ok(mo), Ok(d), Ok(h), Ok(mi), Ok(s)) => (y, mo, d, h, mi, s),
        _ => return 0,
    };

    let days_in_month: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut total_days: i64 = 0;
    for y in 1970..year {
        total_days += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
    }
    let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    for (m, &days) in days_in_month.iter().enumerate().take((month - 1) as usize) {
        total_days += days;
        if m == 1 && is_leap {
            total_days += 1;
        }
    }
    total_days += day - 1;
    total_days * 86400 + hour * 3600 + min * 60 + sec
}

/// Seconds elapsed between the timestamp and now.
pub fn seconds_since(updated_at: &str) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    now - parse_timestamp(updated_at)
}

/// Render `updated_at` as a short relative string (`"3s ago"`, `"5m ago"`,
/// `"2h ago"`, `"4d ago"`). Negative diffs collapse to `"just now"`.
pub fn relative_time(updated_at: &str) -> String {
    let diff = seconds_since(updated_at);
    if diff < 0 {
        return "just now".to_string();
    }
    if diff < 60 {
        format!("{}s ago", diff)
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_timestamp_handles_sqlite_format() {
        assert_eq!(parse_timestamp("1970-01-01 00:00:00"), 0);
        assert_eq!(parse_timestamp("1970-01-01 00:00:01"), 1);
        assert_eq!(parse_timestamp("1970-01-02 00:00:00"), 86400);
    }

    #[test]
    fn parse_timestamp_returns_zero_on_garbage() {
        assert_eq!(parse_timestamp("not a date"), 0);
        assert_eq!(parse_timestamp(""), 0);
    }
}
