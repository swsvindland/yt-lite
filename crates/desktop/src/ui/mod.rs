pub mod feed_view;
pub mod status;

use chrono::Utc;

/// "5 minutes ago", "3 days ago", "2 years ago".
pub fn format_age(published_unix: i64, now_unix: i64) -> String {
    let secs = (now_unix - published_unix).max(0);
    let (n, unit) = match secs {
        s if s < 60 => return "just now".into(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3600, "hour"),
        s if s < 7 * 86_400 => (s / 86_400, "day"),
        s if s < 30 * 86_400 => (s / (7 * 86_400), "week"),
        s if s < 365 * 86_400 => (s / (30 * 86_400), "month"),
        s => (s / (365 * 86_400), "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

pub fn now_unix() -> i64 {
    Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        assert_eq!(format_age(100, 130), "just now");
        assert_eq!(format_age(0, 60), "1 minute ago");
        assert_eq!(format_age(0, 7200), "2 hours ago");
        assert_eq!(format_age(0, 86_400 * 3), "3 days ago");
        assert_eq!(format_age(0, 86_400 * 14), "2 weeks ago");
        assert_eq!(format_age(0, 86_400 * 65), "2 months ago");
        assert_eq!(format_age(0, 86_400 * 800), "2 years ago");
        assert_eq!(format_age(500, 100), "just now");
    }
}
