//! A clip's place in a take a camera split into numbered files: which
//! part it is, of how many, and which part is first.

use crate::MediaId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipSpan {
    pub first: MediaId,
    /// This part's place, from 1.
    pub index: u32,
    pub count: u32,
}

impl ClipSpan {
    /// "part 2 of 3".
    pub fn label(&self) -> String {
        format!("part {} of {}", self.index, self.count)
    }
}
