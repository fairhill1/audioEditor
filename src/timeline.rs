use crate::app::{App, SelectionEdge, SELECTION_EDGE_PX};
use crate::audio;

impl App {
    /// Hit-test: find which (track_idx, clip_idx) is at pixel position (px, py)
    pub(crate) fn hit_test_clip(&self, px: f64, py: f64) -> Option<(usize, usize)> {
        if self.tracks.is_empty() {
            return None;
        }
        let sidebar = self.sidebar_width_px() as f64;
        if px < sidebar {
            return None;
        }
        let config = self.config.as_ref()?;
        let num_tracks = self.tracks.len();
        let lane_height_px = config.height as f64 / num_tracks as f64;
        let track_idx = (py / lane_height_px) as usize;
        if track_idx >= num_tracks {
            return None;
        }

        let cursor_secs = self.px_to_secs(px);

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
        let sidebar = self.sidebar_width_px() as f64;
        if px < sidebar {
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

        let cursor_secs = self.px_to_secs(px);

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
        let sidebar = self.sidebar_width_px() as f64;
        let content_w = self.content_width() as f64;
        if px < sidebar || content_w <= 0.0 {
            return self.view_start;
        }
        self.view_start + ((px - sidebar) / content_w) * self.effective_view_duration()
    }

    /// Convert seconds to pixel X position
    pub(crate) fn secs_to_px(&self, secs: f64) -> f64 {
        let sidebar = self.sidebar_width_px() as f64;
        let content_w = self.content_width() as f64;
        (secs - self.view_start) / self.effective_view_duration() * content_w + sidebar
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

    /// Find all clip indices in a track that overlap the time range [s0, s1).
    pub(crate) fn clips_overlapping_range(&self, track_idx: usize, s0: f64, s1: f64) -> Vec<usize> {
        if track_idx >= self.tracks.len() {
            return Vec::new();
        }
        self.tracks[track_idx]
            .clips
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                let c_end = c.offset_secs + c.duration_secs();
                s0 < c_end && s1 > c.offset_secs
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Copy the region [s0, s1) from all overlapping clips in a track.
    /// Each returned clip is sliced to the selection bounds with offset_secs
    /// relative to s0 (i.e. the first clip starts at 0.0 or later).
    pub(crate) fn copy_region_clips(&self, track_idx: usize, s0: f64, s1: f64) -> Vec<audio::Clip> {
        let indices = self.clips_overlapping_range(track_idx, s0, s1);
        let mut result = Vec::new();
        for idx in indices {
            let clip = &self.tracks[track_idx].clips[idx];
            let rel_start = (s0 - clip.offset_secs).max(0.0);
            let rel_end = (s1 - clip.offset_secs).min(clip.duration_secs());
            if rel_end > rel_start {
                let mut sliced = clip.slice(rel_start, rel_end);
                // Make offset relative to s0
                sliced.offset_secs = (clip.offset_secs + rel_start) - s0;
                result.push(sliced);
            }
        }
        result
    }

    /// Remove the time range [s0, s1) from all overlapping clips in a track.
    /// Processes indices in reverse to avoid invalidation.
    pub(crate) fn remove_region_from_track(&mut self, track_idx: usize, s0: f64, s1: f64) {
        let mut indices = self.clips_overlapping_range(track_idx, s0, s1);
        indices.sort_unstable();
        // Process in reverse order so indices stay valid
        for &idx in indices.iter().rev() {
            let clip = &self.tracks[track_idx].clips[idx];
            let rel_start = (s0 - clip.offset_secs).max(0.0);
            let rel_end = (s1 - clip.offset_secs).min(clip.duration_secs());
            if rel_end <= rel_start {
                continue;
            }
            let (left, right) = clip.remove_region(rel_start, rel_end);
            self.tracks[track_idx].clips.remove(idx);
            let mut insert_at = idx;
            if let Some(l) = left {
                self.tracks[track_idx].clips.insert(insert_at, l);
                insert_at += 1;
            }
            if let Some(r) = right {
                self.tracks[track_idx].clips.insert(insert_at, r);
            }
        }
    }

    /// Snap a group's outer edges against non-group clip edges.
    /// `skip_clips` are the clip indices in the group.
    /// `desired_left` is the desired offset of the leftmost group clip.
    /// `group_span` is the distance from leftmost to rightmost+duration.
    pub(crate) fn snap_offset_group(
        &self, track_idx: usize, skip_clips: &[usize],
        desired_left: f64, group_span: f64,
    ) -> f64 {
        let config = match self.config.as_ref() {
            Some(c) => c,
            None => return desired_left,
        };
        let view_duration = self.effective_view_duration();
        let snap_secs = 10.0 / config.width as f64 * view_duration;

        let group_start = desired_left;
        let group_end = desired_left + group_span;

        let mut best_offset = desired_left;
        let mut best_dist = f64::MAX;

        let track = &self.tracks[track_idx];
        for (i, other) in track.clips.iter().enumerate() {
            if skip_clips.contains(&i) {
                continue;
            }
            let other_start = other.offset_secs;
            let other_end = other.offset_secs + other.duration_secs();

            // Group left edge snaps to other right edge
            let d = (group_start - other_end).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_end;
            }

            // Group right edge snaps to other left edge
            let d = (group_end - other_start).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best_offset = other_start - group_span;
            }
        }

        best_offset
    }

    /// Clamp delta so no group clip overlaps a non-group clip.
    /// Returns the clamped left offset for the group.
    pub(crate) fn clamp_group_no_overlap(
        &self, track_idx: usize, group_indices: &[usize],
        group_orig_offsets: &[f64], desired_left: f64, orig_group_left: f64,
    ) -> f64 {
        let delta = desired_left - orig_group_left;
        let track = &self.tracks[track_idx];

        // Collect non-group clip intervals
        let others: Vec<(f64, f64)> = track.clips.iter().enumerate()
            .filter(|(i, _)| !group_indices.contains(i))
            .map(|(_, c)| (c.offset_secs, c.offset_secs + c.duration_secs()))
            .collect();

        if others.is_empty() {
            return desired_left.max(0.0 + (desired_left - orig_group_left - delta) + delta);
        }

        // Check if any group clip would overlap a non-group clip at this delta
        let mut clamped_delta = delta;

        // Try the desired delta, then clamp if overlapping
        for (gi, &orig_off) in group_orig_offsets.iter().enumerate() {
            let clip_idx = group_indices[gi];
            let clip_dur = track.clips[clip_idx].duration_secs();
            let new_start = orig_off + clamped_delta;
            let new_end = new_start + clip_dur;

            for &(os, oe) in &others {
                if new_start < oe && new_end > os {
                    // Overlap detected — clamp delta
                    if clamped_delta > 0.0 {
                        // Moving right: clamp so new_start = oe or new_end = os
                        // We want to limit delta so that none of the group clips overlap
                        let max_delta = (os - (orig_off + clip_dur)).min(clamped_delta);
                        if max_delta < clamped_delta && max_delta >= 0.0 {
                            clamped_delta = max_delta;
                        } else if max_delta < 0.0 {
                            clamped_delta = 0.0;
                        }
                    } else {
                        // Moving left: clamp so new_end = os or new_start = oe
                        let min_delta = (oe - orig_off).max(clamped_delta);
                        if min_delta > clamped_delta && min_delta <= 0.0 {
                            clamped_delta = min_delta;
                        } else if min_delta > 0.0 {
                            clamped_delta = 0.0;
                        }
                    }
                }
            }
        }

        // Also clamp so no clip goes below 0
        for &orig_off in group_orig_offsets {
            if orig_off + clamped_delta < 0.0 {
                clamped_delta = -orig_off;
            }
        }

        orig_group_left + clamped_delta
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

    /// Snap a time value to the nearest clip edge (start or end) in a track.
    /// Returns the snapped value if within threshold, otherwise the original.
    pub(crate) fn snap_to_clip_edges(&self, track_idx: usize, secs: f64) -> f64 {
        let config = match self.config.as_ref() {
            Some(c) => c,
            None => return secs,
        };
        if track_idx >= self.tracks.len() {
            return secs;
        }
        let view_duration = self.effective_view_duration();
        let snap_secs = 10.0 / config.width as f64 * view_duration;

        let mut best = secs;
        let mut best_dist = f64::MAX;

        for clip in &self.tracks[track_idx].clips {
            let start = clip.offset_secs;
            let end = clip.offset_secs + clip.duration_secs();

            let d = (secs - start).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best = start;
            }

            let d = (secs - end).abs();
            if d < snap_secs && d < best_dist {
                best_dist = d;
                best = end;
            }
        }

        best
    }
}
