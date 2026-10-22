//! Hyperzoom transition preset planning.
//!
//! Pure-math helper that turns user-supplied focal point + duration +
//! peak scale into the per-clip Transform anchor and scale-keyframe set
//! that produces a "fly into the focal point" transition. The handler
//! in `app.rs` consumes the plan and writes it to the source and target
//! clips through the existing transform-keyframe machinery —
//! hyperzoom is a *preset*, not a new effect type, so the result is
//! fully editable by the user in the keyframe editor afterwards.
//!
//! Easing is currently `Linear`; the keyframe editor surfaces Bezier
//! handles per-keyframe so the user can refine the ramp by hand. A
//! future enhancement could emit Bezier tangents up-front for a more
//! professional default feel.
//!
//! The visual letter-picker (click a glyph in the title to set the
//! focal point) is also a deferred follow-up; today the handler takes
//! normalised X/Y in `0.0..=1.0` from the inspector spinboxes, with
//! `(0.5, 0.5)` defaulting to the centre of the source clip.

use klipp_core::keyframe::{Interpolation, Keyframe};
use klipp_core::ClipId;
use klipp_timeline::{ClipKind, Sequence};

/// Per-clip persistent state for the HYPERZOOM inspector section AND
/// the on-canvas focal-point gizmo. Lives in egui's `Memory` keyed by
/// clip-id (see [`hyperzoom_scratch_id`]) so the inspector spinboxes
/// and the viewer crosshair stay in sync without round-tripping
/// through `AppState`. Both `HyperzoomScratch` and
/// `HyperzoomTargetChoice` are `Copy` so reading from `Memory` is a
/// trivial value clone.
#[derive(Debug, Clone, Copy)]
pub struct HyperzoomScratch {
    pub focal_x: f32,
    pub focal_y: f32,
    pub duration_secs: f64,
    pub max_scale: f32,
    pub target_choice: HyperzoomTargetChoice,
    pub easing: HyperzoomEasing,
    pub hold_secs: f64,
    pub add_radial_blur: bool,
    pub radial_blur_amount: f32,
    /// Chromatic-aberration checkbox
    /// (HZ-NEXT-LIGHT-LEAK-CHROMATIC). When true, Apply populates
    /// `ApplyHyperzoom`'s `add_chromatic_aberration` +
    /// `chromatic_aberration_amount`.
    pub add_chromatic_aberration: bool,
    /// Peak chromatic-aberration amount (pixels). Inspector
    /// DragValue range 0..30; default 6.0 — visibly meaningful at
    /// first toggle, well below stylised territory.
    pub chromatic_aberration_amount: f32,
    /// Light-leak / FilmHalation checkbox.
    pub add_light_leak: bool,
    /// Peak FilmHalation intensity. Inspector DragValue range
    /// 0..1; default 0.4 — punchy enough that first toggle-on is
    /// visibly meaningful (FilmHalation's standalone default is
    /// 0.3; the hyperzoom version ramps to peak so needs more
    /// budget).
    pub light_leak_intensity: f32,
    /// Whoosh stinger checkbox (HZ-NEXT-WHOOSH). Inspector-side
    /// scratch — when true, Apply populates `ApplyHyperzoom`'s
    /// `add_whoosh` + `whoosh_variant` + `whoosh_custom_path`.
    pub whoosh_enabled: bool,
    /// Wire-format tag for the variant combo: `0/1/2 = Short /
    /// Medium / Long`, `255 = Custom`. Round-trips through
    /// [`WhooshChoice::from_u8`] / [`WhooshChoice::as_u8`].
    pub whoosh_variant: u8,
}

impl Default for HyperzoomScratch {
    fn default() -> Self {
        Self {
            focal_x: 0.5,
            focal_y: 0.5,
            duration_secs: 0.5,
            max_scale: 50.0,
            target_choice: HyperzoomTargetChoice::Auto,
            easing: HyperzoomEasing::Linear,
            hold_secs: 0.0,
            add_radial_blur: false,
            // Match the "Zoom Blur" preset baseline so toggling on
            // gives a punchy default — but only fires when the user
            // actually flips the checkbox.
            radial_blur_amount: 15.0,
            // Chromatic + light-leak follow the same two-layer
            // defaults pattern as radial-blur and whoosh: the
            // ApplyHyperzoom call sites pass 0.0 / false (off-state
            // canonical), but the inspector's scratch defaults are
            // visibly-meaningful first-toggle-on values per audit
            // (E). 6.0 px chromatic split = subtle but readable;
            // 0.4 halation intensity = punchier than FilmHalation's
            // 0.3 standalone default because the hyperzoom version
            // ramps to peak across the cut.
            add_chromatic_aberration: false,
            chromatic_aberration_amount: 6.0,
            add_light_leak: false,
            light_leak_intensity: 0.4,
            // Whoosh defaults match the AppCommand back-compat
            // path: opt-in checkbox off; variant tag = 0 (Short)
            // so the combo opens at a sensible default the
            // moment the checkbox turns on.
            whoosh_enabled: false,
            whoosh_variant: WhooshChoice::Short.as_u8(),
        }
    }
}

/// Inspector combo choice for `ApplyHyperzoom.whoosh_variant`.
/// Mirrors the [`HyperzoomEasing`] pattern — wire-format `u8` round-
/// trips through `as_u8` / `from_u8` so the UI carries the canonical
/// tag without holding a separate enum value alongside.
///
/// `Custom` (tag 255) carries no enum payload — the actual
/// `PathBuf` lives in a separate egui-Memory slot keyed by the same
/// clip id (see [`read_whoosh_custom`] / [`update_whoosh_custom`])
/// because `HyperzoomScratch` is `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhooshChoice {
    Short,
    Medium,
    Long,
    Custom,
}

impl WhooshChoice {
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Short => 0,
            Self::Medium => 1,
            Self::Long => 2,
            Self::Custom => 255,
        }
    }

    /// Map a wire-format tag back to a UI choice. Unknown tags map
    /// to `Medium` to mirror the AppCommand handler's
    /// "stale-dispatch never produces silence when the box is
    /// ticked" contract.
    pub const fn from_u8(tag: u8) -> Self {
        match tag {
            0 => Self::Short,
            1 => Self::Medium,
            2 => Self::Long,
            255 => Self::Custom,
            _ => Self::Medium,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Short => "Short",
            Self::Medium => "Medium",
            Self::Long => "Long",
            Self::Custom => "Custom Sample\u{2026}",
        }
    }

    pub const ALL: [WhooshChoice; 4] = [
        WhooshChoice::Short,
        WhooshChoice::Medium,
        WhooshChoice::Long,
        WhooshChoice::Custom,
    ];
}

/// Per-clip "Custom Sample" picker state. Lives in egui Memory
/// alongside `HyperzoomScratch` (separate slot because PathBuf
/// isn't `Copy`). The `length_secs` field is populated when the
/// user picks the file — `None` when the probe couldn't read the
/// duration (non-WAV format, malformed header). The inspector
/// shows the soft warning from
/// [`whoosh_custom_length_warning`] when length exceeds the
/// audit-note-(v) 5-second threshold.
#[derive(Debug, Clone, Default)]
pub struct WhooshCustomState {
    pub path: Option<std::path::PathBuf>,
    pub length_secs: Option<f32>,
}

pub fn whoosh_custom_id(clip_id: ClipId) -> egui::Id {
    egui::Id::new(("video:hyperzoom_whoosh_custom", clip_id))
}

pub fn read_whoosh_custom(ctx: &egui::Context, clip_id: ClipId) -> WhooshCustomState {
    ctx.data_mut(|d| {
        d.get_temp::<WhooshCustomState>(whoosh_custom_id(clip_id))
            .unwrap_or_default()
    })
}

pub fn update_whoosh_custom(
    ctx: &egui::Context,
    clip_id: ClipId,
    edit: impl FnOnce(&mut WhooshCustomState),
) {
    ctx.data_mut(|d| {
        let id = whoosh_custom_id(clip_id);
        let mut s: WhooshCustomState = d.get_temp(id).unwrap_or_default();
        edit(&mut s);
        d.insert_temp(id, s);
    });
}

/// Threshold above which the inspector shows a soft "this is
/// longer than typical" warning under the Custom Sample picker.
/// Per audit note (v): warning only — no truncation, no length
/// picker. Stricter enforcement is a v2 if foot-guns surface.
pub const WHOOSH_CUSTOM_LENGTH_WARN_SECS: f32 = 5.0;

/// Soft-warning text for the inspector's Custom Sample row.
/// Returns `None` when the file length is unknown OR ≤ threshold;
/// `Some(message)` when the file is unusually long. Pure function
/// so the threshold logic can be unit-tested without standing up
/// egui.
pub fn whoosh_custom_length_warning(length_secs: Option<f32>) -> Option<String> {
    let len = length_secs?;
    if len > WHOOSH_CUSTOM_LENGTH_WARN_SECS {
        Some(format!(
            "Custom sample is {:.1} s — typical whooshes are under 1 s. \
             Long samples will overlap downstream clips.",
            len,
        ))
    } else {
        None
    }
}

/// Build the three whoosh fields the Apply button passes to
/// `ApplyHyperzoom`, given the inspector's scratch state. Pure
/// helper — extracted from the inspector body so the
/// scratch → AppCommand mapping is unit-testable without standing
/// up egui.
///
/// Returns `(add_whoosh, whoosh_variant, whoosh_custom_path)`.
/// The custom path is included only when the checkbox is on AND
/// the variant is Custom — otherwise it's `None` regardless of
/// what the picker last left in memory (so an old custom file
/// doesn't get attached when the user picks a built-in variant).
pub fn build_whoosh_fields_for_apply(
    scratch: &HyperzoomScratch,
    custom: &WhooshCustomState,
) -> (bool, u8, Option<std::path::PathBuf>) {
    let custom_path = if scratch.whoosh_enabled
        && scratch.whoosh_variant == WhooshChoice::Custom.as_u8()
    {
        custom.path.clone()
    } else {
        None
    };
    (scratch.whoosh_enabled, scratch.whoosh_variant, custom_path)
}

