//! How the viewer's decoders decode: the user's playback preferences as
//! one value, handed to the engine and given to each decoder as it opens,
//! in place of process-wide switches a test or a second engine shared.

/// The decoders' settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeSettings {
    /// Hardware decode (NVDEC, VAAPI) may be used.
    pub hardware: bool,
    /// The tallest picture playback decodes at ("Max playback
    /// resolution"); larger frames are fitted into a box this tall and
    /// twice as wide, turned with the picture.
    pub max_height: u32,
    /// The frames a decoder holds decoded ahead; never below 4.
    pub queue_frames: usize,
    /// Threads a software decoder uses; 0 lets ffmpeg choose.
    pub threads: u32,
    /// When playback decodes a skim file in place of the original.
    pub optimised: OptimisedPlayback,
}

impl Default for DecodeSettings {
    fn default() -> Self {
        Self { hardware: true, max_height: 1080, queue_frames: 16, threads: 0, optimised: OptimisedPlayback::Auto }
    }
}

impl DecodeSettings {
    /// The settings with `queue_frames` kept at 4 or more, as a decoder
    /// holds them.
    pub fn new(hardware: bool, max_height: u32, queue_frames: u32, threads: u32, optimised: OptimisedPlayback) -> Self {
        Self { hardware, max_height, queue_frames: queue_frames.max(4) as usize, threads, optimised }
    }
}

/// When playback decodes a media's skim file in place of the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptimisedPlayback {
    /// The skim file for a source too heavy to play as it is: taller than
    /// 1080 lines, or faster than 30 frames a second.
    #[default]
    Auto,
    /// Never; the original is always played.
    AlwaysOriginal,
    /// Whenever the skim file is there.
    AlwaysOptimised,
}

impl OptimisedPlayback {
    /// From the preference's number: 1 always original, 2 always
    /// optimised, anything else Auto, so a value from a later version
    /// still plays.
    pub fn from_u8(mode: u8) -> Self {
        match mode {
            1 => Self::AlwaysOriginal,
            2 => Self::AlwaysOptimised,
            _ => Self::Auto,
        }
    }

    /// The preference's number for it.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::AlwaysOriginal => 1,
            Self::AlwaysOptimised => 2,
        }
    }

    /// Whether playback decodes the skim file for a source of `height`
    /// lines at `fps`, when the skim file is there (`skim_there`).
    pub fn plays_skim(self, height: u32, fps: f64, skim_there: bool) -> bool {
        match self {
            Self::AlwaysOriginal => false,
            Self::AlwaysOptimised => skim_there,
            // Never while the skim file is still being made: play is not
            // held back for it.
            Self::Auto => skim_there && (height > 1080 || fps > 30.5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The skim file plays in Auto for a source taller than 1080 lines or
    /// faster than 30 frames, once it is there; never in Always original;
    /// whenever it is there in Always optimised; and any other number of
    /// the preference reads as Auto.
    #[test]
    fn optimised_playback_plays_the_skim_file_for_heavy_sources_only_when_it_is_there() {
        use OptimisedPlayback::*;
        for (height, fps, heavy) in [(2160, 60.0, true), (2160, 30.0, true), (1080, 60.0, true), (1080, 50.0, true), (1080, 59.94, true), (1080, 30.9, true), (1080, 30.0, false), (1080, 29.97, false), (720, 23.976, false)] {
            assert_eq!(Auto.plays_skim(height, fps, true), heavy, "{height}p{fps}");
            assert!(!Auto.plays_skim(height, fps, false), "not before it is made");
            assert!(!AlwaysOriginal.plays_skim(height, fps, true));
            assert!(AlwaysOptimised.plays_skim(height, fps, true) && !AlwaysOptimised.plays_skim(height, fps, false));
        }
        for mode in [0, 1, 2] {
            assert_eq!(OptimisedPlayback::from_u8(mode).as_u8(), mode);
        }
        assert_eq!(OptimisedPlayback::from_u8(99), Auto);
    }

    /// A queue of fewer than four frames is held as four.
    #[test]
    fn a_decoder_holds_at_least_four_frames() {
        assert_eq!(DecodeSettings::new(true, 1080, 1, 0, OptimisedPlayback::Auto).queue_frames, 4);
        assert_eq!(DecodeSettings::new(true, 1080, 24, 0, OptimisedPlayback::Auto).queue_frames, 24);
        assert_eq!(DecodeSettings::default(), DecodeSettings::new(true, 1080, 16, 0, OptimisedPlayback::Auto));
    }
}
