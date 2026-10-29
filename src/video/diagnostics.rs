//! Worker-local timing; snapshots are emitted only when the decoder closes.
use std::prelude::v1::*;

#[derive(Debug, Default)]
pub(super) struct Profile {
    pub polls: u64,
    pub pictures: u64,
    pub get_us: u64,
    pub copy_us: u64,
    pub first_pts_ms: Option<u64>,
    pub last_pts_ms: Option<u64>,
    pub max_pts_gap_ms: u64,
    pub nonincreasing_pts: u64,
    pub stride: usize,
    pub rows: usize,
    pub pauses: u64,
    pub resumes: u64,
    pub paused_us: u64,
    paused_since: Option<u64>,
}
impl Profile {
    pub fn picture(&mut self, pts: u64, stride: usize, rows: usize) {
        self.pictures += 1;
        self.first_pts_ms.get_or_insert(pts);
        if let Some(last) = self.last_pts_ms {
            self.max_pts_gap_ms = self.max_pts_gap_ms.max(pts.saturating_sub(last));
            self.nonincreasing_pts += u64::from(pts <= last);
        }
        self.last_pts_ms = Some(pts);
        self.stride = stride;
        self.rows = rows;
    }
    pub fn pause(&mut self, paused: bool, now: u64) {
        if paused {
            self.pauses += 1;
            self.paused_since = Some(now);
        } else {
            self.resumes += 1;
            self.finish(now);
        }
    }
    pub fn finish(&mut self, now: u64) {
        if let Some(started) = self.paused_since.take() {
            self.paused_us += now.saturating_sub(started);
        }
    }
}