/// Read a WAV file's duration in seconds without pulling in an
/// audio decoder dep. Walks RIFF chunks until it finds `fmt ` +
/// `data`; tolerant of intervening chunks (`LIST`, `JUNK`, etc.).
/// Returns `None` for non-WAV files or malformed headers — the
/// inspector treats `None` as "duration unknown, skip the soft
/// warning". MP3/FLAC/OGG users get no warning today; that's the
/// v2 follow-up.
pub fn probe_wav_duration_secs(path: &std::path::Path) -> Option<f32> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut riff = [0u8; 12];
    f.read_exact(&mut riff).ok()?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return None;
    }
    let mut channels: u32 = 0;
    let mut sample_rate: u32 = 0;
    let mut bits_per_sample: u32 = 0;
    let mut data_size: Option<u32> = None;
    loop {
        let mut hdr = [0u8; 8];
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        let size = u32::from_le_bytes(hdr[4..8].try_into().ok()?);
        if &hdr[0..4] == b"fmt " {
            // Read at least the 16-byte PCM fmt subchunk.
            let read_len = size.min(16) as usize;
            let mut fmt = vec![0u8; read_len];
            f.read_exact(&mut fmt).ok()?;
            if read_len < 16 {
                return None;
            }
            channels = u16::from_le_bytes([fmt[2], fmt[3]]) as u32;
            sample_rate = u32::from_le_bytes(fmt[4..8].try_into().ok()?);
            bits_per_sample = u16::from_le_bytes([fmt[14], fmt[15]]) as u32;
            // Skip any extension bytes after the canonical 16.
            let extra = size as i64 - 16;
            if extra > 0 {
                f.seek(SeekFrom::Current(extra)).ok()?;
            }
        } else if &hdr[0..4] == b"data" {
            data_size = Some(size);
            break;
        } else {
            // Even-byte alignment per RIFF spec.
            let skip = size as i64 + (size as i64 & 1);
            f.seek(SeekFrom::Current(skip)).ok()?;
        }
    }
    let data_size = data_size?;
    if channels == 0 || sample_rate == 0 || bits_per_sample == 0 {
        return None;
    }
    let bytes_per_sample = bits_per_sample / 8;
    if bytes_per_sample == 0 {
        return None;
    }
    let total_frames = data_size / (channels * bytes_per_sample);
    Some(total_frames as f32 / sample_rate as f32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HyperzoomTargetChoice {
    /// "Use whatever clip follows this one on the same track." The
    /// `ApplyHyperzoom` handler resolves this at apply time, so the
    /// user can change track contents between configuring and
    /// applying without the inspector going stale.
    Auto,
    /// Explicit pick — the handler will reject if the clip is gone
    /// at apply time.
    Explicit(ClipId),
}

/// Shared egui-`Id` for the per-clip `HyperzoomScratch` slot. Both
/// the inspector and the viewer call this so they read/write the
/// same memory cell — never compute the id ad hoc anywhere else, or
/// the two surfaces will silently desync.
pub fn hyperzoom_scratch_id(clip_id: ClipId) -> egui::Id {
    egui::Id::new(("video:hyperzoom_state", clip_id))
}

/// The egui pass a clip's HYPERZOOM section was last drawn open and on
/// screen in, kept per clip.
fn section_shown_id(clip_id: ClipId) -> egui::Id {
    egui::Id::new(("video:hyperzoom_section_shown", clip_id))
}

/// The HYPERZOOM section of `clip_id` is drawn open and on screen in this
/// pass: the viewer's focal handle shows and takes presses for the clip
/// while it is (#2032).
pub fn mark_section_shown(ctx: &egui::Context, clip_id: ClipId) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(section_shown_id(clip_id), pass));
}

/// Whether `clip_id`'s HYPERZOOM section was drawn open and on screen in
/// this pass or the one before: the inspector may draw before or after
/// the viewer. Only then is the viewer's focal handle there, so a press
/// on a selected clip's middle reaches the clip, not a focus point no one
/// is tuning (#2032).
pub fn section_shown(ctx: &egui::Context, clip_id: ClipId) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.get_temp::<u64>(section_shown_id(clip_id))).is_some_and(|at| at.saturating_add(1) >= pass)
}

/// [`mark_section_shown`] for as long as the test runs: the inspector
/// is not drawn in the viewer harness.
#[cfg(test)]
pub fn keep_section_shown(ctx: &egui::Context, clip_id: ClipId) {
    ctx.data_mut(|d| d.insert_temp(section_shown_id(clip_id), u64::MAX - 1));
}

/// Read this clip's scratch from egui memory. Returns the default
/// (centre focal point, 0.5 s, 50×, auto target) when no value is
/// stored — that's the same starting point the inspector spinboxes
/// fall back to on first paint, so reading from either side yields
/// the same picture before the user has touched anything.
pub fn read_hyperzoom_scratch(ctx: &egui::Context, clip_id: ClipId) -> HyperzoomScratch {
    ctx.data_mut(|d| d.get_temp(hyperzoom_scratch_id(clip_id)).unwrap_or_default())
}

/// Mutate this clip's scratch in place. Pass a closure so the read /
/// modify / write happens under a single `data_mut` lock — avoids
/// the racy two-call pattern (`read` + later `insert`) when multiple
/// surfaces edit different fields in the same frame.
pub fn update_hyperzoom_scratch(ctx: &egui::Context, clip_id: ClipId, edit: impl FnOnce(&mut HyperzoomScratch)) {
    ctx.data_mut(|d| {
        let id = hyperzoom_scratch_id(clip_id);
        let mut s: HyperzoomScratch = d.get_temp(id).unwrap_or_default();
        edit(&mut s);
        d.insert_temp(id, s);
    });
}

/// Easing curve emitted by [`plan_hyperzoom`] across the ramp window.
///
/// * [`Self::Linear`] — straight line between keyframes. Default. The
///   pre-easing-feature behaviour; existing projects + re-applies
///   stay byte-identical.
/// * [`Self::Bezier`] — symmetric S-curve via Bezier tangents at
///   `±duration/3`. Slow start, accelerate through the middle, slow
///   finish. Reads as the cinematic "rocket" feel on both source
///   ramp-in and target ramp-out.
///
/// The user picks the choice in the inspector's HYPERZOOM section.
/// Always opt-in — Apply Hyperzoom never silently flips a project's
/// existing ramps from Linear to Bezier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HyperzoomEasing {
    #[default]
    Linear,
    Bezier,
}

impl HyperzoomEasing {
    /// Wire-format tag used by [`klipp_core::AppCommand::ApplyHyperzoom`].
    /// 0 = Linear, 1 = Bezier. Mirrors the LinkMode-on-AppCommand
    /// pattern so we don't pull `klipp-ui` types into `klipp-core`.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Linear => 0,
            Self::Bezier => 1,
        }
    }

    /// Inverse of [`Self::as_u8`]; unknown values fall back to
    /// `Linear` (the safe default — never silently turns on Bezier).
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Bezier,
            _ => Self::Linear,
        }
    }
}

/// Inputs to [`plan_hyperzoom`]. Durations are clip-local seconds;
/// focal point is normalised screen coords (`0.0..=1.0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HyperzoomInput {
    pub source_clip_dur_secs: f64,
    pub target_clip_dur_secs: f64,
    pub focal_x_norm: f32,
    pub focal_y_norm: f32,
    pub duration_secs: f64,
    pub max_scale: f32,
    /// Easing across the ramp. Defaults to `Linear` for back-compat;
    /// flip to `Bezier` for an S-curve via the inspector.
    pub easing: HyperzoomEasing,
    /// Optional flat-at-peak segment straddling the cut. The source
    /// holds its last `hold_secs` at peak; the target holds its
    /// first `hold_secs` at peak — total perceived "frozen at peak"
    /// duration is `2 * hold_secs`. Set to `0.0` for the original
    /// two-keyframe ramp.
    pub hold_secs: f64,
    /// When `true`, the planner additionally emits radial-blur amount
    /// keyframes alongside the scale ramp. Defaults to `false` for
    /// back-compat — the standalone `RadialBlur` effect is always
    /// available from the effects library; this is just the
    /// convenience flag that wires it through Apply Hyperzoom.
    pub add_radial_blur: bool,
    /// Peak radial-blur amount (passed to `BuiltinEffect::RadialBlur`'s
    /// `amount` field). Same shape as the scale ramp: source ramps
    /// `0.0 → amount` over the source ramp; target ramps `amount → 0.0`
    /// over the target ramp; held segments stay at peak.
    pub radial_blur_amount: f32,
    /// When `true`, the planner additionally emits chromatic-
    /// aberration keyframes alongside the scale ramp. Same opt-in
    /// pattern as `add_radial_blur`. The standalone
    /// `BuiltinEffect::ChromaticAberration` is always available
    /// from the effects library; this just wires it through Apply
    /// Hyperzoom.
    pub add_chromatic_aberration: bool,
    /// Peak chromatic-aberration amount (pixels). One symmetric
    /// knob — the planner emits `red_shift` keyframes at
    /// `0.0 → -amount → 0.0` and `blue_shift` keyframes at
    /// `0.0 → +amount → 0.0` so red converges + blue diverges
    /// from the focal point (the canonical trailer-RGB-split
    /// look). Power users wanting asymmetric splits tweak the
    /// inserted builtin afterwards.
    pub chromatic_aberration_amount: f32,
    /// When `true`, the planner additionally emits film-halation
    /// intensity keyframes alongside the scale ramp. Light-leak
    /// aesthetic rides on `BuiltinEffect::FilmHalation` rather
    /// than `LensFlare` — halation's "bright pixels glow warmly"
    /// matches the trailer feel without needing a defined
    /// source point (the focal point is convergence-of-motion,
    /// not sun-position).
    pub add_light_leak: bool,
    /// Peak film-halation intensity (passed to
    /// `BuiltinEffect::FilmHalation::intensity`). Same time-shape
    /// as the scale ramp.
    pub light_leak_intensity: f32,
}

