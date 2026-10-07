//! Shorts detection.
//!
//! Pure decision logic lives here so it can be unit tested; the network probe
//! (`youtube.com/shorts/<id>` resolves = Short, redirects to /watch = not a
//! Short) is performed by the feed pipeline and interpreted by
//! [`interpret_probe`].
//!
//! The filter fails closed: a video is only displayed once it has a cached
//! [`Verdict::NotShort`].

/// Shorts can be up to 3 minutes. Anything at or under this many seconds is
/// borderline and must be confirmed with a probe. One second of slack covers
/// rounding in the API's ISO-8601 durations.
pub const SHORTS_MAX_SECS: u32 = 181;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Short,
    NotShort,
}

impl Verdict {
    pub fn as_db(self) -> i64 {
        match self {
            Verdict::Short => 1,
            Verdict::NotShort => 0,
        }
    }

    pub fn from_db(v: i64) -> Self {
        if v == 0 { Verdict::NotShort } else { Verdict::Short }
    }
}

/// What the feed source itself told us about the video's URL shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LinkHint {
    /// The source linked the video as `/shorts/<id>` (RSS does this today).
    ShortsLink,
    /// The source linked the video as `/watch?v=<id>`. Not trusted on its
    /// own; borderline durations are still probed.
    WatchLink,
    #[default]
    None,
}

impl LinkHint {
    pub fn from_url(url: &str) -> Self {
        if url.contains("/shorts/") {
            LinkHint::ShortsLink
        } else if url.contains("/watch") {
            LinkHint::WatchLink
        } else {
            LinkHint::None
        }
    }

    pub fn as_db(self) -> i64 {
        match self {
            LinkHint::None => 0,
            LinkHint::ShortsLink => 1,
            LinkHint::WatchLink => 2,
        }
    }

    pub fn from_db(v: i64) -> Self {
        match v {
            1 => LinkHint::ShortsLink,
            2 => LinkHint::WatchLink,
            _ => LinkHint::None,
        }
    }
}

/// Duration facts from `videos.list`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DurationInfo {
    pub secs: u32,
    /// `liveBroadcastContent` was `live` or `upcoming` (duration reads as 0).
    pub live_or_upcoming: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Final(Verdict),
    /// Borderline duration; confirm with the `/shorts/<id>` probe.
    NeedsProbe,
    /// Can't decide yet; stays hidden until enrichment fills the duration.
    NeedsDuration,
}

pub fn classify(hint: LinkHint, duration: Option<DurationInfo>) -> Decision {
    if hint == LinkHint::ShortsLink {
        return Decision::Final(Verdict::Short);
    }
    let Some(d) = duration else {
        return Decision::NeedsDuration;
    };
    if d.live_or_upcoming {
        // Streams and premieres report P0D; they are never Shorts.
        return Decision::Final(Verdict::NotShort);
    }
    if d.secs > SHORTS_MAX_SECS {
        return Decision::Final(Verdict::NotShort);
    }
    Decision::NeedsProbe
}

/// Interprets the response to a non-following request for
/// `https://www.youtube.com/shorts/<id>`. `None` means inconclusive (e.g. a
/// consent-wall redirect); the video stays hidden and is retried later.
pub fn interpret_probe(status: u16, location: Option<&str>) -> Option<Verdict> {
    match status {
        200 => Some(Verdict::Short),
        300..=399 => match location {
            Some(loc) if loc.contains("/watch") => Some(Verdict::NotShort),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dur(secs: u32) -> Option<DurationInfo> {
        Some(DurationInfo {
            secs,
            live_or_upcoming: false,
        })
    }

    #[test]
    fn shorts_link_is_final_regardless_of_duration() {
        assert_eq!(
            classify(LinkHint::ShortsLink, dur(3600)),
            Decision::Final(Verdict::Short)
        );
        assert_eq!(
            classify(LinkHint::ShortsLink, None),
            Decision::Final(Verdict::Short)
        );
    }

    #[test]
    fn long_videos_are_not_shorts() {
        assert_eq!(
            classify(LinkHint::WatchLink, dur(182)),
            Decision::Final(Verdict::NotShort)
        );
        assert_eq!(
            classify(LinkHint::None, dur(7200)),
            Decision::Final(Verdict::NotShort)
        );
    }

    #[test]
    fn borderline_durations_need_probe_even_with_watch_link() {
        for secs in [1, 30, 59, 60, 61, 120, 180, 181] {
            assert_eq!(
                classify(LinkHint::WatchLink, dur(secs)),
                Decision::NeedsProbe,
                "secs={secs}"
            );
            assert_eq!(classify(LinkHint::None, dur(secs)), Decision::NeedsProbe);
        }
    }

    #[test]
    fn unknown_duration_fails_closed() {
        assert_eq!(classify(LinkHint::WatchLink, None), Decision::NeedsDuration);
        assert_eq!(classify(LinkHint::None, None), Decision::NeedsDuration);
    }

    #[test]
    fn live_and_upcoming_are_not_shorts() {
        let live = Some(DurationInfo {
            secs: 0,
            live_or_upcoming: true,
        });
        assert_eq!(
            classify(LinkHint::WatchLink, live),
            Decision::Final(Verdict::NotShort)
        );
    }

    #[test]
    fn probe_interpretation() {
        assert_eq!(interpret_probe(200, None), Some(Verdict::Short));
        assert_eq!(
            interpret_probe(303, Some("https://www.youtube.com/watch?v=abc")),
            Some(Verdict::NotShort)
        );
        assert_eq!(
            interpret_probe(302, Some("/watch?v=abc")),
            Some(Verdict::NotShort)
        );
        // Consent wall or anything unexpected is inconclusive.
        assert_eq!(
            interpret_probe(302, Some("https://consent.youtube.com/m?continue=...")),
            None
        );
        assert_eq!(interpret_probe(303, None), None);
        assert_eq!(interpret_probe(404, None), None);
        assert_eq!(interpret_probe(429, None), None);
    }

    #[test]
    fn link_hint_parsing() {
        assert_eq!(
            LinkHint::from_url("https://www.youtube.com/shorts/R6yNUnRXZ64"),
            LinkHint::ShortsLink
        );
        assert_eq!(
            LinkHint::from_url("https://www.youtube.com/watch?v=3iRUwVzRDZQ"),
            LinkHint::WatchLink
        );
        assert_eq!(LinkHint::from_url(""), LinkHint::None);
        for h in [LinkHint::None, LinkHint::ShortsLink, LinkHint::WatchLink] {
            assert_eq!(LinkHint::from_db(h.as_db()), h);
        }
    }
}
