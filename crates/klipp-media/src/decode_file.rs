//! Which of a media's files is decoded: the original, its proxy or its
//! skim file. Decided here for every reader, so playback, its sound,
//! skimming and the multicam angles each ask by what they decode for,
//! and the differences between them are named rather than copied.

use std::path::{Path, PathBuf};

use crate::MediaItem;

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

/// What a file is decoded for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// The viewer's picture while it plays or is paused: the proxy when
    /// proxies are on, else the skim file when optimised playback says
    /// so, else the original.
    Picture,
    /// Playback's sound: the proxy when proxies are on, else the
    /// original. Never the skim file.
    Sound,
    /// Skimming: a still's own file, else the skim file, never the
    /// original (a media without one is still being made).
    Skim,
    /// A multicam angle's picture, asked on the UI thread for every angle
    /// each batch: no file is looked for on disk (a network share could
    /// stall it), and proxies count whether they are on or not. The skim
    /// file first, then a made proxy, then the original; a file that is
    /// gone fails to open in the worker, which then decodes the original.
    MulticamAngle,
}

/// Which of a media's files a [`FileChoice`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Original,
    Proxy,
    Skim,
}

/// The file to decode, and which it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChoice {
    pub path: PathBuf,
    pub kind: FileKind,
}

/// What the viewer's settings say about the files it may decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sources {
    /// Proxies are switched on.
    pub use_proxies: bool,
    pub optimised: OptimisedPlayback,
}

impl MediaItem {
    /// The file to decode for `purpose`; `None` only for [`Purpose::Skim`]
    /// on a media whose skim file is not there yet.
    pub fn file_for(&self, purpose: Purpose, sources: Sources) -> Option<FileChoice> {
        self.file_for_with(purpose, sources, &|p: &Path| p.exists())
    }

    /// The proxy the viewer decodes in place of the original when proxies
    /// are on (`use_proxies`): made, and its file there.
    pub fn proxy_in_use(&self, use_proxies: bool) -> Option<&Path> {
        self.proxy_in_use_with(use_proxies, &|p: &Path| p.exists())
    }

    fn proxy_in_use_with(&self, use_proxies: bool, there: &dyn Fn(&Path) -> bool) -> Option<&Path> {
        self.proxy_path.as_deref().filter(|p| use_proxies && self.proxy_ready && there(p))
    }

