//! ISO-8601 duration parsing for `contentDetails.duration` (e.g. `PT1H2M3S`).

pub fn parse_iso8601(s: &str) -> Option<u32> {
    let rest = s.strip_prefix('P')?;
    let (date, time) = match rest.split_once('T') {
        Some((d, t)) => (d, t),
        None => (rest, ""),
    };
    let mut total: u64 = 0;
    total += parse_units(date, &[('W', 604_800), ('D', 86_400)])?;
    total += parse_units(time, &[('H', 3600), ('M', 60), ('S', 1)])?;
    u32::try_from(total).ok()
}

fn parse_units(part: &str, units: &[(char, u64)]) -> Option<u64> {
    let mut total = 0u64;
    let mut num = String::new();
    for c in part.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
        } else {
            let (_, mult) = units.iter().find(|(u, _)| *u == c)?;
            // Fractional seconds are truncated.
            let value: f64 = num.parse().ok()?;
            total += value as u64 * mult;
            num.clear();
        }
    }
    num.is_empty().then_some(total)
}

/// `1:02:03`, `12:34`, `0:45`.
pub fn format_clock(secs: u32) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        assert_eq!(parse_iso8601("PT45S"), Some(45));
        assert_eq!(parse_iso8601("PT3M"), Some(180));
        assert_eq!(parse_iso8601("PT1H2M3S"), Some(3723));
        assert_eq!(parse_iso8601("PT10H"), Some(36000));
        assert_eq!(parse_iso8601("P1DT2H"), Some(93600));
        assert_eq!(parse_iso8601("P0D"), Some(0));
        assert_eq!(parse_iso8601("PT1.5S"), Some(1));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_iso8601(""), None);
        assert_eq!(parse_iso8601("1H"), None);
        assert_eq!(parse_iso8601("PT5X"), None);
        assert_eq!(parse_iso8601("PT5"), None);
    }

    #[test]
    fn formats_clock() {
        assert_eq!(format_clock(45), "0:45");
        assert_eq!(format_clock(754), "12:34");
        assert_eq!(format_clock(3723), "1:02:03");
    }
}
