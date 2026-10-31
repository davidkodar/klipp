//! Phosphor icon library integration for Klipp.
//!
//! Call [`install`] once at startup, then use [`Icon`] variants
//! with `.image()` / `.image()` to get tintable `egui::Image` widgets.

/// Register SVG image loaders with egui. Call once in app init.
pub fn install(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
}

macro_rules! define_icons {
    ($($variant:ident => $name:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon {
            $($variant,)*
        }

        impl Icon {
            /// Regular-weight SVG as an `egui::Image`, sized to `size × size`.
            pub fn image(self, size: f32) -> egui::Image<'static> {
                let (uri, bytes) = match self {
                    $(Icon::$variant => (
                        concat!("bytes://icons/regular/", $name, ".svg"),
                        &include_bytes!(concat!("../../../assets/icons/regular/", $name, ".svg"))[..],
                    ),)*
                };
                egui::Image::from_bytes(uri, bytes)
                    .fit_to_exact_size(egui::vec2(size, size))
            }

            /// Bold-weight SVG as an `egui::Image`, sized to `size × size`.
            pub fn bold(self, size: f32) -> egui::Image<'static> {
                let (uri, bytes) = match self {
                    $(Icon::$variant => (
                        concat!("bytes://icons/bold/", $name, "-bold.svg"),
                        &include_bytes!(concat!("../../../assets/icons/bold/", $name, "-bold.svg"))[..],
                    ),)*
                };
                egui::Image::from_bytes(uri, bytes)
                    .fit_to_exact_size(egui::vec2(size, size))
            }
        }
    };
}

define_icons! {
    // Playback / transport
    Play            => "play",
    Pause           => "pause",
    Stop            => "stop",
    SkipBack        => "skip-back",
    SkipForward     => "skip-forward",
    Rewind          => "rewind",
    FastForward     => "fast-forward",
    CaretLeft       => "caret-left",
    CaretRight      => "caret-right",
    CaretUp         => "caret-up",
    CaretDown       => "caret-down",

    // Timeline / editing
    Scissors        => "scissors",
    Cursor          => "cursor",
    Hand            => "hand",
    MagnifyingGlass => "magnifying-glass",
    ArrowsOut       => "arrows-out",
    ArrowCounterClockwise => "arrow-counter-clockwise",
    ArrowClockwise  => "arrow-clockwise",
    FrameCorners    => "frame-corners",

    // Color panel
    Eyedropper      => "eyedropper",
    Sliders         => "sliders",
    Equalizer       => "equalizer",

    // Media / files
    Folder          => "folder",
    FilmStrip       => "film-strip",
    FilmSlate       => "film-slate",
    MusicNote       => "music-note",
    Waveform        => "waveform",

    // Export / render
    Export          => "export",
    UploadSimple    => "upload-simple",
    Spinner         => "spinner",

    // UI chrome
    X               => "x",
    XCircle         => "x-circle",
    Plus            => "plus",
    Minus           => "minus",
    Gear            => "gear",
    Lock            => "lock",
    Snowflake       => "snowflake",
    LockOpen        => "lock-open",
    Eye             => "eye",
    EyeSlash        => "eye-slash",
    Warning         => "warning",
    Info            => "info",
    Check           => "check",
    CheckCircle     => "check-circle",

    // Audio
    SpeakerHigh     => "speaker-high",
    SpeakerSlash    => "speaker-slash",

    // Browser
    List            => "list",
    GridFour        => "grid-four",
    SortAscending   => "sort-ascending",
    SortDescending  => "sort-descending",
    Star            => "star",

    // Inspector
    Diamond         => "diamond",
    TextAlignLeft   => "text-align-left",
    TextAlignCenter => "text-align-center",
    TextAlignRight  => "text-align-right",
    TextAlignJustify => "text-align-justify",

    // Browser (cont.)
    PushPin         => "push-pin",

    // Misc
    Lightning       => "lightning",
}
