//! Smart downloads' arithmetic: how big a download will be before it's
//! resolved, and which smart downloads to delete to make room for one that's
//! more likely to be watched.

/// Roughly how many bytes a download of `duration_secs` takes: YouTube's
/// typical H.264 bitrates at `max_height` (they vary with the content), plus
/// 128 kbps AAC.
pub fn estimate_bytes(duration_secs: u32, max_height: u32, audio_only: bool) -> u64 {
    const AUDIO: u64 = 16_000;
    let video = match max_height {
        _ if audio_only => 0,
        0..=360 => 40_000,
        361..=480 => 70_000,
        481..=720 => 140_000,
        _ => 280_000,
    };
    u64::from(duration_secs) * (video + AUDIO)
}

/// A finished smart download.
#[derive(Clone, Debug, PartialEq)]
pub struct Kept {
    pub id: String,
    pub bytes: u64,
    /// Its place among today's candidates (0 is the most wanted); `None` if
    /// it isn't one any more.
    pub rank: Option<usize>,
    pub completed_at: i64,
}

/// The smart downloads to delete so `need` more bytes fit in `budget` with
/// `used` already taken, least wanted first: ones that are no longer
/// candidates (oldest first), then candidates from the bottom up. With
/// `rank`, only candidates ranked below it go. `None` if that can't make
/// enough room.
pub fn make_room(
    kept: &[Kept],
    used: u64,
    need: u64,
    budget: u64,
    rank: Option<usize>,
) -> Option<Vec<String>> {
    let mut victims: Vec<&Kept> = kept
        .iter()
        .filter(|k| match (k.rank, rank) {
            (None, _) | (_, None) => true,
            (Some(theirs), Some(ours)) => theirs > ours,
        })
        .collect();
    victims.sort_by_key(|k| match k.rank {
        None => (0, k.completed_at),
        Some(r) => (1, -(r as i64)),
    });
    let mut used = used;
    let mut out = Vec::new();
    for v in victims {
        if used + need <= budget {
            break;
        }
        used = used.saturating_sub(v.bytes);
        out.push(v.id.clone());
    }
    (used + need <= budget).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_grow_with_quality() {
        let ten_minutes = 600;
        let audio = estimate_bytes(ten_minutes, 1080, true);
        assert_eq!(audio, 9_600_000);
        let sd = estimate_bytes(ten_minutes, 480, false);
        let hd = estimate_bytes(ten_minutes, 720, false);
        let full = estimate_bytes(ten_minutes, 1080, false);
        assert!(audio < sd && sd < hd && hd < full);
        // A 10-minute 720p video, as measured: 78 MB.
        assert!((70_000_000..110_000_000).contains(&hd), "{hd}");
    }

    fn kept(id: &str, bytes: u64, rank: Option<usize>, completed_at: i64) -> Kept {
        Kept {
            id: id.into(),
            bytes,
            rank,
            completed_at,
        }
    }

    #[test]
    fn room_comes_from_the_least_wanted() {
        let all = [
            kept("top", 100, Some(0), 5),
            kept("gone-new", 100, None, 9),
            kept("mid", 100, Some(3), 5),
            kept("gone-old", 100, None, 1),
            kept("low", 100, Some(7), 5),
        ];
        // Fits already.
        assert_eq!(make_room(&all, 500, 100, 600, Some(5)), Some(vec![]));
        // Old leftovers first, then newer ones, then the lowest-ranked.
        assert_eq!(
            make_room(&all, 500, 250, 600, Some(5)).unwrap(),
            ["gone-old", "gone-new"]
        );
        assert_eq!(
            make_room(&all, 500, 350, 600, Some(2)).unwrap(),
            ["gone-old", "gone-new", "low"]
        );
        // Never something more wanted than what it's for.
        assert_eq!(make_room(&all, 500, 450, 600, Some(4)), None);
        assert_eq!(make_room(&all, 500, 450, 600, Some(2)).unwrap().len(), 4);
        // Over a smaller budget: anything can go, least wanted first.
        assert_eq!(
            make_room(&all, 500, 0, 150, None).unwrap(),
            ["gone-old", "gone-new", "low", "mid"]
        );
        // Bigger than the whole budget.
        assert_eq!(make_room(&all, 500, 700, 600, None), None);
    }
}