/// Output of [`plan_hyperzoom`]. Keyframe times are clip-local seconds
/// (relative to `source_in`, same convention as the rest of the
/// transform-keyframe machinery).
#[derive(Debug, Clone, PartialEq)]
pub struct HyperzoomPlan {
    pub source_anchor: (f32, f32),
    pub source_scale_kfs: Vec<Keyframe>,
    pub target_anchor: (f32, f32),
    pub target_scale_kfs: Vec<Keyframe>,
    /// Radial-blur center + per-side amount keyframes when
    /// `HyperzoomInput::add_radial_blur` was true. `None` otherwise so
    /// the handler can skip the radial-blur insertion entirely on
    /// the back-compat path.
    pub radial_blur: Option<RadialBlurPlan>,
    /// Chromatic-aberration center + per-side red/blue keyframes
    /// when `HyperzoomInput::add_chromatic_aberration` was true.
    /// `None` otherwise — same opt-in semantics as `radial_blur`.
    pub chromatic_aberration: Option<ChromaticAberrationPlan>,
    /// Film-halation per-side intensity keyframes when
    /// `HyperzoomInput::add_light_leak` was true. `None` otherwise.
    pub light_leak: Option<LightLeakPlan>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RadialBlurPlan {
    /// Normalised screen coords — same value as the focal point.
    pub center: (f32, f32),
    /// Peak amount the ramp climbs to (mirrors the input field).
    pub peak_amount: f32,
    /// `BuiltinEffect::RadialBlur::amount` keyframes for the source
    /// clip. Same time-shape as the scale ramp: ramps `0.0 → amount`
    /// over the source ramp window; under hold > 0 a 3rd keyframe
    /// pins the held segment at peak.
    pub source_amount_kfs: Vec<Keyframe>,
    /// Mirror of `source_amount_kfs` for the target side.
    pub target_amount_kfs: Vec<Keyframe>,
}

/// Sub-plan for the chromatic-aberration convenience layer. Two
/// keyframe sets per side — `red_shift` ramps `0 → -amount → 0`
/// while `blue_shift` ramps `0 → +amount → 0` so red converges
/// and blue diverges from the focal point (the canonical
/// trailer-RGB-split look). Both ride the same `build_ramp_*`
/// helpers as the scale ramp so the four animations stay
/// lock-step in time by construction.
#[derive(Debug, Clone, PartialEq)]
pub struct ChromaticAberrationPlan {
    /// Normalised screen coords for the split center — same value
    /// as the focal point.
    pub center: (f32, f32),
    /// Peak split magnitude (pixels). The handler uses this for
    /// the static fallback values it writes onto the inserted
    /// `BuiltinEffect::ChromaticAberration`; the keyframes drive
    /// the actual animation.
    pub peak_amount: f32,
    /// `BuiltinEffect::ChromaticAberration::red_shift` keyframes
    /// for the source clip. Values run `0 → -amount → 0` (red
    /// converges inward at peak).
    pub source_red_kfs: Vec<Keyframe>,
    /// `BuiltinEffect::ChromaticAberration::blue_shift` keyframes
    /// for the source clip. Values run `0 → +amount → 0` (blue
    /// diverges outward at peak). Mirror sign of red.
    pub source_blue_kfs: Vec<Keyframe>,
    /// Target-side red mirror — `0 → -amount → 0` riding the
    /// same time-shape as the scale ramp's target side.
    pub target_red_kfs: Vec<Keyframe>,
    /// Target-side blue mirror.
    pub target_blue_kfs: Vec<Keyframe>,
}

/// Sub-plan for the light-leak convenience layer. Rides
/// `BuiltinEffect::FilmHalation` (the "bright pixels glow warmly"
/// pass) rather than `LensFlare` — halation doesn't need a
/// defined source point and matches the trailer-style warm-bleed
/// aesthetic for the whole-image cut.
#[derive(Debug, Clone, PartialEq)]
pub struct LightLeakPlan {
    /// Peak halation intensity (mirrors the input field).
    pub peak_intensity: f32,
    /// `BuiltinEffect::FilmHalation::intensity` keyframes for the
    /// source clip. Same time-shape as the scale ramp: ramps
    /// `0.0 → intensity` over the source ramp window; under
    /// hold > 0 a 3rd keyframe pins the held segment at peak.
    pub source_intensity_kfs: Vec<Keyframe>,
    /// Mirror of `source_intensity_kfs` for the target side.
    pub target_intensity_kfs: Vec<Keyframe>,
}

/// Compute the keyframes + anchor for a hyperzoom transition between
/// two clips. All inputs are clamped to safe ranges so a UI bug or a
/// stale palette dispatch can never push impossible values into clip
/// data:
///
/// * `focal_x_norm` / `focal_y_norm` clamp to `0.0..=1.0`
/// * `max_scale` clamps to `>= 1.01` (a peak below 1× wouldn't zoom)
/// * `duration_secs` clamps to `>= 0.05` (50 ms — anything shorter
///   would land both keyframes on top of each other and `upsert` would
///   collapse them)
/// * Short clips (shorter than `duration_secs`) ramp from time 0
///   instead of going negative.
pub fn plan_hyperzoom(input: HyperzoomInput) -> HyperzoomPlan {
    let fx = input.focal_x_norm.clamp(0.0, 1.0);
    let fy = input.focal_y_norm.clamp(0.0, 1.0);
    let max_scale = input.max_scale.max(1.01);
    let dur = input.duration_secs.max(0.05);
    let hold = input.hold_secs.max(0.0);
    let easing = input.easing;

    // Source side. Anatomy of the timeline (clip-local seconds):
    //
    //     ramp_start → … (ease) … → peak_kf_time → … (flat) … → clip_end
    //
    // * Without hold (`hold == 0`): peak_kf_time == clip_end, no flat
    //   tail — the plan emits 2 keyframes (back-compat with the
    //   pre-hold-feature output).
    // * With hold (`hold > 0`): the last `hold` seconds of the source
    //   sit at peak, so the cut into the target lands inside a held-
    //   peak window rather than at the apex of the ramp.
    let peak_src = (input.source_clip_dur_secs - hold).max(0.0);
    let src_ramp_start = (peak_src - dur).max(0.0);
    let peak_src = peak_src.max(src_ramp_start + 0.05);
    let source_scale_kfs = build_ramp_keyframes(
        (src_ramp_start, 1.0),
        (peak_src, max_scale),
        easing,
        if hold > 0.0 {
            // Tail-hold: extra flat-at-peak keyframe at clip_end.
            // `>= peak_src + 0.05` so upsert's 20 ms dedup window
            // doesn't fold the two peak keyframes into one.
            Some(input.source_clip_dur_secs.max(peak_src + 0.05))
        } else {
            None
        },
    );

    // Target side, mirror anatomy:
    //
    //     0 → … (flat) … → ramp_start → … (ease) … → ramp_end
    //
    // The hold occupies the head of the target so the held-peak
    // window straddles the cut visually.
    let tgt_ramp_start = hold;
    let tgt_ramp_end = (tgt_ramp_start + dur).min(input.target_clip_dur_secs.max(tgt_ramp_start + dur));
    let tgt_ramp_end = tgt_ramp_end.max(tgt_ramp_start + 0.05);
    let target_scale_kfs = build_ramp_keyframes_target(
        if hold > 0.0 { Some(0.0) } else { None },
        (tgt_ramp_start, max_scale),
        (tgt_ramp_end, 1.0),
        easing,
    );

    // Radial-blur amount keyframes share the source/target ramp
    // window with the scale animation, but the values run 0.0 →
    // peak instead of 1.0 → max_scale. Reusing the same
    // build_ramp_keyframes helpers keeps the time math identical
    // by construction — there's exactly one place where the
    // ramp_start / peak / hold-tail times are computed.
    let radial_blur = if input.add_radial_blur && input.radial_blur_amount > 0.0 {
        let amount = input.radial_blur_amount.max(0.0);
        let src_amount_kfs = build_ramp_keyframes(
            (src_ramp_start, 0.0),
            (peak_src, amount),
            easing,
            if hold > 0.0 {
                Some(input.source_clip_dur_secs.max(peak_src + 0.05))
            } else {
                None
            },
        );
        let tgt_amount_kfs = build_ramp_keyframes_target(
            if hold > 0.0 { Some(0.0) } else { None },
            (tgt_ramp_start, amount),
            (tgt_ramp_end, 0.0),
            easing,
        );
        Some(RadialBlurPlan {
            center: (fx, fy),
            peak_amount: amount,
            source_amount_kfs: src_amount_kfs,
            target_amount_kfs: tgt_amount_kfs,
        })
    } else {
        None
    };

    // Chromatic-aberration keyframes — two paired ramps per side
    // (red_shift and blue_shift). Red converges (negative shift)
    // at peak; blue diverges (positive shift). Same time-shape as
    // the scale ramp via the shared build_ramp_* helpers.
    let chromatic_aberration = if input.add_chromatic_aberration && input.chromatic_aberration_amount > 0.0 {
        let amount = input.chromatic_aberration_amount.max(0.0);
        let src_red_kfs = build_ramp_keyframes(
            (src_ramp_start, 0.0),
            (peak_src, -amount),
            easing,
            if hold > 0.0 {
                Some(input.source_clip_dur_secs.max(peak_src + 0.05))
            } else {
                None
            },
        );
        let src_blue_kfs = build_ramp_keyframes(
            (src_ramp_start, 0.0),
            (peak_src, amount),
            easing,
            if hold > 0.0 {
                Some(input.source_clip_dur_secs.max(peak_src + 0.05))
            } else {
                None
            },
        );
        let tgt_red_kfs = build_ramp_keyframes_target(
            if hold > 0.0 { Some(0.0) } else { None },
            (tgt_ramp_start, -amount),
            (tgt_ramp_end, 0.0),
            easing,
        );
        let tgt_blue_kfs = build_ramp_keyframes_target(
            if hold > 0.0 { Some(0.0) } else { None },
            (tgt_ramp_start, amount),
            (tgt_ramp_end, 0.0),
            easing,
        );
        Some(ChromaticAberrationPlan {
            center: (fx, fy),
            peak_amount: amount,
            source_red_kfs: src_red_kfs,
            source_blue_kfs: src_blue_kfs,
            target_red_kfs: tgt_red_kfs,
            target_blue_kfs: tgt_blue_kfs,
        })
    } else {
        None
    };

    // Light-leak intensity keyframes — single ramp per side on
    // FilmHalation::intensity. Same time-shape via the shared
    // build_ramp_* helpers.
    let light_leak = if input.add_light_leak && input.light_leak_intensity > 0.0 {
        let intensity = input.light_leak_intensity.max(0.0);
        let src_intensity_kfs = build_ramp_keyframes(
            (src_ramp_start, 0.0),
            (peak_src, intensity),
            easing,
            if hold > 0.0 {
                Some(input.source_clip_dur_secs.max(peak_src + 0.05))
            } else {
                None
            },
        );
        let tgt_intensity_kfs = build_ramp_keyframes_target(
            if hold > 0.0 { Some(0.0) } else { None },
            (tgt_ramp_start, intensity),
            (tgt_ramp_end, 0.0),
            easing,
        );
        Some(LightLeakPlan {
            peak_intensity: intensity,
            source_intensity_kfs: src_intensity_kfs,
            target_intensity_kfs: tgt_intensity_kfs,
        })
    } else {
        None
    };

    HyperzoomPlan {
        source_anchor: (fx, fy),
        source_scale_kfs,
        target_anchor: (fx, fy),
        target_scale_kfs,
        radial_blur,
        chromatic_aberration,
        light_leak,
    }
}

/// Build the source-side keyframe sequence:
///
/// * `ramp_start` → `peak`: easing-shaped ramp.
/// * Optional `tail_hold_at` keyframe pinned at the peak value to
///   extend a flat segment after the ramp completes.
///
/// Bezier easing emits an S-curve via symmetric `±dur/3` horizontal
/// tangents on the ramp endpoints. The tail-hold keyframe (when
/// present) is always Linear with zero tangents so the held segment
/// is dead flat — a Bezier flat segment would be redundant and could
/// produce visual jitter at sub-millisecond integration steps.
fn build_ramp_keyframes(
    ramp_start: (f64, f32),
    peak: (f64, f32),
    easing: HyperzoomEasing,
    tail_hold_at: Option<f64>,
) -> Vec<Keyframe> {
    let segment_dur = (peak.0 - ramp_start.0).max(1e-6);
    let mut out = Vec::with_capacity(3);
    out.push(ramp_endpoint_keyframe(ramp_start.0, ramp_start.1, easing, segment_dur, RampEndpoint::Start));
    out.push(ramp_endpoint_keyframe(peak.0, peak.1, easing, segment_dur, RampEndpoint::End));
    if let Some(t) = tail_hold_at {
        out.push(keyframe(t, peak.1));
    }
    out
}

/// Mirror of [`build_ramp_keyframes`] for the target side, where the
/// optional flat segment is at the *head* (frame 0 → ramp_start) and
/// the easing applies to the descending ramp segment.
fn build_ramp_keyframes_target(
    head_hold_at: Option<f64>,
    ramp_start: (f64, f32),
    ramp_end: (f64, f32),
    easing: HyperzoomEasing,
) -> Vec<Keyframe> {
    let segment_dur = (ramp_end.0 - ramp_start.0).max(1e-6);
    let mut out = Vec::with_capacity(3);
    if let Some(t) = head_hold_at {
        out.push(keyframe(t, ramp_start.1));
    }
    out.push(ramp_endpoint_keyframe(ramp_start.0, ramp_start.1, easing, segment_dur, RampEndpoint::Start));
    out.push(ramp_endpoint_keyframe(ramp_end.0, ramp_end.1, easing, segment_dur, RampEndpoint::End));
    out
}

#[derive(Copy, Clone)]
enum RampEndpoint {
    Start,
    End,
}

/// Emit a keyframe at `(time_secs, value)` whose interpolation +
/// tangents match `easing`. Bezier endpoints get symmetric horizontal
/// tangents at `±segment_dur/3` — the standard "smooth" Bezier shape;
/// 1/3 of the segment is the convention shared by After Effects,
/// Resolve, and most NLE keyframe editors.
fn ramp_endpoint_keyframe(
    time_secs: f64,
    value: f32,
    easing: HyperzoomEasing,
    segment_dur: f64,
    which: RampEndpoint,
) -> Keyframe {
    match easing {
        HyperzoomEasing::Linear => keyframe(time_secs, value),
        HyperzoomEasing::Bezier => {
            let third = segment_dur / 3.0;
            let (tangent_in, tangent_out) = match which {
                RampEndpoint::Start => ((0.0, 0.0), (third, 0.0)),
                RampEndpoint::End => ((-third, 0.0), (0.0, 0.0)),
            };
            Keyframe {
                time_secs,
                value,
                interpolation: Interpolation::Bezier,
                tangent_in,
                tangent_out,
                handles: Default::default(),
            }
        }
    }
}

/// Walk every track strictly *below* `source_clip_id`'s track and
/// return the clip-ids whose timeline range overlaps the ramp window
/// `[source.timeline_out - ramp_secs, source.timeline_out]`. The
/// returned clips are the "beneath the source during the ramp" set —
/// candidates for an auto-applied `transform_link` so they fly into
/// the focal point alongside the source.
///
/// Filters out:
///
/// * The source clip itself.
/// * Clips on the source's own track or any track above it (overlay-
///   above-source isn't "beneath" — those clips would land in the
///   user's manual-parent decision instead).
/// * `AdjustmentLayer` / `Gap` / nested `Sequence` clips — none have a
///   meaningful own transform to inherit one onto.
/// * Clips that already have a `transform_link` set — preserves the
///   user's explicit parenting choice. Re-running auto-link is then
///   idempotent.
/// * Clips whose timeline range doesn't overlap the ramp window —
///   they aren't on screen during the zoom, so linking them is a
///   no-op visually but still pollutes the data.
/// * Clips where binding `clip → source` would close a transform-link
///   cycle (defence-in-depth via the same 64-depth walker the
///   `SetClipTransformLink` handler uses; in normal hyperzoom usage
///   the source has no link of its own so cycles can't happen, but a
///   pathological project file shouldn't produce a runtime cycle).
///
/// Returns clip-ids in iteration order (track-major, then clip order
/// within each track) so callers can render any UI list deterministically.
pub fn collect_auto_link_targets(
    seq: &Sequence,
    source_clip_id: ClipId,
    ramp_secs: f64,
) -> Vec<ClipId> {
    // Locate the source clip + its track index. Bail if the source
    // is gone (stale dispatch).
    let mut source_track_idx: Option<usize> = None;
    let mut ramp_start_secs: f64 = 0.0;
    let mut ramp_end_secs: f64 = 0.0;
    for (ti, track) in seq.tracks.iter().enumerate() {
        if let Some(c) = track.clips.iter().find(|c| c.id == source_clip_id) {
            source_track_idx = Some(ti);
            let end = c.timeline_out.to_secs_f64();
            ramp_end_secs = end;
            ramp_start_secs = (end - ramp_secs.max(0.0)).max(0.0);
            break;
        }
    }
    let Some(src_ti) = source_track_idx else {
        return Vec::new();
    };

    let ramp_secs_safe = ramp_secs.max(0.0);
    if ramp_secs_safe < 1e-9 {
        // Zero-length ramp: no clip is "during" it. Don't pretend.
        return Vec::new();
    }

    let mut out = Vec::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        if ti >= src_ti {
            // Same-track and above-track clips skipped. The source
            // itself is on `src_ti` and its target was handled by the
            // hyperzoom Apply.
            continue;
        }
        for clip in &track.clips {
            if clip.id == source_clip_id {
                continue;
            }
            if matches!(
                clip.kind,
                ClipKind::AdjustmentLayer | ClipKind::Gap | ClipKind::Sequence { .. }
            ) {
                continue;
            }
            if clip.transform_link.is_some() {
                continue;
            }
            // Half-open overlap: a clip ending exactly at ramp_start
            // doesn't visually overlap the ramp (it's gone the frame
            // the ramp begins), and a clip starting exactly at
            // ramp_end is the next shot — handled by the hyperzoom
            // target side.
            let clip_in = clip.timeline_in.to_secs_f64();
            let clip_out = clip.timeline_out.to_secs_f64();
            let overlaps = clip_in < ramp_end_secs && clip_out > ramp_start_secs;
            if !overlaps {
                continue;
            }
            // Cycle-protection: would this binding close a
            // transform-link loop?
            if would_create_link_cycle(seq, clip.id, source_clip_id) {
                continue;
            }
            out.push(clip.id);
        }
    }
    out
}

