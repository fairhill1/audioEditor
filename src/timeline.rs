use crate::app::{App, SelectionEdge, SELECTION_EDGE_PX};

impl App {
    /// Hit-test: find which (track_idx, clip_idx) is at pixel position (px, py)
    pub(crate) fn hit_test_clip(&self, px: f64, py: f64) -> Option<(usize, usize)> {
        if self.tracks.is_empty() {
            return None;
        }
        let config = self.config.as_ref()?;
        let num_tracks = self.tracks.len();
        let lane_height_px = config.height as f64 / num_tracks as f64;
        let track_idx = (py / lane_height_px) as usize;
        if track_idx >= num_tracks {
            return None;
        }

        let view_start = self.view_start;
        let view_duration = self.effective_view_duration();
        let cursor_secs = view_start + (px / config.width as f64) * view_duration;

        let track = &self.tracks[track_idx];
        for (clip_idx, clip) in track.clips.iter().enumerate() {
            let clip_start = clip.offset_secs;
            let clip_end = clip.offset_secs + clip.duration_secs();
            if cursor_secs >= clip_start && cursor_secs <= clip_end {
                return Some((track_idx, clip_idx));
            }
        }
        None
    }

    /// Hit-test: check if pixel position (px, py) is in a clip's title bar
    pub(crate) fn hit_test_title_bar(&self, px: f64, py: f64) -> Option<(usize, usize)> {
        if self.tracks.is_empty() {
            return None;
        }
        let config = self.config.as_ref()?;
        let num_tracks = self.tracks.len();
        let lane_height_px = config.height as f64 / num_tracks as f64;
        let track_idx = (py / lane_height_px) as usize;
        if track_idx >= num_tracks {
            return None;
        }

        let lane_top_px = track_idx as f64 * lane_height_px;
        let title_bar_physical = Self::TITLE_BAR_LP as f64 * self.scale_factor() as f64;
        if py - lane_top_px >= title_bar_physical {
            return None;
        }

        let view_start = self.view_start;
        let view_duration = self.effective_view_duration();
        let cursor_secs = view_start + (px / config.width as f64) * view_duration;

        let track = &self.tracks[track_idx];
        for (clip_idx, clip) in track.clips.iter().enumerate() {
            let clip_start = clip.offset_secs;
            let clip_end = clip.offset_secs + clip.duration_secs();
            if cursor_secs >= clip_start && cursor_secs <= clip_end {
                return Some((track_idx, clip_idx));
            }
        }
        None
    }

    /// Convert a pixel X position to seconds on the timeline
    pub(crate) fn px_to_secs(&self, px: f64) -> f64 {
        let config = self.config.as_ref().unwrap();
        self.view_start + (px / config.width as f64) * self.effective_view_duration()
    }

    /// Convert seconds to pixel X position
    pub(crate) fn secs_to_px(&self, secs: f64) -> f64 {
        let config = self.config.as_ref().unwrap();
        (secs - self.view_start) / self.effective_view_duration() * config.width as f64
    }

    /// Check if a pixel X is near a selection edge, returning which edge
    pub(crate) fn hit_test_selection_edge(&self, px: f64) -> Option<SelectionEdge> {
        let (s0, s1) = self.selection?;
        let left_px = self.secs_to_px(s0);
        let right_px = self.secs_to_px(s1);
        let threshold = SELECTION_EDGE_PX;
        // Prefer whichever edge is closer
        let d_left = (px - left_px).abs();
        let d_right = (px - right_px).abs();
        if d_left <= threshold && d_left <= d_right {
            Some(SelectionEdge::Left)
        } else if d_right <= threshold {
            Some(SelectionEdge::Right)
        } else {
            None
        }
    }

    /// Snap a clip's offset to nearby clip edges in the same track.
    /// Returns the snapped offset, or the original if no snap applies.
    pub(crate) fn snap_offset(&self, track_idx: usize, skip_clip: usize, desired: f64, clip_dur: f64) -> f64 {
        let config = match self.config.as_ref() {
            Some(c) => c,
            None => return desired,
        };
        let view_duration = self.effective_view_duration();
        let snap_secs = 10.0 / config.width as f64 * view_duration;

        let clip_start = desired;
        let clip_end = desired + clip_dur;

        let mut best_offset = desired;
        let mut best_dist = f64::MAX;

        let track = &self.tracks[track_idx];
        for (i, other) in track.clips.iter().enumerate() {
            if i == skip_clip {
                continue;
            }
            let other_start = other.offset_secs;
            let other_end = other.offset_secs + other.duration_secs();

            // Dragged clip start snaps to other clip end
            let d = (clip_start - other_end).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_end;
            }

            // Dragged clip end snaps to other clip start
            let d = (clip_end - other_start).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_start - clip_dur;
            }

            // Dragged clip start snaps to other clip start
            let d = (clip_start - other_start).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_start;
            }

            // Dragged clip end snaps to other clip end
            let d = (clip_end - other_end).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_end - clip_dur;
            }
        }

        best_offset
    }

    /// Clamp a clip's offset so it doesn't overlap any other clip in the track.
    /// Returns the nearest valid offset.
    pub(crate) fn clamp_no_overlap(&self, track_idx: usize, skip_clip: usize, desired: f64, clip_dur: f64) -> f64 {
        let track = &self.tracks[track_idx];

        // Collect all other clips' intervals, sorted by start time
        let mut intervals: Vec<(f64, f64)> = track
            .clips
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != skip_clip)
            .map(|(_, c)| (c.offset_secs, c.offset_secs + c.duration_secs()))
            .collect();
        intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        if intervals.is_empty() {
            return desired.max(0.0);
        }

        // Build list of gaps where the clip could fit
        // Gap before first clip
        let mut candidates: Vec<f64> = Vec::new();

        // Before first interval
        if clip_dur <= intervals[0].0 {
            // Clip fits before the first clip
            let clamped = desired.clamp(0.0, intervals[0].0 - clip_dur);
            candidates.push(clamped);
        } else if 0.0 + clip_dur <= intervals[0].0 + 1e-9 {
            candidates.push(0.0);
        }

        // Gaps between intervals
        for i in 0..intervals.len() - 1 {
            let gap_start = intervals[i].1;
            let gap_end = intervals[i + 1].0;
            let gap_size = gap_end - gap_start;
            if gap_size >= clip_dur - 1e-9 {
                let clamped = desired.clamp(gap_start, gap_end - clip_dur);
                candidates.push(clamped);
            }
        }

        // After last interval
        let last_end = intervals.last().unwrap().1;
        let clamped = desired.max(last_end);
        candidates.push(clamped);

        // Also consider placing at time 0 if there's room
        if intervals[0].0 >= clip_dur && !candidates.iter().any(|&c| c < 1e-9) {
            candidates.push(0.0);
        }

        // Pick the candidate nearest to desired
        candidates
            .into_iter()
            .min_by(|a, b| {
                let da = (*a - desired).abs();
                let db = (*b - desired).abs();
                da.partial_cmp(&db).unwrap()
            })
            .unwrap_or(desired.max(0.0))
    }
}
