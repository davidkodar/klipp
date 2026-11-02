//! fontdue 0.9.3's line breaker, taken out of its private `unicode`
//! module unchanged (#1693): where a line of text may break, by Unicode
//! line-break classes (LineBreak 10.0.0) and a pair state machine, both
//! from xi-editor's xi-unicode. Klipp's titles break their lines by
//! shaped widths at exactly the places fontdue breaks them by its own,
//! so text whose shaped widths are fontdue's is broken exactly as
//! fontdue broke it.
//!
//! Licensed as fontdue is, MIT OR Apache-2.0 OR Zlib; the tables and
//! state machine are xi-unicode's, Apache-2.0 (see `LICENSE-*`).
#![forbid(unsafe_code)]

mod tables;
mod words;

pub use words::{complex_breaks, is_complex, join_words, spaced_words, word_starts};

use crate::tables::*;

/// A break opportunity's strength. Ordering is Hard > Soft > None.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LinebreakData {
    bits: u8,
}

pub const LINEBREAK_NONE: LinebreakData = LinebreakData::new(0b0000_0000);
pub const LINEBREAK_SOFT: LinebreakData = LinebreakData::new(0b0000_0001);
pub const LINEBREAK_HARD: LinebreakData = LinebreakData::new(0b0000_0010);

impl LinebreakData {
    const SOFT: u8 = 0b0000_0001;
    const HARD: u8 = 0b0000_0010;

    const fn new(bits: u8) -> LinebreakData {
        LinebreakData { bits }
    }

    /// The breaks a layout takes: hard ones when it wraps them, soft
    /// ones when it wraps words and has a width to wrap at.
    pub fn from_mask(wrap_soft_breaks: bool, wrap_hard_breaks: bool, has_width: bool) -> LinebreakData {
        let mut mask = 0;
        if wrap_hard_breaks {
            mask |= LinebreakData::HARD;
        }
        if wrap_soft_breaks && has_width {
            mask |= LinebreakData::SOFT;
        }
        LinebreakData { bits: mask }
    }

    pub fn is_hard(&self) -> bool {
        self.bits == LinebreakData::HARD
    }

    pub fn is_soft(&self) -> bool {
        self.bits == LinebreakData::SOFT
    }

    pub fn mask(&self, other: LinebreakData) -> LinebreakData {
        Self::new(self.bits & other.bits)
    }
}

/// Line-break state over a text, a character at a time.
#[derive(Debug, Copy, Clone, Default)]
pub struct Linebreaker {
    state: u8,
}

impl Linebreaker {
    pub fn new() -> Linebreaker {
        Linebreaker { state: 0 }
    }

    pub fn reset(&mut self) {
        self.state = 0;
    }

    /// The break before `codepoint`, given the characters before it.
    // [See LICENSE-xi-unicode] Copyright 2016 The xi-editor Authors
    pub fn next(&mut self, codepoint: char) -> LinebreakData {
        let cp = codepoint as usize;
        let lb = if cp < 0x800 {
            LINEBREAK_1_2[cp]
        } else if cp < 0x10000 {
            let child = LINEBREAK_3_ROOT[cp >> 6];
            LINEBREAK_3_CHILD[(child as usize) * 0x40 + (cp & 0x3f)]
        } else {
            let mid = LINEBREAK_4_ROOT[cp >> 12];
            let leaf = LINEBREAK_4_MID[(mid as usize) * 0x40 + ((cp >> 6) & 0x3f)];
            LINEBREAK_4_LEAVES[(leaf as usize) * 0x40 + (cp & 0x3f)]
        };
        let i = (self.state as usize) * N_LINEBREAK_CATEGORIES + (lb as usize);
        let new = LINEBREAK_STATE_MACHINE[i];
        if (new as i8) < 0 {
            self.state = new & 0x3f;
            if new >= 0xc0 {
                LINEBREAK_HARD
            } else {
                LINEBREAK_SOFT
            }
        } else {
            self.state = new;
            LINEBREAK_NONE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_break_softly_and_newlines_hard() {
        let mut lb = Linebreaker::new();
        let breaks: Vec<LinebreakData> = "ab cd\nef".chars().map(|c| lb.next(c)).collect();
        assert_eq!(breaks[3], LINEBREAK_SOFT, "before the word after a space");
        assert_eq!(breaks[6], LINEBREAK_HARD, "after a line break");
        assert!(breaks[1] == LINEBREAK_NONE && breaks[4] == LINEBREAK_NONE, "not inside a word");
    }
}