/// Inline mirror of `app::transform_link_chain_creates_cycle` so the
/// helper crate doesn't need to reach across module boundaries for
/// the predicate. Same 64-link depth cap; same semantics. The two
/// copies are pinned by `transform_link_handler_tests` (in `app.rs`)
/// and the auto-link tests below.
fn would_create_link_cycle(seq: &Sequence, consumer: ClipId, target: ClipId) -> bool {
    if consumer == target {
        return true;
    }
    let mut current = target;
    for _ in 0..64 {
        let next = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == current)
            .and_then(|c| c.transform_link.as_ref().map(|tl| tl.target.referenced_clip_id()));
        match next {
            None => return false,
            Some(n) if n == consumer => return true,
            Some(n) => current = n,
        }
    }
    true
}

fn keyframe(time_secs: f64, value: f32) -> Keyframe {
    Keyframe {
        time_secs,
        value,
        interpolation: Interpolation::Linear,
        tangent_in: (0.0, 0.0),
        tangent_out: (0.0, 0.0),
        handles: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(focal_x: f32, focal_y: f32, dur: f64, max: f32, src: f64, tgt: f64) -> HyperzoomInput {
        // Default constructor used by the original two-keyframe tests:
        // Linear easing, no hold, no radial blur — matches the
        // pre-feature output exactly so those tests stay green.
        HyperzoomInput {
            source_clip_dur_secs: src,
            target_clip_dur_secs: tgt,
            focal_x_norm: focal_x,
            focal_y_norm: focal_y,
            duration_secs: dur,
            max_scale: max,
            easing: HyperzoomEasing::Linear,
            hold_secs: 0.0,
            add_radial_blur: false,
            radial_blur_amount: 0.0,
            add_chromatic_aberration: false,
            chromatic_aberration_amount: 0.0,
            add_light_leak: false,
            light_leak_intensity: 0.0,
        }
    }

    #[test]
    fn plan_centre_focal_point_matches_inputs() {
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        assert_eq!(plan.source_anchor, (0.5, 0.5));
        assert_eq!(plan.target_anchor, (0.5, 0.5));
    }

    #[test]
    fn plan_source_ramps_from_one_to_max_at_clip_end() {
        // 3 s source, 0.5 s ramp → kf at t=2.5, value=1.0; kf at t=3.0, value=max.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        let kfs = &plan.source_scale_kfs;
        assert_eq!(kfs.len(), 2);
        assert!((kfs[0].time_secs - 2.5).abs() < 1e-9);
        assert!((kfs[0].value - 1.0).abs() < 1e-6);
        assert!((kfs[1].time_secs - 3.0).abs() < 1e-9);
        assert!((kfs[1].value - 50.0).abs() < 1e-6);
    }

    #[test]
    fn plan_target_ramps_from_max_to_one_starting_at_zero() {
        // 3 s target, 0.5 s ramp → kf at t=0, value=max; kf at t=0.5, value=1.0.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        let kfs = &plan.target_scale_kfs;
        assert_eq!(kfs.len(), 2);
        assert!((kfs[0].time_secs - 0.0).abs() < 1e-9);
        assert!((kfs[0].value - 50.0).abs() < 1e-6);
        assert!((kfs[1].time_secs - 0.5).abs() < 1e-9);
        assert!((kfs[1].value - 1.0).abs() < 1e-6);
    }

    #[test]
    fn plan_clamps_focal_point_to_unit_range() {
        let plan = plan_hyperzoom(input(-0.5, 1.5, 0.5, 50.0, 3.0, 3.0));
        assert_eq!(plan.source_anchor, (0.0, 1.0));
        assert_eq!(plan.target_anchor, (0.0, 1.0));
    }

    #[test]
    fn plan_clamps_max_scale_to_floor() {
        // max < 1.0 would mean "zoom out" — guard against it; floor
        // is 1.01 so the keyframes still differ.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 0.5, 3.0, 3.0));
        let last_src = plan.source_scale_kfs.last().unwrap();
        assert!(last_src.value >= 1.01);
    }

    #[test]
    fn plan_short_source_does_not_produce_negative_ramp_start() {
        // Source is 0.2 s but duration_secs = 0.5 — ramp must start at
        // t=0, not t=-0.3.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 0.2, 3.0));
        let first_src = &plan.source_scale_kfs[0];
        assert!(first_src.time_secs >= 0.0);
    }

    // ──────────── Easing + hold-at-peak (opt-in extras) ────────────

    fn input_with(
        focal_x: f32, focal_y: f32, dur: f64, max: f32, src: f64, tgt: f64,
        easing: HyperzoomEasing, hold: f64,
    ) -> HyperzoomInput {
        HyperzoomInput {
            source_clip_dur_secs: src,
            target_clip_dur_secs: tgt,
            focal_x_norm: focal_x,
            focal_y_norm: focal_y,
            duration_secs: dur,
            max_scale: max,
            easing,
            hold_secs: hold,
            add_radial_blur: false,
            radial_blur_amount: 0.0,
            add_chromatic_aberration: false,
            chromatic_aberration_amount: 0.0,
            add_light_leak: false,
            light_leak_intensity: 0.0,
        }
    }

    #[test]
    fn linear_default_emits_linear_interpolation() {
        // Pin: pre-easing-feature behaviour stays Linear. A regression
        // where Apply Hyperzoom silently flipped to Bezier on existing
        // projects would be a back-compat break.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        for kf in &plan.source_scale_kfs {
            assert!(matches!(kf.interpolation, Interpolation::Linear));
            assert_eq!(kf.tangent_in, (0.0, 0.0));
            assert_eq!(kf.tangent_out, (0.0, 0.0));
        }
        for kf in &plan.target_scale_kfs {
            assert!(matches!(kf.interpolation, Interpolation::Linear));
            assert_eq!(kf.tangent_in, (0.0, 0.0));
            assert_eq!(kf.tangent_out, (0.0, 0.0));
        }
    }

    #[test]
    fn bezier_easing_emits_symmetric_horizontal_tangents() {
        // Bezier S-curve: ramp endpoints have horizontal handles
        // (zero value-delta) extending ±dur/3 into the segment.
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.6, 50.0, 3.0, 3.0, HyperzoomEasing::Bezier, 0.0));
        let third = 0.6 / 3.0;
        // Source: 2 keyframes (no hold). First = ramp-start, second = peak.
        assert_eq!(plan.source_scale_kfs.len(), 2);
        let s0 = &plan.source_scale_kfs[0];
        let s1 = &plan.source_scale_kfs[1];
        assert!(matches!(s0.interpolation, Interpolation::Bezier));
        assert!(matches!(s1.interpolation, Interpolation::Bezier));
        assert_eq!(s0.tangent_in, (0.0, 0.0));
        assert!((s0.tangent_out.0 - third).abs() < 1e-9);
        assert!(s0.tangent_out.1.abs() < 1e-6);
        assert!((s1.tangent_in.0 + third).abs() < 1e-9, "End-of-ramp pulls tangent left");
        assert!(s1.tangent_in.1.abs() < 1e-6);
        assert_eq!(s1.tangent_out, (0.0, 0.0));
    }

    #[test]
    fn bezier_target_endpoints_mirror_source_shape() {
        // Same handle math, applied to the descending ramp on the
        // target side. The ramp is `max → 1.0` but the tangent
        // shape (horizontal handles ±third) is identical — the
        // descending segment becomes ease-out / settle-into-1.0
        // automatically because of the value direction.
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.6, 50.0, 3.0, 3.0, HyperzoomEasing::Bezier, 0.0));
        let third = 0.6 / 3.0;
        assert_eq!(plan.target_scale_kfs.len(), 2);
        let t0 = &plan.target_scale_kfs[0];
        let t1 = &plan.target_scale_kfs[1];
        assert!(matches!(t0.interpolation, Interpolation::Bezier));
        assert!(matches!(t1.interpolation, Interpolation::Bezier));
        assert!((t0.tangent_out.0 - third).abs() < 1e-9);
        assert!((t1.tangent_in.0 + third).abs() < 1e-9);
    }

    #[test]
    fn hold_secs_zero_keeps_two_keyframe_back_compat_shape() {
        // Pin: hold = 0 must produce the same 2-keyframe output as
        // the pre-hold-feature `input()` constructor. Otherwise the
        // existing `plan_source_ramps_from_one_to_max_at_clip_end`
        // and friends would silently change shape under us.
        let baseline = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        let with_hold_zero = plan_hyperzoom(input_with(0.5, 0.5, 0.5, 50.0, 3.0, 3.0, HyperzoomEasing::Linear, 0.0));
        assert_eq!(baseline.source_scale_kfs.len(), with_hold_zero.source_scale_kfs.len());
        assert_eq!(baseline.target_scale_kfs.len(), with_hold_zero.target_scale_kfs.len());
        for (a, b) in baseline.source_scale_kfs.iter().zip(&with_hold_zero.source_scale_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
            assert!((a.value - b.value).abs() < 1e-6);
        }
    }

    #[test]
    fn hold_inserts_third_keyframe_on_each_side() {
        // 3 s clip, 0.5 s ramp, 0.2 s hold. Expected:
        //   source kfs: (clip_dur - hold - dur, 1.0), (clip_dur - hold, max), (clip_dur, max)
        //              = (2.3, 1.0), (2.8, 50.0), (3.0, 50.0)
        //   target kfs: (0.0, max), (hold, max), (hold + dur, 1.0)
        //              = (0.0, 50.0), (0.2, 50.0), (0.7, 1.0)
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.5, 50.0, 3.0, 3.0, HyperzoomEasing::Linear, 0.2));
        let s = &plan.source_scale_kfs;
        let t = &plan.target_scale_kfs;
        assert_eq!(s.len(), 3);
        assert_eq!(t.len(), 3);
        assert!((s[0].time_secs - 2.3).abs() < 1e-6);
        assert!((s[0].value - 1.0).abs() < 1e-6);
        assert!((s[1].time_secs - 2.8).abs() < 1e-6);
        assert!((s[1].value - 50.0).abs() < 1e-6);
        assert!((s[2].time_secs - 3.0).abs() < 1e-6);
        assert!((s[2].value - 50.0).abs() < 1e-6);
        assert!((t[0].time_secs - 0.0).abs() < 1e-6);
        assert!((t[0].value - 50.0).abs() < 1e-6);
        assert!((t[1].time_secs - 0.2).abs() < 1e-6);
        assert!((t[1].value - 50.0).abs() < 1e-6);
        assert!((t[2].time_secs - 0.7).abs() < 1e-6);
        assert!((t[2].value - 1.0).abs() < 1e-6);
    }

    #[test]
    fn hold_segment_is_linear_even_under_bezier_easing() {
        // The flat held segment must stay flat — we deliberately keep
        // the hold keyframe Linear with zero tangents so a Bezier
        // S-curve doesn't introduce sub-pixel jitter through the held
        // window. Only the ramp endpoints carry the easing shape.
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.5, 50.0, 3.0, 3.0, HyperzoomEasing::Bezier, 0.2));
        let s_tail = plan.source_scale_kfs.last().unwrap();
        let t_head = plan.target_scale_kfs.first().unwrap();
        assert!(matches!(s_tail.interpolation, Interpolation::Linear));
        assert!(matches!(t_head.interpolation, Interpolation::Linear));
        assert_eq!(s_tail.tangent_in, (0.0, 0.0));
        assert_eq!(s_tail.tangent_out, (0.0, 0.0));
        assert_eq!(t_head.tangent_in, (0.0, 0.0));
        assert_eq!(t_head.tangent_out, (0.0, 0.0));
    }

    // ─────────── Radial-blur convenience layer (opt-in) ───────────

    fn input_with_blur(
        easing: HyperzoomEasing, hold: f64,
        add_blur: bool, blur_amount: f32,
    ) -> HyperzoomInput {
        HyperzoomInput {
            source_clip_dur_secs: 3.0,
            target_clip_dur_secs: 3.0,
            focal_x_norm: 0.5,
            focal_y_norm: 0.5,
            duration_secs: 0.5,
            max_scale: 50.0,
            easing,
            hold_secs: hold,
            add_radial_blur: add_blur,
            radial_blur_amount: blur_amount,
            add_chromatic_aberration: false,
            chromatic_aberration_amount: 0.0,
            add_light_leak: false,
            light_leak_intensity: 0.0,
        }
    }

    #[test]
    fn radial_blur_disabled_yields_none_in_plan() {
        // Pin: by default no radial-blur entry — handler skips the
        // RadialBlur insertion entirely. Ensures back-compat for
        // every existing Apply Hyperzoom invocation.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        assert!(plan.radial_blur.is_none());
    }

    #[test]
    fn radial_blur_zero_amount_yields_none_even_when_enabled() {
        // Toggle on but amount=0: the visual effect is identity, so
        // the planner short-circuits. Avoids inserting a zero-blur
        // RadialBlur builtin that would just round-trip pixels
        // through the GPU/CPU dispatch for nothing.
        let plan = plan_hyperzoom(input_with_blur(HyperzoomEasing::Linear, 0.0, true, 0.0));
        assert!(plan.radial_blur.is_none());
    }

    #[test]
    fn radial_blur_enabled_emits_amount_keyframes_with_scale_timing() {
        // Same time-shape as the scale ramp, values 0 → amount → 0.
        let plan = plan_hyperzoom(input_with_blur(HyperzoomEasing::Linear, 0.0, true, 15.0));
        let rb = plan.radial_blur.expect("radial blur enabled with positive amount");
        assert_eq!(rb.center, (0.5, 0.5));
        assert!((rb.peak_amount - 15.0).abs() < 1e-6);
        // Source: (2.5, 0.0) → (3.0, 15.0) — same times as scale_kfs.
        assert_eq!(rb.source_amount_kfs.len(), 2);
        assert!((rb.source_amount_kfs[0].time_secs - 2.5).abs() < 1e-9);
        assert!(rb.source_amount_kfs[0].value.abs() < 1e-6);
        assert!((rb.source_amount_kfs[1].time_secs - 3.0).abs() < 1e-9);
        assert!((rb.source_amount_kfs[1].value - 15.0).abs() < 1e-6);
        // Target: (0.0, 15.0) → (0.5, 0.0).
        assert_eq!(rb.target_amount_kfs.len(), 2);
        assert!((rb.target_amount_kfs[0].time_secs - 0.0).abs() < 1e-9);
        assert!((rb.target_amount_kfs[0].value - 15.0).abs() < 1e-6);
        assert!((rb.target_amount_kfs[1].time_secs - 0.5).abs() < 1e-9);
        assert!(rb.target_amount_kfs[1].value.abs() < 1e-6);

        // Time-shape parity with scale_kfs — drift between the two
        // would mean the blur ramp leads or lags the zoom ramp,
        // which would feel weirdly disconnected.
        for (a, b) in plan.source_scale_kfs.iter().zip(&rb.source_amount_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
        for (a, b) in plan.target_scale_kfs.iter().zip(&rb.target_amount_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
    }

    #[test]
    fn radial_blur_picks_up_hold_segment_three_keyframes() {
        // 3-keyframe shape mirrors the scale ramp under hold > 0.
        let plan = plan_hyperzoom(input_with_blur(HyperzoomEasing::Linear, 0.2, true, 15.0));
        let rb = plan.radial_blur.unwrap();
        assert_eq!(rb.source_amount_kfs.len(), 3);
        assert_eq!(rb.target_amount_kfs.len(), 3);
        // Source held tail at peak amount.
        assert!((rb.source_amount_kfs[1].value - 15.0).abs() < 1e-6);
        assert!((rb.source_amount_kfs[2].value - 15.0).abs() < 1e-6);
        // Target head held at peak amount.
        assert!((rb.target_amount_kfs[0].value - 15.0).abs() < 1e-6);
        assert!((rb.target_amount_kfs[1].value - 15.0).abs() < 1e-6);
    }

    #[test]
    fn radial_blur_inherits_bezier_easing_on_ramp_endpoints() {
        // Bezier flag flows through to the radial-blur amount kfs
        // identically — so the blur ramps with the same S-curve as
        // the zoom and the two motions stay locked.
        let plan = plan_hyperzoom(input_with_blur(HyperzoomEasing::Bezier, 0.0, true, 15.0));
        let rb = plan.radial_blur.unwrap();
        for kf in &rb.source_amount_kfs {
            assert!(matches!(kf.interpolation, Interpolation::Bezier));
        }
        for kf in &rb.target_amount_kfs {
            assert!(matches!(kf.interpolation, Interpolation::Bezier));
        }
    }

    #[test]
    fn hold_clamps_negative_to_zero() {
        // Defensive: negative hold inputs (stale UI state, mis-typed
        // value) clamp to 0 — never produce a 3-keyframe shape with
        // a backward-running tail.
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.5, 50.0, 3.0, 3.0, HyperzoomEasing::Linear, -0.5));
        assert_eq!(plan.source_scale_kfs.len(), 2);
        assert_eq!(plan.target_scale_kfs.len(), 2);
    }

    #[test]
    fn plan_emits_distinct_keyframe_times() {
        // Even the shortest planned move keeps both authored endpoints
        // when stored through the shared timestamp-identity rule.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.0, 50.0, 3.0, 3.0));
        for planned in [&plan.source_scale_kfs, &plan.target_scale_kfs] {
            let mut stored = Vec::new();
            for k in planned {
                klipp_core::keyframe::upsert_keyframe(&mut stored, k.time_secs, k.value);
            }
            assert_eq!(stored.len(), planned.len());
            assert!(planned.windows(2).all(|w| w[1].time_secs - w[0].time_secs >= 0.05 - 1e-9));
        }
    }

    // ────── Chromatic-aberration convenience layer (opt-in) ──────

    fn input_with_chroma(
        easing: HyperzoomEasing, hold: f64,
        add_chroma: bool, chroma_amount: f32,
    ) -> HyperzoomInput {
        HyperzoomInput {
            source_clip_dur_secs: 3.0,
            target_clip_dur_secs: 3.0,
            focal_x_norm: 0.5,
            focal_y_norm: 0.5,
            duration_secs: 0.5,
            max_scale: 50.0,
            easing,
            hold_secs: hold,
            add_radial_blur: false,
            radial_blur_amount: 0.0,
            add_chromatic_aberration: add_chroma,
            chromatic_aberration_amount: chroma_amount,
            add_light_leak: false,
            light_leak_intensity: 0.0,
        }
    }

    #[test]
    fn chromatic_disabled_yields_none_in_plan() {
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        assert!(plan.chromatic_aberration.is_none());
    }

    #[test]
    fn chromatic_zero_amount_yields_none_even_when_enabled() {
        let plan = plan_hyperzoom(input_with_chroma(HyperzoomEasing::Linear, 0.0, true, 0.0));
        assert!(plan.chromatic_aberration.is_none());
    }

    #[test]
    fn chromatic_enabled_emits_red_and_blue_keyframes_with_scale_timing() {
        let plan = plan_hyperzoom(input_with_chroma(HyperzoomEasing::Linear, 0.0, true, 6.0));
        let ca = plan.chromatic_aberration.expect("chromatic enabled with positive amount");
        assert_eq!(ca.center, (0.5, 0.5));
        assert!((ca.peak_amount - 6.0).abs() < 1e-6);
        // Source red: (2.5, 0.0) → (3.0, -6.0). Source blue:
        // (2.5, 0.0) → (3.0, +6.0). Lock-step times.
        assert_eq!(ca.source_red_kfs.len(), 2);
        assert_eq!(ca.source_blue_kfs.len(), 2);
        assert!((ca.source_red_kfs[1].value - -6.0).abs() < 1e-6);
        assert!((ca.source_blue_kfs[1].value - 6.0).abs() < 1e-6);
        // Target sides mirror.
        assert!((ca.target_red_kfs[0].value - -6.0).abs() < 1e-6);
        assert!((ca.target_blue_kfs[0].value - 6.0).abs() < 1e-6);
        assert!(ca.target_red_kfs[1].value.abs() < 1e-6);
        assert!(ca.target_blue_kfs[1].value.abs() < 1e-6);
        // Time-shape parity with scale_kfs — all four chromatic
        // keyframe sets ride the same time-shape as the scale ramp.
        for (a, b) in plan.source_scale_kfs.iter().zip(&ca.source_red_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
        for (a, b) in plan.source_scale_kfs.iter().zip(&ca.source_blue_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
        for (a, b) in plan.target_scale_kfs.iter().zip(&ca.target_red_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
        for (a, b) in plan.target_scale_kfs.iter().zip(&ca.target_blue_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
    }

    #[test]
    fn chromatic_red_and_blue_are_sign_mirrors() {
        // Pin the symmetric-split convention: at every keyframe,
        // red_shift == -blue_shift. If a future audit changes the
        // convention (e.g. asymmetric ramps), this test surfaces
        // it before trailer aesthetics drift.
        let plan = plan_hyperzoom(input_with_chroma(HyperzoomEasing::Bezier, 0.2, true, 8.0));
        let ca = plan.chromatic_aberration.unwrap();
        for (r, b) in ca.source_red_kfs.iter().zip(&ca.source_blue_kfs) {
            assert!((r.value + b.value).abs() < 1e-6,
                "red={} blue={} at t={} not sign-mirrors", r.value, b.value, r.time_secs);
        }
        for (r, b) in ca.target_red_kfs.iter().zip(&ca.target_blue_kfs) {
            assert!((r.value + b.value).abs() < 1e-6,
                "red={} blue={} at t={} not sign-mirrors", r.value, b.value, r.time_secs);
        }
    }

    #[test]
    fn chromatic_picks_up_hold_segment_three_keyframes() {
        let plan = plan_hyperzoom(input_with_chroma(HyperzoomEasing::Linear, 0.2, true, 6.0));
        let ca = plan.chromatic_aberration.unwrap();
        assert_eq!(ca.source_red_kfs.len(), 3);
        assert_eq!(ca.source_blue_kfs.len(), 3);
        assert_eq!(ca.target_red_kfs.len(), 3);
        assert_eq!(ca.target_blue_kfs.len(), 3);
    }

    #[test]
    fn chromatic_inherits_bezier_easing_on_ramp_endpoints() {
        let plan = plan_hyperzoom(input_with_chroma(HyperzoomEasing::Bezier, 0.0, true, 6.0));
        let ca = plan.chromatic_aberration.unwrap();
        for kf in ca.source_red_kfs.iter().chain(&ca.source_blue_kfs)
            .chain(&ca.target_red_kfs).chain(&ca.target_blue_kfs)
        {
            assert!(matches!(kf.interpolation, Interpolation::Bezier));
        }
    }

    // ────── Light-leak (FilmHalation) convenience layer (opt-in) ──────

    fn input_with_leak(
        easing: HyperzoomEasing, hold: f64,
        add_leak: bool, leak_intensity: f32,
    ) -> HyperzoomInput {
        HyperzoomInput {
            source_clip_dur_secs: 3.0,
            target_clip_dur_secs: 3.0,
            focal_x_norm: 0.5,
            focal_y_norm: 0.5,
            duration_secs: 0.5,
            max_scale: 50.0,
            easing,
            hold_secs: hold,
            add_radial_blur: false,
            radial_blur_amount: 0.0,
            add_chromatic_aberration: false,
            chromatic_aberration_amount: 0.0,
            add_light_leak: add_leak,
            light_leak_intensity: leak_intensity,
        }
    }

    #[test]
    fn light_leak_disabled_yields_none_in_plan() {
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        assert!(plan.light_leak.is_none());
    }

    #[test]
    fn light_leak_zero_intensity_yields_none_even_when_enabled() {
        let plan = plan_hyperzoom(input_with_leak(HyperzoomEasing::Linear, 0.0, true, 0.0));
        assert!(plan.light_leak.is_none());
    }

    #[test]
    fn light_leak_enabled_emits_intensity_keyframes_with_scale_timing() {
        let plan = plan_hyperzoom(input_with_leak(HyperzoomEasing::Linear, 0.0, true, 0.4));
        let ll = plan.light_leak.expect("light-leak enabled with positive intensity");
        assert!((ll.peak_intensity - 0.4).abs() < 1e-6);
        assert_eq!(ll.source_intensity_kfs.len(), 2);
        assert_eq!(ll.target_intensity_kfs.len(), 2);
        assert!(ll.source_intensity_kfs[0].value.abs() < 1e-6);
        assert!((ll.source_intensity_kfs[1].value - 0.4).abs() < 1e-6);
        assert!((ll.target_intensity_kfs[0].value - 0.4).abs() < 1e-6);
        assert!(ll.target_intensity_kfs[1].value.abs() < 1e-6);
        // Lock-step with scale ramp.
        for (a, b) in plan.source_scale_kfs.iter().zip(&ll.source_intensity_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
        for (a, b) in plan.target_scale_kfs.iter().zip(&ll.target_intensity_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
    }

    #[test]
    fn light_leak_picks_up_hold_segment_three_keyframes() {
        let plan = plan_hyperzoom(input_with_leak(HyperzoomEasing::Linear, 0.2, true, 0.4));
        let ll = plan.light_leak.unwrap();
        assert_eq!(ll.source_intensity_kfs.len(), 3);
        assert_eq!(ll.target_intensity_kfs.len(), 3);
        // Source held tail + target held head at peak intensity.
        assert!((ll.source_intensity_kfs[1].value - 0.4).abs() < 1e-6);
        assert!((ll.source_intensity_kfs[2].value - 0.4).abs() < 1e-6);
        assert!((ll.target_intensity_kfs[0].value - 0.4).abs() < 1e-6);
        assert!((ll.target_intensity_kfs[1].value - 0.4).abs() < 1e-6);
    }

    #[test]
    fn light_leak_inherits_bezier_easing_on_ramp_endpoints() {
        let plan = plan_hyperzoom(input_with_leak(HyperzoomEasing::Bezier, 0.0, true, 0.4));
        let ll = plan.light_leak.unwrap();
        for kf in ll.source_intensity_kfs.iter().chain(&ll.target_intensity_kfs) {
            assert!(matches!(kf.interpolation, Interpolation::Bezier));
        }
    }

    // ──────────────── Auto-link target collection ────────────────

    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Track, TrackKind, TransformLink};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn media_clip(timeline_in_f: i64, timeline_out_f: i64) -> Clip {
        // Source-time placeholders: irrelevant for auto-link, the
        // helper keys off timeline_in/timeline_out only.
        Clip::media(MediaId::new(), tc(timeline_in_f), tc(timeline_out_f), tc(0), tc(timeline_out_f - timeline_in_f))
    }

    fn track_with_clips(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = clips;
        t
    }

    fn seq_with_tracks(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        // `new_default` already supplies Video 1 + Audio 1; replace them
        // wholesale so the test sequence has exactly the tracks the
        // test wants in the order it wants.
        s.tracks = tracks;
        s
    }

    #[test]
    fn auto_link_collects_overlapping_clip_on_lower_track() {
        // V1: shot A spans [0..120 frames @ 24 fps] = 5 s.
        // V2: source title spans [0..120] = 5 s.
        // Ramp: 0.5 s = window [4.5 s .. 5.0 s]. Shot A overlaps.
        let shot_a = media_clip(0, 120);
        let title = media_clip(0, 120);
        let title_id = title.id;
        let shot_a_id = shot_a.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert_eq!(targets, vec![shot_a_id]);
    }

    #[test]
    fn auto_link_skips_clips_above_source_track() {
        // V1: source title (lower).
        // V2: shot above the source (would render ON TOP — not "beneath").
        let title = media_clip(0, 120);
        let title_id = title.id;
        let shot_above = media_clip(0, 120);
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![title]),
            track_with_clips("V2", vec![shot_above]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_skips_clip_already_linked() {
        // Shot A on V1 has an existing transform_link (user manually
        // parented it earlier). Auto-link must not overwrite —
        // running it twice is then idempotent.
        let mut shot_a = media_clip(0, 120);
        shot_a.transform_link = Some(TransformLink::new(klipp_core::ClipId::new()));
        let title = media_clip(0, 120);
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_skips_non_overlapping_clip() {
        // Shot A ends at frame 60 (2.5 s). Source title is at
        // [60..120] (2.5..5 s). Ramp window = [4.5 s .. 5.0 s].
        // Shot A doesn't overlap the ramp window.
        let shot_a = media_clip(0, 60);
        let title = media_clip(60, 120);
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_includes_clip_that_starts_inside_ramp_window() {
        // Shot A starts at frame 110 (≈ 4.58 s) — inside the ramp
        // window of [4.5 .. 5.0]. Should be included even though it
        // covers only part of the ramp.
        let shot_a = media_clip(110, 130);
        let title = media_clip(0, 120);
        let title_id = title.id;
        let shot_a_id = shot_a.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert_eq!(targets, vec![shot_a_id]);
    }

    #[test]
    fn auto_link_skips_adjustment_layer_and_gap() {
        // Both AdjustmentLayer and Gap clip kinds have no own
        // transform — auto-linking either is a useless mutation.
        let mut adj = media_clip(0, 120);
        adj.kind = klipp_timeline::ClipKind::AdjustmentLayer;
        let mut gap = media_clip(0, 120);
        gap.kind = klipp_timeline::ClipKind::Gap;
        let title = media_clip(0, 120);
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![adj]),
            track_with_clips("V2", vec![gap]),
            track_with_clips("V3", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_skips_self_target_cycle() {
        // Edge case: source title already links → some clip on V1.
        // Auto-link tries to bind that V1 clip → source title; this
        // would close a cycle and must be rejected.
        let shot_a = media_clip(0, 120);
        let shot_a_id = shot_a.id;
        let mut title = media_clip(0, 120);
        title.transform_link = Some(TransformLink::new(shot_a_id));
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty(), "rejecting bind would close source ↔ shot_a cycle");
    }

    #[test]
    fn auto_link_returns_empty_when_source_missing() {
        // Stale dispatch (clip removed between dispatch and handler).
        let title = media_clip(0, 120);
        let seq = seq_with_tracks(vec![track_with_clips("V1", vec![title])]);
        let targets = collect_auto_link_targets(&seq, klipp_core::ClipId::new(), 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_returns_empty_for_zero_ramp() {
        // Pathological ramp_secs (0 or negative). No clip is "during"
        // a zero-length window; the helper bails cleanly.
        let shot_a = media_clip(0, 120);
        let title = media_clip(0, 120);
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        assert!(collect_auto_link_targets(&seq, title_id, 0.0).is_empty());
        assert!(collect_auto_link_targets(&seq, title_id, -1.0).is_empty());
    }

    // ────────── WhooshChoice round-trip + labels ──────────

    #[test]
    fn whoosh_choice_u8_round_trip() {
        for c in WhooshChoice::ALL {
            assert_eq!(WhooshChoice::from_u8(c.as_u8()), c);
        }
    }

    #[test]
    fn whoosh_choice_unknown_tag_falls_back_to_medium() {
        // Mirrors the AppCommand handler's
        // "stale-dispatch never produces silence" contract —
        // unknown wire-format tags coerce to Medium so the combo
        // shows a sensible default if a stale value lands here.
        for tag in [3u8, 7, 100, 254] {
            assert_eq!(WhooshChoice::from_u8(tag), WhooshChoice::Medium);
        }
    }

    #[test]
    fn whoosh_choice_labels_distinct_and_non_empty() {
        let labels: Vec<&'static str> =
            WhooshChoice::ALL.iter().map(|c| c.label()).collect();
        for l in &labels {
            assert!(!l.is_empty());
        }
        // Distinct
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len());
    }

    // ────────── Soft-warning threshold ──────────

    #[test]
    fn whoosh_warning_none_when_length_unknown() {
        assert!(whoosh_custom_length_warning(None).is_none());
    }

    #[test]
    fn whoosh_warning_none_at_or_below_threshold() {
        assert!(whoosh_custom_length_warning(Some(0.0)).is_none());
        assert!(whoosh_custom_length_warning(Some(1.0)).is_none());
        assert!(whoosh_custom_length_warning(Some(WHOOSH_CUSTOM_LENGTH_WARN_SECS)).is_none());
    }

    #[test]
    fn whoosh_warning_fires_above_threshold() {
        let w = whoosh_custom_length_warning(Some(6.5));
        let msg = w.expect("over-threshold should warn");
        assert!(msg.contains("6.5"), "warning includes the actual length");
        assert!(
            msg.to_ascii_lowercase().contains("long"),
            "warning frames the length as long: {msg}",
        );
    }

    #[test]
    fn whoosh_warning_strict_inequality_at_boundary() {
        // Pin: strict greater-than. A file exactly at the
        // threshold doesn't warn — keeps the warning rare and
        // signal-bearing.
        assert!(whoosh_custom_length_warning(Some(5.0)).is_none());
        assert!(whoosh_custom_length_warning(Some(5.000_001)).is_some());
    }

    // ────────── WAV duration probe ──────────

    fn write_pcm16_wav(path: &std::path::Path, sample_rate: u32, channels: u16, samples: u32) {
        use std::io::Write;
        let bytes_per_sample = 2u32;
        let data_size = samples * channels as u32 * bytes_per_sample;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_size).to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&channels.to_le_bytes()).unwrap();
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&(sample_rate * channels as u32 * bytes_per_sample).to_le_bytes()).unwrap();
        f.write_all(&(channels * bytes_per_sample as u16).to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();
        let zero = vec![0u8; data_size as usize];
        f.write_all(&zero).unwrap();
    }

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("klipp-wav-probe-{name}"));
        p
    }

    #[test]
    fn probe_wav_duration_matches_synthesised_length() {
        // 1.5 s @ 48 kHz mono → 72 000 frames.
        let path = tmp_path("klipp-1500ms-mono.wav");
        write_pcm16_wav(&path, 48_000, 1, 72_000);
        let dur = probe_wav_duration_secs(&path).expect("probe wav");
        assert!((dur - 1.5).abs() < 0.001, "got {dur}, want ~1.5");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn probe_wav_handles_stereo_and_44100() {
        // 0.5 s @ 44.1 kHz stereo → 22 050 frames; the probe
        // must divide out channels correctly.
        let path = tmp_path("klipp-500ms-stereo.wav");
        write_pcm16_wav(&path, 44_100, 2, 22_050);
        let dur = probe_wav_duration_secs(&path).expect("probe wav");
        assert!((dur - 0.5).abs() < 0.001, "got {dur}, want ~0.5");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn probe_returns_none_for_non_wav() {
        let path = tmp_path("not-a-wav.txt");
        std::fs::write(&path, b"this is not a RIFF file").unwrap();
        assert!(probe_wav_duration_secs(&path).is_none());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn probe_returns_none_for_missing_file() {
        let path = tmp_path("definitely-does-not-exist.wav");
        let _ = std::fs::remove_file(&path);
        assert!(probe_wav_duration_secs(&path).is_none());
    }

    #[test]
    fn probe_handles_klipp_synth_output() {
        // The synth's own output should round-trip through our
        // probe — the dogfood test.
        let path = tmp_path("klipp-synth-medium.wav");
        klipp_audio::write_whoosh_wav(klipp_audio::WhooshVariant::Medium, &path).unwrap();
        let dur = probe_wav_duration_secs(&path).expect("probe synth");
        // Medium variant = 0.50 s. Allow 1 ms slack for
        // sample-count rounding.
        assert!((dur - 0.50).abs() < 0.001, "got {dur}, want ~0.50");
        std::fs::remove_file(&path).ok();
    }

    // ────────── HyperzoomScratch whoosh defaults + round-trip ──────────

    #[test]
    fn scratch_default_whoosh_is_disabled_short() {
        // Pin: defaults match the AppCommand back-compat path.
        // Combo opens at Short so flipping the checkbox on is
        // instantly meaningful — no "(none selected)" blank.
        let s = HyperzoomScratch::default();
        assert!(!s.whoosh_enabled);
        assert_eq!(s.whoosh_variant, WhooshChoice::Short.as_u8());
    }

    #[test]
    fn scratch_round_trips_whoosh_fields_through_egui_memory() {
        // Toggle checkbox + pick Long via update_hyperzoom_scratch;
        // read back via read_hyperzoom_scratch; verify the values
        // survived the memory round-trip. Pinned because every
        // surface that reads HyperzoomScratch (inspector, viewer
        // crosshair, future palette) depends on this contract.
        let ctx = egui::Context::default();
        let cid = ClipId::new();
        // Default before any write.
        let initial = read_hyperzoom_scratch(&ctx, cid);
        assert!(!initial.whoosh_enabled);
        // Write non-default values.
        update_hyperzoom_scratch(&ctx, cid, |s| {
            s.whoosh_enabled = true;
            s.whoosh_variant = WhooshChoice::Long.as_u8();
        });
        let after = read_hyperzoom_scratch(&ctx, cid);
        assert!(after.whoosh_enabled);
        assert_eq!(after.whoosh_variant, WhooshChoice::Long.as_u8());
    }

    #[test]
    fn whoosh_custom_state_round_trips_through_egui_memory() {
        let ctx = egui::Context::default();
        let cid = ClipId::new();
        // Empty before any write.
        let initial = read_whoosh_custom(&ctx, cid);
        assert!(initial.path.is_none());
        assert!(initial.length_secs.is_none());
        // Write path + length.
        let path = std::path::PathBuf::from("/tmp/user-custom.wav");
        update_whoosh_custom(&ctx, cid, |s| {
            s.path = Some(path.clone());
            s.length_secs = Some(0.7);
        });
        let after = read_whoosh_custom(&ctx, cid);
        assert_eq!(after.path.as_deref(), Some(path.as_path()));
        assert_eq!(after.length_secs, Some(0.7));
    }

    #[test]
    fn whoosh_custom_state_isolated_per_clip() {
        // Two clip ids must not share scratch — pinning the
        // egui::Id keying contract.
        let ctx = egui::Context::default();
        let a = ClipId::new();
        let b = ClipId::new();
        update_whoosh_custom(&ctx, a, |s| {
            s.path = Some(std::path::PathBuf::from("/tmp/a.wav"));
            s.length_secs = Some(0.2);
        });
        let b_read = read_whoosh_custom(&ctx, b);
        assert!(b_read.path.is_none(), "clip B sees no leak from clip A");
    }

    // ────────── Apply-button field construction ──────────

    #[test]
    fn apply_fields_disabled_returns_canonical_defaults() {
        // Checkbox off → all three fields default regardless of
        // what the variant tag or custom path happen to hold (the
        // user might have toggled off after picking a custom
        // file; we don't ship a stale path).
        let scratch = HyperzoomScratch {
            whoosh_enabled: false,
            whoosh_variant: WhooshChoice::Custom.as_u8(),
            ..Default::default()
        };
        let custom = WhooshCustomState {
            path: Some(std::path::PathBuf::from("/tmp/stale.wav")),
            length_secs: Some(2.0),
        };
        let (add_whoosh, variant, path) = build_whoosh_fields_for_apply(&scratch, &custom);
        assert!(!add_whoosh);
        // Variant tag is preserved (it's just the combo's value);
        // the AppCommand handler ignores it when add_whoosh is
        // false anyway.
        assert_eq!(variant, WhooshChoice::Custom.as_u8());
        // BUT — the custom path is suppressed, mirroring the
        // off-state byte-identical behaviour.
        assert!(path.is_none(), "stale custom path not attached when checkbox off");
    }

    #[test]
    fn apply_fields_enabled_builtin_passes_no_custom_path() {
        // Checkbox on + variant = Medium → wire-tag 1, path None
        // even when the custom-state slot has stale data from a
        // previous Custom selection.
        let scratch = HyperzoomScratch {
            whoosh_enabled: true,
            whoosh_variant: WhooshChoice::Medium.as_u8(),
            ..Default::default()
        };
        let custom = WhooshCustomState {
            path: Some(std::path::PathBuf::from("/tmp/old-custom.wav")),
            length_secs: Some(0.4),
        };
        let (add_whoosh, variant, path) = build_whoosh_fields_for_apply(&scratch, &custom);
        assert!(add_whoosh);
        assert_eq!(variant, 1);
        assert!(path.is_none());
    }

    #[test]
    fn apply_fields_enabled_custom_passes_path_when_picked() {
        let scratch = HyperzoomScratch {
            whoosh_enabled: true,
            whoosh_variant: WhooshChoice::Custom.as_u8(),
            ..Default::default()
        };
        let custom_path = std::path::PathBuf::from("/tmp/user-pick.wav");
        let custom = WhooshCustomState {
            path: Some(custom_path.clone()),
            length_secs: Some(0.6),
        };
        let (add_whoosh, variant, path) = build_whoosh_fields_for_apply(&scratch, &custom);
        assert!(add_whoosh);
        assert_eq!(variant, 255);
        assert_eq!(path.as_deref(), Some(custom_path.as_path()));
    }

    #[test]
    fn apply_fields_enabled_custom_with_no_pick_yet_passes_none() {
        // User opened the inspector, ticked the checkbox, picked
        // Custom in the combo, but hasn't actually clicked the
        // file picker yet. Apply must surface this as path=None;
        // the AppCommand handler then logs a warning and skips
        // the whoosh insertion (commit 2 contract).
        let scratch = HyperzoomScratch {
            whoosh_enabled: true,
            whoosh_variant: WhooshChoice::Custom.as_u8(),
            ..Default::default()
        };
        let custom = WhooshCustomState::default();
        let (add_whoosh, variant, path) = build_whoosh_fields_for_apply(&scratch, &custom);
        assert!(add_whoosh);
        assert_eq!(variant, 255);
        assert!(path.is_none());
    }

    // ────────── Chromatic + light-leak scratch defaults ──────────

    #[test]
    fn scratch_default_chromatic_disabled_amount_is_six() {
        // Audit (E) pin: scratch defaults are visibly-meaningful
        // first-toggle-on values, NOT 0.0. The AppCommand call
        // sites pass 0.0 (off-state magnitude irrelevant); the
        // inspector's scratch carries 6.0 so the moment the user
        // ticks the box, the result reads visually.
        let s = HyperzoomScratch::default();
        assert!(!s.add_chromatic_aberration);
        assert!((s.chromatic_aberration_amount - 6.0).abs() < 1e-6);
    }

    #[test]
    fn scratch_default_light_leak_disabled_intensity_is_point_four() {
        let s = HyperzoomScratch::default();
        assert!(!s.add_light_leak);
        assert!((s.light_leak_intensity - 0.4).abs() < 1e-6);
    }

    #[test]
    fn scratch_round_trips_chromatic_and_light_leak_through_egui_memory() {
        let ctx = egui::Context::default();
        let cid = ClipId::new();
        // Default before any write — uses the audit (E) values.
        let initial = read_hyperzoom_scratch(&ctx, cid);
        assert!(!initial.add_chromatic_aberration);
        assert!(!initial.add_light_leak);
        assert!((initial.chromatic_aberration_amount - 6.0).abs() < 1e-6);
        assert!((initial.light_leak_intensity - 0.4).abs() < 1e-6);
        // Toggle on + tweak amounts.
        update_hyperzoom_scratch(&ctx, cid, |s| {
            s.add_chromatic_aberration = true;
            s.chromatic_aberration_amount = 12.5;
            s.add_light_leak = true;
            s.light_leak_intensity = 0.65;
        });
        let after = read_hyperzoom_scratch(&ctx, cid);
        assert!(after.add_chromatic_aberration);
        assert!(after.add_light_leak);
        assert!((after.chromatic_aberration_amount - 12.5).abs() < 1e-6);
        assert!((after.light_leak_intensity - 0.65).abs() < 1e-6);
    }

    #[test]
    fn scratch_chromatic_and_light_leak_isolated_per_clip() {
        // Two clip ids must not share scratch — same key contract
        // as the existing whoosh fields.
        let ctx = egui::Context::default();
        let a = ClipId::new();
        let b = ClipId::new();
        update_hyperzoom_scratch(&ctx, a, |s| {
            s.add_chromatic_aberration = true;
            s.chromatic_aberration_amount = 10.0;
        });
        let b_read = read_hyperzoom_scratch(&ctx, b);
        assert!(!b_read.add_chromatic_aberration);
        assert!((b_read.chromatic_aberration_amount - 6.0).abs() < 1e-6,
            "clip B sees the default 6.0, no leak from clip A");
    }

    #[test]
    fn first_toggle_on_uses_audit_defaults_not_zero() {
        // The contract: when the user ticks the chromatic checkbox
        // for the first time on a fresh clip, the Apply button
        // must construct ApplyHyperzoom with amount=6.0 (audit E),
        // NOT 0.0. If the inspector defaulted scratch to 0.0
        // instead of 6.0, the first toggle-on apply would be a
        // visual no-op even though the user opted in. This test
        // pins the visibly-meaningful contract.
        // No other changes — no DragValue interaction yet.
        let scratch = HyperzoomScratch {
            add_chromatic_aberration: true, // user just ticked
            ..Default::default()
        };
        assert!(scratch.chromatic_aberration_amount > 0.5,
            "first-toggle-on must pass a meaningful amount, got {}",
            scratch.chromatic_aberration_amount);

        let scratch2 = HyperzoomScratch {
            add_light_leak: true,
            ..Default::default()
        };
        assert!(scratch2.light_leak_intensity > 0.05,
            "first-toggle-on must pass a meaningful intensity, got {}",
            scratch2.light_leak_intensity);
    }
}