    fn file_for_with(&self, purpose: Purpose, sources: Sources, there: &dyn Fn(&Path) -> bool) -> Option<FileChoice> {
        let choice = |path: &Path, kind| Some(FileChoice { path: path.to_path_buf(), kind });
        let original = || choice(&self.path, FileKind::Original);
        match purpose {
            Purpose::Picture => {
                if let Some(proxy) = self.proxy_in_use_with(sources.use_proxies, there) {
                    return choice(proxy, FileKind::Proxy);
                }
                match self.skim_file.as_deref() {
                    Some(skim) if sources.optimised.plays_skim(self.resolution.height, self.fps.as_f64(), there(skim)) => choice(skim, FileKind::Skim),
                    _ => original(),
                }
            }
            Purpose::Sound => match self.proxy_in_use_with(sources.use_proxies, there) {
                Some(proxy) => choice(proxy, FileKind::Proxy),
                None => original(),
            },
            Purpose::Skim if self.is_still => original(),
            Purpose::Skim => self.skim_file.as_deref().filter(|p| there(p)).and_then(|p| choice(p, FileKind::Skim)),
            Purpose::MulticamAngle => match (self.skim_file.as_deref(), self.proxy_path.as_deref().filter(|_| self.proxy_ready)) {
                (Some(skim), _) => choice(skim, FileKind::Skim),
                (None, Some(proxy)) => choice(proxy, FileKind::Proxy),
                (None, None) => original(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::Fps;

    fn item(height: u32, fps: f64) -> MediaItem {
        let mut m = MediaItem::synthetic_for_test(PathBuf::from("/v/a.mov"), Some(PROXY));
        m.resolution.height = height;
        m.resolution.width = height * 16 / 9;
        m.fps = Fps::new((fps * 1000.0).round() as u32, 1000);
        m.proxy_ready = true;
        m.skim_file = Some("/cache/a.skim.mp4".into());
        m
    }

    fn kind(m: &MediaItem, purpose: Purpose, sources: Sources, there: &[&str]) -> Option<FileKind> {
        m.file_for_with(purpose, sources, &|p: &Path| there.iter().any(|t| Path::new(t) == p)).map(|c| c.kind)
    }

    const PROXY: &str = "/v/a.proxy.mov";
    const SKIM: &str = "/cache/a.skim.mp4";

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

    /// The picture is the proxy when proxies are on and it is made and
    /// there; else the skim file when optimised playback says so; else the
    /// original.
    #[test]
    fn the_picture_is_the_proxy_then_the_skim_file_then_the_original() {
        let heavy = item(2160, 60.0);
        let on = Sources { use_proxies: true, optimised: OptimisedPlayback::Auto };
        let off = Sources { use_proxies: false, ..on };
        assert_eq!(kind(&heavy, Purpose::Picture, on, &[PROXY, SKIM]), Some(FileKind::Proxy));
        assert_eq!(kind(&heavy, Purpose::Picture, off, &[PROXY, SKIM]), Some(FileKind::Skim), "proxies off");
        assert_eq!(kind(&heavy, Purpose::Picture, on, &[SKIM]), Some(FileKind::Skim), "proxy file gone");
        assert_eq!(kind(&heavy, Purpose::Picture, on, &[]), Some(FileKind::Original), "neither there");
        let mut unmade = heavy.clone();
        unmade.proxy_ready = false;
        assert_eq!(kind(&unmade, Purpose::Picture, on, &[PROXY, SKIM]), Some(FileKind::Skim), "proxy not made");
        assert_eq!(kind(&item(1080, 25.0), Purpose::Picture, off, &[SKIM]), Some(FileKind::Original), "light enough to play");
        let original = Sources { optimised: OptimisedPlayback::AlwaysOriginal, ..off };
        assert_eq!(kind(&heavy, Purpose::Picture, original, &[PROXY, SKIM]), Some(FileKind::Original));
        assert_eq!(heavy.file_for_with(Purpose::Picture, on, &|_| true).unwrap().path, Path::new(PROXY));
    }

    /// Sound plays from the proxy as the picture does, never from the skim
    /// file, whatever optimised playback says.
    #[test]
    fn sound_is_the_proxy_or_the_original_never_the_skim_file() {
        let heavy = item(2160, 60.0);
        let on = Sources { use_proxies: true, optimised: OptimisedPlayback::AlwaysOptimised };
        assert_eq!(kind(&heavy, Purpose::Sound, on, &[PROXY, SKIM]), Some(FileKind::Proxy));
        assert_eq!(kind(&heavy, Purpose::Sound, on, &[SKIM]), Some(FileKind::Original));
        assert_eq!(kind(&heavy, Purpose::Sound, Sources { use_proxies: false, ..on }, &[PROXY, SKIM]), Some(FileKind::Original));
    }

    /// Skimming reads the skim file, or a still's own file, and nothing
    /// while the skim file is still being made.
    #[test]
    fn skimming_reads_the_skim_file_or_a_stills_own() {
        let m = item(1080, 25.0);
        let any = Sources::default();
        assert_eq!(kind(&m, Purpose::Skim, any, &[SKIM]), Some(FileKind::Skim));
        assert_eq!(kind(&m, Purpose::Skim, any, &[PROXY]), None, "not made yet");
        let mut still = m.clone();
        still.is_still = true;
        assert_eq!(kind(&still, Purpose::Skim, any, &[]), Some(FileKind::Original));
    }

    /// A multicam angle looks for no file: the skim file if it has one,
    /// else a made proxy whether proxies are on or not, else the original.
    #[test]
    fn a_multicam_angle_chooses_without_looking_on_disk() {
        let mut m = item(1080, 25.0);
        let off = Sources::default();
        assert_eq!(kind(&m, Purpose::MulticamAngle, off, &[]), Some(FileKind::Skim));
        m.skim_file = None;
        assert_eq!(kind(&m, Purpose::MulticamAngle, off, &[]), Some(FileKind::Proxy));
        m.proxy_ready = false;
        assert_eq!(kind(&m, Purpose::MulticamAngle, off, &[PROXY]), Some(FileKind::Original));
    }
}
