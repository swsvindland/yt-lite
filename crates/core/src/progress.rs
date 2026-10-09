//! Playback progress: where a video resumes, and when it counts as watched.
//! Every player reports positions through [`crate::db::Db::save_progress`],
//! so they all apply the same rules.

/// Stopping before this many seconds leaves nothing to resume.
pub const MIN_RESUME_SECS: f64 = 10.0;

/// What a reported playback position means.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Progress {
    /// Barely started: nothing to resume.
    Start,
    /// Stopped partway: resume from here.
    Resume(f64),
    /// Played to (nearly) the end: watched.
    Finished,
}

impl Progress {
    /// `duration`: the player's if it knows it, else the listing's.
    pub fn at(position: f64, duration: Option<f64>) -> Self {
        let duration = duration.filter(|d| d.is_finite() && *d > 0.0);
        if let Some(d) = duration
            && position >= d - end_margin(d)
        {
            Progress::Finished
        } else if !position.is_finite() || position < MIN_RESUME_SECS {
            Progress::Start
        } else {
            Progress::Resume(position)
        }
    }
}

/// The closing stretch that counts as finished (end screens, credits,
/// outros): 5% of the video, at least 10 s and at most a minute.
fn end_margin(duration: f64) -> f64 {
    (duration * 0.05).clamp(10.0, 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_resume_and_finished() {
        let song = Some(240.0);
        assert_eq!(Progress::at(4.0, song), Progress::Start);
        assert_eq!(Progress::at(90.0, song), Progress::Resume(90.0));
        // 5% of 4 minutes is 12 s.
        assert_eq!(Progress::at(227.0, song), Progress::Resume(227.0));
        assert_eq!(Progress::at(228.0, song), Progress::Finished);
        assert_eq!(Progress::at(240.0, song), Progress::Finished);
    }

    #[test]
    fn end_margin_is_capped_for_long_videos() {
        let podcast = Some(3.0 * 3600.0);
        assert_eq!(
            Progress::at(3.0 * 3600.0 - 90.0, podcast),
            Progress::Resume(3.0 * 3600.0 - 90.0)
        );
        assert_eq!(
            Progress::at(3.0 * 3600.0 - 60.0, podcast),
            Progress::Finished
        );
        // Short videos still get 10 s.
        assert_eq!(Progress::at(51.0, Some(60.0)), Progress::Finished);
    }

    #[test]
    fn unknown_duration_never_finishes() {
        assert_eq!(Progress::at(5000.0, None), Progress::Resume(5000.0));
        assert_eq!(
            Progress::at(5000.0, Some(f64::NAN)),
            Progress::Resume(5000.0)
        );
        assert_eq!(Progress::at(f64::NAN, Some(600.0)), Progress::Start);
    }
}
