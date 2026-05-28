//! Pure rendering helpers for the TUI.
//!
//! Returning `ratatui::Color` ties this module to the UI layer, which is why
//! it lives on the binary side rather than in the lib. Everything in here is
//! still pure and unit-testable.

use ratatui::style::Color;

/// Emoji prefix shown next to each session in the dashboard.
pub fn status_emoji(status: &str, is_stale: bool) -> &'static str {
    if is_stale {
        return "💤";
    }
    match status {
        "working" => "🔨",
        "waiting" => "🔔",
        "idle" => "✅",
        "pending" => "⏳",
        _ => "❓",
    }
}

/// Color the tmux session name is rendered in.
pub fn status_color(status: &str, is_stale: bool) -> Color {
    if is_stale {
        return Color::Cyan;
    }
    match status {
        "working" => Color::Blue,
        "waiting" => Color::Yellow,
        "idle" => Color::Green,
        "pending" => Color::DarkGray,
        _ => Color::Gray,
    }
}

/// Truncate `s` to at most `max` characters, replacing the last char with `…`
/// when truncation happens. Char-boundary-safe — never panics on multi-byte
/// UTF-8 input the way naive `&s[..n]` slicing would.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Single-character shortcut label for the i-th session: `1`-`9`, then `a`-`z`.
/// Returns `" "` past 35.
pub fn shortcut_label(i: usize) -> String {
    match i {
        0..=8 => format!("{}", i + 1),
        9..=34 => format!("{}", (b'a' + (i - 9) as u8) as char),
        _ => " ".to_string(),
    }
}

/// Build the 8-cell context-window mini-bar shown next to each session.
///
/// Returns the rendered string (`"▓▓▓░░░░░ 38%"`) and a color: green under 60%,
/// yellow 60–80%, red 80%+. `None` when `total` is non-positive so callers can
/// silently skip rendering for unpopulated rows.
pub fn format_context_bar(used: i64, total: i64) -> Option<(String, Color)> {
    if total <= 0 {
        return None;
    }
    let pct = ((used.max(0) as f64 / total as f64) * 100.0).min(100.0) as i64;
    const WIDTH: usize = 8;
    let filled = ((pct as usize * WIDTH) / 100).min(WIDTH);
    let bar = format!("{}{}", "▓".repeat(filled), "░".repeat(WIDTH - filled));
    let color = if pct >= 80 {
        Color::Red
    } else if pct >= 60 {
        Color::Yellow
    } else {
        Color::Green
    };
    Some((format!("{} {}%", bar, pct), color))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_returns_empty_when_max_zero() {
        assert_eq!(truncate_chars("hello", 0), "");
    }

    #[test]
    fn truncate_chars_returns_input_when_short_enough() {
        assert_eq!(truncate_chars("hi", 10), "hi");
        assert_eq!(truncate_chars("hello", 5), "hello");
    }

    #[test]
    fn truncate_chars_adds_ellipsis_when_truncated() {
        assert_eq!(truncate_chars("hello world", 6), "hello…");
    }

    #[test]
    fn truncate_chars_handles_multibyte_safely() {
        // 'я' is 2 bytes in UTF-8 — byte slicing would land mid-char and panic.
        let s = "яяяяяя";
        let out = truncate_chars(s, 4);
        assert_eq!(out.chars().count(), 4);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn shortcut_label_uses_digits_then_letters() {
        assert_eq!(shortcut_label(0), "1");
        assert_eq!(shortcut_label(8), "9");
        assert_eq!(shortcut_label(9), "a");
        assert_eq!(shortcut_label(34), "z");
        assert_eq!(shortcut_label(35), " ");
    }

    #[test]
    fn format_context_bar_none_when_total_zero() {
        assert!(format_context_bar(100, 0).is_none());
        assert!(format_context_bar(100, -10).is_none());
    }

    #[test]
    fn format_context_bar_color_thresholds() {
        let (_, c) = format_context_bar(0, 200_000).unwrap();
        assert_eq!(c, Color::Green);

        let (_, c) = format_context_bar(118_000, 200_000).unwrap(); // 59%
        assert_eq!(c, Color::Green);

        let (_, c) = format_context_bar(120_000, 200_000).unwrap(); // 60%
        assert_eq!(c, Color::Yellow);

        let (_, c) = format_context_bar(158_000, 200_000).unwrap(); // 79%
        assert_eq!(c, Color::Yellow);

        let (_, c) = format_context_bar(160_000, 200_000).unwrap(); // 80%
        assert_eq!(c, Color::Red);

        let (_, c) = format_context_bar(220_000, 200_000).unwrap(); // overflow clamps
        assert_eq!(c, Color::Red);
    }

    #[test]
    fn format_context_bar_renders_filled_and_empty_segments() {
        let (bar, _) = format_context_bar(100_000, 200_000).unwrap(); // 50%
        assert!(bar.starts_with("▓▓▓▓░░░░"));
        assert!(bar.ends_with("50%"));
    }

    #[test]
    fn format_context_bar_clamps_overflow_to_100() {
        let (bar, _) = format_context_bar(i64::MAX, 200_000).unwrap();
        assert!(bar.contains("100%"));
    }

    #[test]
    fn format_context_bar_clamps_negative_used_to_zero() {
        let (bar, _) = format_context_bar(-50, 200_000).unwrap();
        assert!(bar.contains("0%"));
    }
}
