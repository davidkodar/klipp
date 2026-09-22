//! Intent in, geometry out: an edit said in editorial terms, resolved
//! against the document into the recorded commands that make it, with a
//! reason when it cannot be made.
//!
//! An [`Op`] names clips and media by id and places by an [`Anchor`]: a
//! time, a frame, a clip's edge, a marker, an offset from one of those.
//! [`resolve`] runs the ops in order over a copy of the document — each
//! op sees what the ones before it did — snapping every time to a frame,
//! and returns a [`Plan`]: one [`Step`] per op with the [`EditCommand`]
//! that does it and a sentence saying what. A host applies the plan's
//! commands through [`crate::apply::DocumentView::apply_forward`] inside
//! one history batch, so a plan is one undo step; or shows the plan and
//! applies nothing, which is the dry run. Because the same commands are
//! replayed, what the dry run said is what the apply does.
//!
//! A [`Refusal`] says which step, why, and what would make it possible.

use crate::history::{EditCommand, LinkedMovePartner};
use klipp_core::{ClipId, Fps, MediaId, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};
use serde::{Deserialize, Serialize};

/// A point on the timeline, said the way an editor says it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// Seconds from the start of the timeline.
    Secs(f64),
    /// A frame number at the sequence's rate.
    Frame(i64),
    /// Where a clip starts: the clip by id, or by where it is, which is
    /// how a step names a clip an earlier step of the same plan placed.
    ClipStart(ClipRef),
    /// Where a clip ends.
    ClipEnd(ClipRef),
    /// Where the last clip on a track ends (the start, on an empty
    /// track): where the next select of an assembly goes.
    TrackEnd(TrackRef),
    /// A timeline marker, by its label (case-insensitive).
    Marker(String),
    /// Another anchor moved by some seconds, negative for earlier.
    Offset { from: Box<Anchor>, secs: f64 },
}

/// A clip, by its id or by where it is: the clip covering `secs` on
/// track `track`, as the document stands when the op runs. The latter
/// is how a step names the halves a split before it made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClipRef {
    Id(ClipId),
    At { track: usize, secs: f64 },
}

/// Which track a clip goes on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TrackRef {
    /// A track's index in the sequence, top first, as the views list them.
    Index(usize),
    /// The first track of a kind: "video", "audio", "text".
    Kind(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    In,
    Out,
}

/// One editorial operation.
// The `title` variant carries many optional style/motion fields, so it is
// larger than the rest; ops are resolved one at a time, never held in bulk,
// so the size disparity does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// A media item onto a track at an anchor, the whole file unless
    /// `source_in` and `source_out` (seconds in the file) say otherwise.
    /// For still images these name the hold range: the import duration
    /// is a default, not a source limit.
    /// Refused where it would overlap a clip already on the track —
    /// unless `insert`, which makes room: every unlocked track is
    /// rippled later by the clip's length from the anchor, and a clip
    /// straddling the anchor is split there.
    Place {
        media_id: MediaId,
        track: TrackRef,
        at: Anchor,
        #[serde(default)]
        source_in: Option<f64>,
        #[serde(default)]
        source_out: Option<f64>,
        #[serde(default)]
        insert: bool,
        /// The file's sound with it, as a linked clip on the first audio
        /// track (made if there is none), at the same place. On by
        /// default; off for B-roll over a voice.
        #[serde(default = "default_true")]
        with_audio: bool,
    },
    /// A track: a video or text track goes on top (it becomes track 0
    /// and every other index moves down by one), an audio track at the
    /// bottom.
    AddTrack {
        kind: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// One edge of a clip to an anchor. The clip's other edge stays;
    /// the source moves with the edge, so the picture at the other edge
    /// does not change. Refused past the media's start or end, or into
    /// a neighbour — unless `ripple`, which moves the later clips on the
    /// track by the change in the clip's length (a head trim then keeps
    /// the clip where it starts and shortens it from the front).
    Trim {
        clip_id: ClipRef,
        edge: Edge,
        to: Anchor,
        #[serde(default)]
        ripple: bool,
        /// The clip it is linked to — a picture's sound, a sound's
        /// picture — is trimmed with it by the same amount. On by
        /// default; off to move one edge against the other on purpose
        /// (a J-cut or an L-cut says that better).
        #[serde(default = "default_true")]
        linked: bool,
    },
    /// A J-cut: the sound of `incoming_clip` starts `frames` before its
    /// picture, over the end of the clip before it. Both clips need
    /// linked audio, adjacent on one audio track: the outgoing audio is
    /// shortened by `frames`, the incoming audio starts that much earlier.
    JCut { incoming_clip: ClipRef, frames: i64 },
    /// An L-cut: the sound of `outgoing_clip` stays `frames` past its
    /// picture, under the start of the clip after it. The outgoing audio
    /// runs on by `frames`, the incoming audio starts that much later.
    LCut { outgoing_clip: ClipRef, frames: i64 },
    /// A clip into two at an anchor strictly inside it.
    Split { clip_id: ClipRef, at: Anchor },
    /// A clip off its track; with `ripple`, the clips after it on that
    /// track close the gap.
    Remove {
        clip_id: ClipRef,
        #[serde(default)]
        ripple: bool,
    },
    /// A clip's start to an anchor, its length unchanged; a linked audio
    /// or video partner goes with it. Refused into another clip.
    Move { clip_id: ClipRef, to: Anchor },
    /// Captions for what is said on the timeline, from the transcripts of
    /// the clips whose sound is heard: each word lands where its clip
    /// puts it, words gather into cues of at most `max_lines` lines of
    /// `max_chars`, a cue ends at a sentence, a pause or a cut. They go
    /// on a subtitle track (the first, made at the bottom if there is
    /// none, or `track`), between `from` and `to` when given.
    Captions {
        #[serde(default)]
        track: Option<TrackRef>,
        #[serde(default)]
        from: Option<Anchor>,
        #[serde(default)]
        to: Option<Anchor>,
        #[serde(default = "default_caption_chars")]
        max_chars: usize,
        #[serde(default = "default_caption_lines")]
        max_lines: usize,
        /// Take away the captions that are there first (on that track,
        /// in that range), so they can be made again after a re-cut or a
        /// better transcript. Without it, captions already there are in
        /// the way and the op is refused.
        #[serde(default)]
        replace: bool,
    },
    /// A clip's sound: its volume and pre-fader gain in decibels, its
    /// pan (-1 left to 1 right), a fade in and out in seconds. What is
    /// left out stays as it is. For an audio clip linked to a video
    /// clip the settings live on the video clip, where the app and the
    /// render read them, and that is the clip changed.
    Audio {
        clip_id: ClipRef,
        #[serde(default)]
        volume_db: Option<f32>,
        #[serde(default)]
        gain_db: Option<f32>,
        #[serde(default)]
        pan: Option<f32>,
        #[serde(default)]
        fade_in: Option<f32>,
        #[serde(default)]
        fade_out: Option<f32>,
    },
    /// Music under speech: the volume of `clip_id` drops by `by_db`
    /// (9 by default) wherever another clip is heard over it, moving
    /// there and back over `fade_secs` (0.5 by default), never as a step.
    /// Written as volume keyframes on the clip; any it had are replaced.
    Duck {
        clip_id: ClipRef,
        #[serde(default = "default_duck_db")]
        by_db: f32,
        #[serde(default = "default_duck_fade")]
        fade_secs: f64,
    },
    /// A lower third: `name`, and `role` under it, bottom left inside
    /// title-safe, for `secs` from `at`, fading in and out. It goes on a
    /// text track where it fits (one is made on top if none does).
    /// Refused across a shot change. The name comes from the brief or
    /// the cast list, never from a guess.
    ///
    /// Where the speaker is not on the left, or the picture has a name
    /// of its own burnt in down there (a split-screen podcast), it is
    /// put where it belongs: `side` "right" sets it against the right
    /// edge, `x` is that edge and `y` the name's line, both as a
    /// fraction of the frame and kept inside title-safe.
    LowerThird {
        name: String,
        #[serde(default)]
        role: Option<String>,
        at: Anchor,
        #[serde(default = "default_lower_third_secs")]
        secs: f64,
        #[serde(default)]
        track: Option<TrackRef>,
        #[serde(default)]
        side: Option<String>,
        #[serde(default)]
        x: Option<f32>,
        #[serde(default)]
        y: Option<f32>,
    },
    /// The words of a title or a caption: a cue the transcript misheard,
    /// a name spelt as the cast list spells it. The first text box of
    /// the clip takes `text`; its look, place and timing stay.
    SetText { clip_id: ClipRef, text: String },
    /// A clip's colour grade, as a professional would name it: what is
    /// given is set on the clip's primary grade, the rest is left. Every
    /// knob is zero at neutral, save `saturation` and `contrast`, whose
    /// neutral is one. `exposure` is in stops; `temperature` and `tint`
    /// warm/cool and green/magenta the white balance, in mireds of
    /// assumed light, -50 to 50 as the app's own sliders read (about 5
    /// Lab b* at 25, 10 at 50 — a tenth of a unit does nothing);
    /// `lift`, `gamma`,
    /// `gain` move the shadows, mids and highlights (their master);
    /// `saturation` at zero is grey, at one is as shot, above one is more.
    Grade {
        clip_id: ClipRef,
        #[serde(default)]
        exposure: Option<f32>,
        #[serde(default)]
        contrast: Option<f32>,
        #[serde(default)]
        saturation: Option<f32>,
        #[serde(default)]
        temperature: Option<f32>,
        #[serde(default)]
        tint: Option<f32>,
        #[serde(default)]
        lift: Option<f32>,
        #[serde(default)]
        gamma: Option<f32>,
        #[serde(default)]
        gain: Option<f32>,
        #[serde(default)]
        highlights: Option<f32>,
        #[serde(default)]
        shadows: Option<f32>,
        /// Saturation that protects skin and already-saturated colours:
        /// the pro way to add life without turning faces orange. Zero
        /// neutral.
        #[serde(default)]
        vibrance: Option<f32>,
        /// Take the grade back to neutral first, then set what is given.
        #[serde(default)]
        reset: bool,
    },
    /// A finishing effect on a picture clip. `kind` is "vignette" (darken
    /// the edges), "grain" (film grain), "glow" (soften the highlights, a
    /// dreamy diffusion), "blur", or "sharpen". `amount` is 0 to 1, the
    /// strength; left out, a tasteful default. A second effect of the
    /// same kind replaces the first.
    Effect {
        clip_id: ClipRef,
        kind: String,
        #[serde(default)]
        amount: Option<f32>,
    },
    /// Hide a patch of the picture — an email, a key, a face — for a
    /// stretch of a clip: a cropped copy of the same footage goes on a
    /// video track above (one is made on top if none has room), covering
    /// `x`,`y`,`w`,`h` of the frame (fractions, from the top left) from
    /// `from` to `to` (the clip's own edges when left out), pixelated
    /// (`kind` "mosaic", the default) or blurred (`kind` "blur"), by
    /// `amount` 0 to 1. The copy follows nothing: move the clip and
    /// redact again. Refused on a retimed clip.
    Redact {
        clip_id: ClipRef,
        #[serde(default)]
        from: Option<Anchor>,
        #[serde(default)]
        to: Option<Anchor>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        #[serde(default = "default_redact_kind")]
        kind: String,
        #[serde(default)]
        amount: Option<f32>,
    },
    /// Move what a clip shows without moving the clip: its source in and
    /// out both by `secs` (later in the file for positive), the timeline
    /// untouched. Refused past the file's start or end.
    Slip { clip_id: ClipRef, secs: f64 },
    /// What a clip is to the mix and the checks: "dialogue", "music",
    /// "sfx", "ambience", "foley", "voiceover", "broll" or "graphics". A
    /// music bed and a sound effect are told from speech by this, so
    /// they are never taken for speech and speech is never ducked.
    Role { clip_id: ClipRef, role: String },
    /// The sequence's picture size, and its rate when given: what the
    /// delivery is. The picture is fitted, never stretched. The brief's
    /// size, when the footage is another.
    Format {
        width: u32,
        height: u32,
        #[serde(default)]
        fps: Option<f64>,
    },
    /// Steady a shaky clip. `smoothing` 0 to 1 is how much camera motion
    /// is ironed out (0.5 default); the frame is cropped a little to hide
    /// the edges it moves.
    Stabilize {
        clip_id: ClipRef,
        #[serde(default = "default_stabilize")]
        smoothing: f32,
    },
    /// Reframe a picture: `scale` zooms in (1 is as shot, 1.5 is a
    /// tighter crop), `x` and `y` pan the frame (0 centre, -1 to 1) as a
    /// share of how far the scale lets it go — at 1 the picture's edge
    /// meets the frame's, so it never shows past it; a pan needs a scale
    /// above 1. Keeps the subject in frame when the delivery aspect is
    /// not the footage's — a wide interview cut for a vertical feed. Read
    /// `frame_stats` or the framing analysis to know where the subject is.
    Reframe {
        clip_id: ClipRef,
        #[serde(default = "default_reframe_scale")]
        scale: f32,
        #[serde(default)]
        x: f32,
        #[serde(default)]
        y: f32,
    },
    /// Animate a picture's zoom over the clip: scale `from` at the start
    /// to `to` at the end — a slow push-in (`from` 1, `to` 1.15) or a
    /// pull-out (`from` 1.2, `to` 1). Optionally push toward `x`,`y`
    /// (-1..1, measured as `reframe` measures them) for a Ken Burns move. 1 is as shot, up to 4. Renders as
    /// per-frame transform keyframes on the GPU; use it for energy on a
    /// static shot where a hard reframe would sit still.
    Zoom {
        clip_id: ClipRef,
        #[serde(default = "default_reframe_scale")]
        from: f32,
        to: f32,
        #[serde(default)]
        x: f32,
        #[serde(default)]
        y: f32,
    },
    /// An animated crop that follows the action: `keys` are poses in
    /// the clip's own seconds, each a `scale` (1 as shot, up to 4)
    /// centred on the frame point `x`,`y` (0..1, as the beat sheet's
    /// spots and screen text give them); the picture eases from one to
    /// the next and never shows past its edge. Transform keyframes on
    /// the GPU, like zoom.
    Pan {
        clip_id: ClipRef,
        keys: Vec<PanKey>,
    },
    /// A pan made from the beat sheet: at each spot where something
    /// happened inside the clip's stretch, the picture pushes to
    /// `scale` on it, holds `hold_secs`, and eases back out unless the
    /// next spot is near. Needs the file's beat sheet (analyze).
    Follow {
        clip_id: ClipRef,
        #[serde(default = "default_follow_scale")]
        scale: f32,
        #[serde(default = "default_follow_hold")]
        hold_secs: f64,
    },
    /// A highlight over the picture at `at` for `secs`: `kind` "box" (a
    /// rounded stroke round the region), "ring" (a capsule) or "spot"
    /// (everything outside it dimmed). The region is centred on `x`,`y`
    /// with size `w`×`h`, all fractions of the frame. `color` #rrggbb
    /// (the brand's first colour when left out), `stroke` in px.
    Highlight {
        at: Anchor,
        #[serde(default = "default_title_secs")]
        secs: f64,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        #[serde(default = "default_highlight_kind")]
        kind: String,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        stroke: Option<f32>,
        #[serde(default)]
        track: Option<TrackRef>,
    },
    /// A speed ramp over the clip: from `from`× to `to`× (0.1 to 8) over
    /// `secs` from the clip's start (the whole clip when left out), then
    /// holding `to`. The clip's length on the timeline follows.
    Ramp {
        clip_id: ClipRef,
        from: f64,
        to: f64,
        #[serde(default)]
        secs: Option<f64>,
    },
    /// Apply a creative look — a 3D LUT — to a picture clip, as one node
    /// after the grade so the look sits on the corrected image. `lut_path`
    /// is the resolved file; `name` is what to call it in the reply;
    /// `opacity` (default 1) fades the look in; `remove` clears it.
    Look {
        clip_id: ClipRef,
        #[serde(default)]
        lut_path: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        opacity: Option<f32>,
        #[serde(default)]
        remove: bool,
    },
    /// A secondary (HSL qualifier) grade on a picture clip: key a colour
    /// or tone range — by a named `target` (skin, sky, foliage, reds…) or
    /// an explicit `hue` with `hue_width`, and optional saturation and
    /// luminance windows — then correct only what is keyed with
    /// `exposure`, `saturation`, `hue_shift` or `temperature`. `invert`
    /// grades everything but the key; `remove` clears it.
    Qualify {
        clip_id: ClipRef,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        hue: Option<f32>,
        #[serde(default)]
        hue_width: Option<f32>,
        #[serde(default)]
        sat_low: Option<f32>,
        #[serde(default)]
        sat_high: Option<f32>,
        #[serde(default)]
        lum_low: Option<f32>,
        #[serde(default)]
        lum_high: Option<f32>,
        #[serde(default)]
        invert: bool,
        #[serde(default)]
        exposure: Option<f32>,
        #[serde(default)]
        saturation: Option<f32>,
        #[serde(default)]
        hue_shift: Option<f32>,
        #[serde(default)]
        temperature: Option<f32>,
        #[serde(default)]
        remove: bool,
        /// Hold the key inside a shape as well: the correction then
        /// reaches only what is both the right colour *and* in the right
        /// place. A hue key alone cannot tell a face from a red jumper,
        /// a brick wall or a wooden table — they are all in the skin
        /// band, and pulling the face warmer turns all of them.
        ///
        /// Each entry is a box in frame fractions at a time in clip
        /// seconds, as a face detector gives them. One entry is a still
        /// window; several are keyframed, so the window stays on a
        /// subject who moves.
        #[serde(default)]
        window: Option<Vec<WindowKey>>,
        /// How soft the window's edge is, 0 to 1. Generous by default:
        /// a hard-edged window on a face reads as a sticker the moment
        /// the subject moves at all.
        #[serde(default)]
        window_feather: Option<f32>,
    },
    /// Change a clip's speed. `speed` above 1 is faster and shorter, below
    /// 1 slower and longer (0.5 is half speed); the clips after it on the
    /// track keep their place, so a gap or an overlap is yours to close.
    Retime {
        clip_id: ClipRef,
        speed: f64,
    },
    /// Clean a clip's sound. Each flag adds the tool for it: `wind` a
    /// broadband reducer for wind and rumble, `hiss` the same lighter
    /// for tape or air hiss, `hum` a notch for 50/60 Hz mains buzz,
    /// `deess` a de-esser for harsh sibilance. Set on the clip whose
    /// sound plays (a linked video clip's, as elsewhere).
    CleanAudio {
        clip_id: ClipRef,
        #[serde(default)]
        wind: bool,
        #[serde(default)]
        hiss: bool,
        #[serde(default)]
        hum: bool,
        #[serde(default)]
        deess: bool,
    },
    /// A transition between `clip_id` and the clip after it on its track,
    /// `secs` long, centred on the cut. `kind` is a transition's name.
    /// `cross_dissolve` is the default, and the only one an interview or
    /// a documentary wants. `dip_to_black` and `dip_to_white` mark a
    /// break. A stylised one (`zoom`, `whip`, `glitch`, `film_burn`) a
    /// piece asks for by its style, never by default. Both clips need
    /// source beyond the cut to overlap; a dissolve on a clip with no
    /// handle there is refused.
    Transition {
        clip_id: ClipRef,
        #[serde(default = "default_transition_kind")]
        kind: String,
        #[serde(default = "default_transition_secs")]
        secs: f64,
    },
    /// The look and place of titles: one clip, or every title on a track
    /// (the captions, all at once), between `from` and `to` when given —
    /// which is how the captions are lifted clear of a lower third for
    /// as long as it is up. What is left out stays as it is. `size` is
    /// in pixels of a 1080-line frame; `color` is `#rrggbb` or
    /// `#rrggbbaa`; `x` and `y` are where the text's centre goes, 0 to 1
    /// of the frame, everything in the title moving with it; `backing`
    /// is how dark a panel sits behind the text as the eye sees it, 0
    /// (none) to 1 (black); `outline` is a width in pixels, 0 for none.
    Style {
        #[serde(default)]
        clip_id: Option<ClipRef>,
        #[serde(default)]
        track: Option<TrackRef>,
        #[serde(default)]
        from: Option<Anchor>,
        #[serde(default)]
        to: Option<Anchor>,
        #[serde(default)]
        size: Option<f32>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        font: Option<String>,
        #[serde(default)]
        bold: Option<bool>,
        #[serde(default)]
        align: Option<String>,
        #[serde(default)]
        x: Option<f32>,
        #[serde(default)]
        y: Option<f32>,
        #[serde(default)]
        backing: Option<f32>,
        #[serde(default)]
        outline: Option<f32>,
        /// A solid colour card behind everything, `#rrggbb`: a card's
        /// fill, changed after the fact.
        #[serde(default)]
        full_frame: Option<String>,
    },
    /// Create a title from scratch with any look and motion — the way to
    /// put a heading, a callout or a hero card on screen. `text` is the
    /// words; `at` and `secs` place and hold it; `track` names a text
    /// track (one is made on top if left out). Look: `x`,`y` centre the
    /// text (0..1), `size` in px of a 1080-line frame, `color` #rrggbb,
    /// `font`, `bold`, `align` (left/center/right); `backing` a dark
    /// panel behind the text (0..1) or `backing_color`, `full_frame` a
    /// solid colour card behind everything, `outline`+`outline_color` a
    /// stroke, `glow`+`glow_color` a neon glow, `gradient` a two-colour
    /// fill (#top,#bottom). Motion: `animate` an entrance (fade, rise,
    /// drop, slide_left, slide_right, scale_up, scale_down, blur,
    /// typewriter) over `animate_secs`, `stagger` (line/word/char) for
    /// per-unit timing; `loop` a continuous effect (pulse, flash/blink,
    /// neon, wave, shake, color_cycle); `glitch` and `chromatic` (0..1)
    /// add a digital edge. `style` names a look from the brief's type
    /// styles (or heading, callout, card): the look's fields fill every
    /// one left out here.
    Title {
        text: String,
        at: Anchor,
        #[serde(default)]
        style: Option<String>,
        #[serde(default = "default_title_secs")]
        secs: f64,
        #[serde(default)]
        track: Option<TrackRef>,
        #[serde(default)]
        x: Option<f32>,
        #[serde(default)]
        y: Option<f32>,
        #[serde(default)]
        size: Option<f32>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        font: Option<String>,
        #[serde(default)]
        bold: Option<bool>,
        #[serde(default)]
        align: Option<String>,
        #[serde(default)]
        backing: Option<f32>,
        #[serde(default)]
        backing_color: Option<String>,
        #[serde(default)]
        full_frame: Option<String>,
        #[serde(default)]
        outline: Option<f32>,
        #[serde(default)]
        outline_color: Option<String>,
        #[serde(default)]
        glow: Option<f32>,
        #[serde(default)]
        glow_color: Option<String>,
        #[serde(default)]
        gradient: Option<[String; 2]>,
        #[serde(default)]
        animate: Option<String>,
        #[serde(default)]
        animate_secs: Option<f32>,
        #[serde(default)]
        stagger: Option<String>,
        #[serde(default, rename = "loop")]
        loop_effect: Option<String>,
        #[serde(default)]
        glitch: Option<f32>,
        #[serde(default)]
        chromatic: Option<f32>,
    },
    /// Compose a picture into the frame rather than letting it fill the
    /// frame: `x`,`y` is where its centre sits and `width`,`height` how
    /// much of the frame it covers, all fractions, with an optional turn
    /// and fade. Two or more poses animate between them. `reframe` and
    /// `zoom` crop into a picture that still fills the frame; this is the
    /// one that puts a picture beside another, or shrinks it into a
    /// corner over a background.
    Compose {
        clip_id: ClipRef,
        poses: Vec<ComposePose>,
    },
    /// Move a camera over the whole sequence. Every layer inherits it,
    /// so one drift carries the scene rather than each clip being
    /// animated in step with the others — including layers that come and
    /// go during the move. `keys` are poses at seconds on the timeline,
    /// not in a clip; an empty list takes the camera away again.
    Camera {
        keys: Vec<CameraKey>,
    },
    /// Cut the picture down to part of itself: `left`, `right`, `top`
    /// and `bottom` are how much comes off each side, as fractions of
    /// the picture. What is cut away is transparent, so a screen
    /// recording cropped to its window and then composed is a card of
    /// that window, and `corners` rounds the window's corners. All four
    /// at 0 takes a crop off again.
    Crop {
        clip_id: ClipRef,
        #[serde(default)]
        left: f32,
        #[serde(default)]
        right: f32,
        #[serde(default)]
        top: f32,
        #[serde(default)]
        bottom: f32,
    },
    /// Place a native generator. Omit track to use/create a bottom
    /// Background video track. Give exactly one of secs or end; end can
    /// be a clip_end anchor so a background matches its picture exactly.
    Generator {
        data: klipp_core::GeneratorData,
        at: Anchor,
        #[serde(default)]
        secs: Option<f64>,
        #[serde(default)]
        end: Option<Anchor>,
        #[serde(default)]
        track: Option<TrackRef>,
    },
}

/// What a `style` op changes, gathered.
#[derive(Debug, Clone, Default, PartialEq)]
struct Look {
    full_frame: Option<[u8; 4]>,
    size: Option<f32>,
    color: Option<[u8; 4]>,
    font: Option<String>,
    bold: Option<bool>,
    align: Option<klipp_timeline::TextAlignment>,
    x: Option<f32>,
    y: Option<f32>,
    backing: Option<f32>,
    outline: Option<f32>,
}

fn default_caption_chars() -> usize {
    42
}

fn default_caption_lines() -> usize {
    2
}

fn default_lower_third_secs() -> f64 {
    4.0
}

fn default_transition_kind() -> String {
    "cross_dissolve".to_string()
}

fn default_transition_secs() -> f64 {
    0.5
}

fn default_stabilize() -> f32 {
    0.5
}

fn default_redact_kind() -> String {
    "mosaic".into()
}

fn default_highlight_kind() -> String {
    "box".into()
}

fn default_follow_scale() -> f32 {
    1.6
}

fn default_follow_hold() -> f64 {
    1.2
}

/// One pose of the camera: at `secs` on the timeline, looking at the
/// frame point `x`,`y`, `zoom` far in and rolled `roll_deg` clockwise.
/// It reads as a camera, so moving it right moves the scene left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CameraKey {
    #[serde(default)]
    pub secs: f64,
    #[serde(default = "half")]
    pub x: f32,
    #[serde(default = "half")]
    pub y: f32,
    #[serde(default = "one")]
    pub zoom: f32,
    #[serde(default)]
    pub roll_deg: f32,
}

impl Default for CameraKey {
    fn default() -> Self {
        Self { secs: 0.0, x: 0.5, y: 0.5, zoom: 1.0, roll_deg: 0.0 }
    }
}

/// One pose of a composed picture: at `secs` into the clip, its centre
/// on the frame point `x`,`y` covering `width` by `height` of the frame,
/// turned `rotation_deg` clockwise about that centre and `opacity` seen.
/// All of it is fractions of the frame, so the same pose means the same
/// thing whatever the delivery size. `x` and `y` reach past the frame so
/// a picture can start off screen and move in.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ComposePose {
    #[serde(default)]
    pub secs: f64,
    #[serde(default = "half")]
    pub x: f32,
    #[serde(default = "half")]
    pub y: f32,
    #[serde(default = "one")]
    pub width: f32,
    #[serde(default = "one")]
    pub height: f32,
    #[serde(default)]
    pub rotation_deg: f32,
    #[serde(default = "one")]
    pub opacity: f32,
}

impl Default for ComposePose {
    fn default() -> Self {
        Self { secs: 0.0, x: 0.5, y: 0.5, width: 1.0, height: 1.0, rotation_deg: 0.0, opacity: 1.0 }
    }
}

fn one() -> f32 {
    1.0
}

/// One pose of a pan: at `secs` into the clip, `scale` centred on the
/// frame point `x`,`y` (fractions of the frame).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PanKey {
    pub secs: f64,
    #[serde(default = "default_reframe_scale")]
    pub scale: f32,
    #[serde(default = "half")]
    pub x: f32,
    #[serde(default = "half")]
    pub y: f32,
}

fn half() -> f32 {
    0.5
}

/// The poses a follow makes of the spots: to each spot, a hold, and back
/// out unless the next is near.
pub fn follow_keys(spots: &[(f64, f32, f32)], clip_secs: f64, scale: f32, hold: f64) -> Vec<PanKey> {
    const EASE: f64 = 0.35;
    let rest = |t: f64| PanKey { secs: t, scale: 1.0, x: 0.5, y: 0.5 };
    let mut keys: Vec<PanKey> = Vec::new();
    let mut last_end: Option<f64> = None;
    for (n, (t, x, y)) in spots.iter().enumerate() {
        let start = (t - EASE).max(0.0);
        match last_end {
            // The next spot comes before the last hold is over: the hold
            // ends where the move to the next begins, no pull-out between.
            Some(e) if start <= e + EASE + 0.05 => {
                if let Some(end_key) = keys.pop() {
                    let prev_t = keys.last().map(|k| k.secs).unwrap_or(0.0);
                    let cut = start.max(prev_t);
                    if cut > prev_t + 1e-6 {
                        keys.push(PanKey { secs: cut, ..end_key });
                    }
                }
            }
            Some(e) => {
                keys.push(rest(e + EASE));
                keys.push(rest(start));
            }
            None => {
                if start > 0.0 {
                    keys.push(rest(0.0));
                    keys.push(rest(start));
                }
            }
        }
        let end = (t + hold).min(clip_secs);
        keys.push(PanKey { secs: *t, scale, x: *x, y: *y });
        keys.push(PanKey { secs: end, scale, x: *x, y: *y });
        last_end = Some(end);
        if n + 1 == spots.len() && end + EASE < clip_secs {
            keys.push(rest(end + EASE));
        }
    }
    keys.dedup_by(|a, b| (a.secs - b.secs).abs() < 1e-6);
    keys
}

fn default_reframe_scale() -> f32 {
    1.0
}

fn default_duck_db() -> f32 {
    9.0
}

fn default_duck_fade() -> f64 {
    0.5
}

/// A word of a media file's transcript, in the file's seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct SpokenWord {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

/// Where the resolver gets transcripts from. The document does not know
/// how speech is recognised; whoever asks for a plan does.
pub trait Transcripts {
    /// The words of `item`, in order, or `None` when it has no transcript.
    fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>>;
    /// Where `item`'s sound was measured silent, in the file's seconds.
    /// A cut there is clean whatever the words' times say: those are a
    /// model's estimate, the silence is a measurement.
    fn silences(&self, _item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
        Vec::new()
    }
    /// Where `item`'s picture changes shot, in the file's seconds: two
    /// selects from either side of one are a cut, not a jump cut.
    fn shot_changes(&self, _item: &klipp_media::MediaItem) -> Vec<f64> {
        Vec::new()
    }
    /// How `item`'s screen behaves over time, when it is a screen
    /// recording with a beat sheet; `None` for a camera file or one
    /// not analysed for it.
    fn screen_beats(&self, _item: &klipp_media::MediaItem) -> Option<ScreenBeats> {
        None
    }
    /// What on `item`'s screen looks like it should not be published,
    /// each with its kind, a masked version, when (file seconds) and
    /// where (left, top, width, height as fractions of the frame).
    fn screen_sensitive(&self, _item: &klipp_media::MediaItem) -> Vec<SensitiveText> {
        Vec::new()
    }
    /// Where the faces are in `item`, per sampled frame: (file seconds,
    /// the head as left, top, width, height in fractions of the frame).
    /// What a title keeps clear of.
    fn faces(&self, _item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
        Vec::new()
    }
}

/// The faces under a stretch of the timeline: every picture clip that
/// overlaps `from..to`, its file's faces inside the stretch it shows.
pub fn faces_under(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts, from: f64, to: f64) -> Vec<[f32; 4]> {
    let mut out = Vec::new();
    for t in seq.tracks.iter().filter(|t| t.kind == TrackKind::Video && t.visible) {
        for c in t.clips.iter().filter(|c| c.enabled && secs(c.timeline_in) < to && secs(c.timeline_out) > from) {
            let Some(item) = c.media_id().and_then(|id| pool.get(&id)) else { continue };
            let (lo, hi) = (from.max(secs(c.timeline_in)), to.min(secs(c.timeline_out)));
            let file_at = |x: f64| secs(c.source_in) + (x - secs(c.timeline_in)) * c.speed.max(0.001);
            let (f_lo, f_hi) = (file_at(lo) - 0.75, file_at(hi) + 0.75);
            out.extend(transcripts.faces(item).into_iter().filter(|(s, _)| *s >= f_lo && *s <= f_hi).map(|(_, b)| b));
        }
    }
    out
}

/// Whether two boxes (left, top, width, height) overlap, with `margin`
/// round the first.
pub fn boxes_overlap(a: [f32; 4], b: [f32; 4], margin: f32) -> bool {
    a[0] - margin < b[0] + b[2] && a[0] + a[2] + margin > b[0] && a[1] - margin < b[1] + b[3] && a[1] + a[3] + margin > b[1]
}

/// The box a title covers, as fractions of the frame: its text boxes
/// sized from their type and words, its shapes and images from their
/// transforms. `None` for a title with nothing visible.
pub fn title_box(td: &klipp_timeline::TitleData) -> Option<[f32; 4]> {
    use klipp_timeline::TitleObjectContent;
    let mut acc: Option<[f32; 4]> = None;
    for o in td.objects.iter().filter(|o| o.visible) {
        let (cx, cy) = (o.transform.center.x, o.transform.center.y);
        let (w, h) = match &o.content {
            TitleObjectContent::TextBox(tb) => text_extent(&tb.text, tb.font_size),
            _ => (o.transform.size.x, o.transform.size.y),
        };
        let b = [cx - w / 2.0, cy - h / 2.0, w, h];
        acc = Some(match acc {
            None => b,
            Some(a) => {
                let (x0, y0) = (a[0].min(b[0]), a[1].min(b[1]));
                let (x1, y1) = ((a[0] + a[2]).max(b[0] + b[2]), (a[1] + a[3]).max(b[1] + b[3]));
                [x0, y0, x1 - x0, y1 - y0]
            }
        });
    }
    acc
}

/// About how much of the frame text of `size` (px of a 1080-line frame)
/// covers: the longest line at a bit over half an em a character, the
/// lines at 1.3 em.
pub fn text_extent(text: &str, size: f32) -> (f32, f32) {
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0).max(1) as f32;
    let lines = text.lines().count().max(1) as f32;
    ((longest * size * 0.55 / 1920.0).min(0.94), (lines * size * 1.3 / 1080.0).min(0.9))
}

/// The nine zones of the frame a title can sit in, by the centre they
/// give a box of `size`.
pub const ZONES: [(&str, f32, f32); 9] = [
    ("top left", 0.22, 0.12), ("top centre", 0.5, 0.12), ("top right", 0.78, 0.12),
    ("middle left", 0.22, 0.5), ("centre", 0.5, 0.5), ("middle right", 0.78, 0.5),
    ("lower left", 0.22, 0.85), ("lower centre", 0.5, 0.85), ("lower right", 0.78, 0.85),
];

/// Where a box of `size` centred at `center` should go so it covers no
/// keep-out: where it is, when it is clear; else the nearest of the
/// nine zones that is, with its name. `None` when no zone is clear.
pub fn clear_zone(center: (f32, f32), size: (f32, f32), keep_out: &[[f32; 4]]) -> Option<((f32, f32), Option<&'static str>)> {
    const MARGIN: f32 = 0.04;
    let boxed = |c: (f32, f32)| [(c.0 - size.0 / 2.0).clamp(0.02, 0.98 - size.0), (c.1 - size.1 / 2.0).clamp(0.02, 0.98 - size.1), size.0, size.1];
    let clear = |c: (f32, f32)| !keep_out.iter().any(|k| boxes_overlap(boxed(c), *k, MARGIN));
    if keep_out.is_empty() || clear(center) {
        return Some((center, None));
    }
    let mut zones: Vec<&(&str, f32, f32)> = ZONES.iter().collect();
    zones.sort_by(|a, b| ((a.1 - center.0).powi(2) + (a.2 - center.1).powi(2)).total_cmp(&((b.1 - center.0).powi(2) + (b.2 - center.1).powi(2))));
    zones.into_iter().find(|z| clear((z.1, z.2))).map(|z| ((z.1, z.2), Some(z.0)))
}


/// A secret left on screen, as the check reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct SensitiveText {
    pub kind: String,
    pub masked: String,
    pub start_secs: f64,
    pub end_secs: f64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A screen recording's beat sheet as the check reads it, all in the
/// file's seconds: where the screen was changing, where a change
/// settled (the places to cut to), and where it held still long enough
/// to carry a title.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScreenBeats {
    pub action: Vec<(f64, f64)>,
    pub settles: Vec<f64>,
    pub holds: Vec<(f64, f64)>,
    /// Where things happened: (seconds, x, y) as fractions of the frame.
    pub spots: Vec<(f64, f32, f32)>,
}

/// No transcripts at all: `captions` is refused.
pub struct NoTranscripts;

impl Transcripts for NoTranscripts {
    fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
        None
    }
}

fn default_true() -> bool {
    true
}

/// A clip as a plan reports it, after a step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipAfter {
    pub clip_id: ClipId,
    pub track: usize,
    pub timeline_in_secs: f64,
    pub timeline_out_secs: f64,
    pub source_in_secs: f64,
    pub source_out_secs: f64,
}

/// One op resolved: what it does, in a sentence and as the command
/// that does it.
#[derive(Debug, Clone)]
pub struct Step {
    pub op: Op,
    pub what: String,
    pub edit: EditCommand,
    /// The clips the step touched or made, as they are after it.
    pub after: Vec<ClipAfter>,
}

/// A plan as a client reads it: the steps without their commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    pub op: Op,
    pub what: String,
    pub after: Vec<ClipAfter>,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub steps: Vec<Step>,
    /// The timeline's length once the plan has run, seconds.
    pub duration_secs: f64,
    /// Where the last picture ends; the timeline is as long as its
    /// longest track, which may be sound or captions.
    pub picture_end_secs: f64,
}

impl Plan {
    pub fn report(&self) -> Vec<StepReport> {
        self.steps.iter().map(|s| StepReport { op: s.op.clone(), what: s.what.clone(), after: s.after.clone() }).collect()
    }

    /// [`Self::text`] with long runs of one kind of step folded: the
    /// first two and the last of a run of five or more, and how many
    /// were left out. A sixty-step assembly said every step twice, in
    /// text and as JSON, and cost its caller fifteen thousand tokens.
    pub fn text_folded(&self) -> String {
        let kind = |s: &Step| std::mem::discriminant(&s.op);
        let mut out = String::new();
        let mut i = 0;
        while i < self.steps.len() {
            let mut j = i;
            while j + 1 < self.steps.len() && kind(&self.steps[j + 1]) == kind(&self.steps[i]) {
                j += 1;
            }
            let run = j - i + 1;
            for k in i..=j {
                if run >= 5 && k > i + 1 && k < j {
                    if k == i + 2 {
                        out.push_str(&format!("   … {} more like these …\n", run - 3));
                    }
                    continue;
                }
                out.push_str(&format!("{}. {}\n", k + 1, self.steps[k].what));
            }
            i = j + 1;
        }
        out.push_str(&self.ending());
        out
    }

    /// How long the timeline is afterwards — and where the picture ends,
    /// when that is somewhere else: a trim that left the sound behind
    /// read as "90.10 s" with the picture over at 89.3.
    fn ending(&self) -> String {
        if self.picture_end_secs > 0.0 && (self.duration_secs - self.picture_end_secs).abs() > 0.05 {
            format!("Timeline afterwards: {:.2} s; the picture ends at {:.2} s\n", self.duration_secs, self.picture_end_secs)
        } else {
            format!("Timeline afterwards: {:.2} s\n", self.duration_secs)
        }
    }

    /// The plan in a few lines, for a person or a model to read.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (i, s) in self.steps.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, s.what));
        }
        out.push_str(&self.ending());
        out
    }
}

/// Why a plan could not be made: the step (0-based), the reason, and
/// what would make it possible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refusal {
    pub step: usize,
    pub why: String,
    pub hint: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "step {}: {}. {}", self.step + 1, self.why, self.hint)
    }
}

fn refuse(step: usize, why: impl Into<String>, hint: impl Into<String>) -> Refusal {
    Refusal { step, why: why.into(), hint: hint.into() }
}

/// `secs` on the nearest frame at `fps`, as ticks at the sequence's rate.
fn snap(secs: f64, fps: Fps) -> Timecode {
    let frames = (secs * fps.as_f64()).round() as i64;
    frame_tc(frames, fps)
}

fn frame_tc(frames: i64, fps: Fps) -> Timecode {
    // `rate` is the fps numerator, so a frame is `den` ticks: one for
    // whole rates, 1001 for the NTSC ones.
    Timecode { value: frames * fps.den.max(1) as i64, rate: fps.rate() }
}

fn one_frame(fps: Fps) -> Timecode {
    frame_tc(1, fps)
}

fn secs(t: Timecode) -> f64 {
    t.to_secs_f64()
}

fn find_clip(seq: &Sequence, id: ClipId) -> Option<(usize, usize, &Clip)> {
    seq.tracks.iter().enumerate().find_map(|(ti, t)| t.clips.iter().position(|c| c.id == id).map(|ci| (ti, ci, &t.clips[ci])))
}

/// The clip a step names, by id or by place, or a refusal saying which.
fn resolve_clip<'a>(seq: &'a Sequence, fps: Fps, r: &ClipRef, step: usize) -> Result<(usize, usize, &'a Clip), Refusal> {
    match r {
        ClipRef::Id(id) => find_clip(seq, *id).ok_or_else(|| refuse(step, format!("no clip {}", id.0), "clip ids are in the timeline view; a clip made by an earlier step can be named by {track, secs}")),
        ClipRef::At { track, secs: at } => {
            let t = seq.tracks.get(*track).ok_or_else(|| refuse(step, format!("no track {track}"), format!("the sequence has tracks 0 to {}", seq.tracks.len().saturating_sub(1))))?;
            let tc = snap(*at, fps);
            let ci = t
                .clips
                .iter()
                .position(|c| c.timeline_in <= tc && tc < c.timeline_out)
                .ok_or_else(|| refuse(step, format!("nothing on track {track} at {at:.2} s"), "give a time inside a clip on that track"))?;
            Ok((*track, ci, &t.clips[ci]))
        }
    }
}

fn clip_ref_id(seq: &Sequence, fps: Fps, r: &ClipRef, step: usize) -> Result<ClipId, Refusal> {
    resolve_clip(seq, fps, r, step).map(|(_, _, c)| c.id)
}

/// A file's name short enough to say forty times in a plan: a long one
/// keeps its start and its extension. The ids say which file it is.
fn short_name(filename: &str) -> String {
    const KEEP: usize = 22;
    let n = filename.chars().count();
    if n <= KEEP + 8 {
        return filename.to_string();
    }
    let ext = filename.rsplit_once('.').map(|(_, e)| e).filter(|e| e.len() <= 5).unwrap_or("");
    let head: String = filename.chars().take(KEEP).collect();
    format!("{}…{}{ext}", head.trim_end(), if ext.is_empty() { "" } else { "." })
}

fn clip_name(seq: &Sequence, pool: &MediaPool, clip: &Clip) -> String {
    match &clip.kind {
        ClipKind::Media { media_id } => pool.get(media_id).map(|m| short_name(&m.filename)).unwrap_or_else(|| "media".into()),
        ClipKind::Title(td) => format!("title \"{}\"", td.text().lines().next().unwrap_or("").chars().take(24).collect::<String>()),
        ClipKind::Generator(_) => "generator".into(),
        ClipKind::AdjustmentLayer => "adjustment layer".into(),
        ClipKind::Gap => "gap".into(),
        ClipKind::Sequence { .. } => "nested sequence".into(),
    }
    .to_string()
        + &seq.tracks.iter().enumerate().find(|(_, t)| t.clips.iter().any(|c| c.id == clip.id)).map(|(i, _)| format!(" on track {i}")).unwrap_or_default()
}

/// A track kind by the name the ops use for it.
fn track_kind(name: &str) -> Option<TrackKind> {
    match name.to_lowercase().as_str() {
        "video" => Some(TrackKind::Video),
        "audio" => Some(TrackKind::Audio),
        "text" | "title" => Some(TrackKind::Text),
        "subtitle" | "subtitles" | "captions" => Some(TrackKind::Subtitle),
        _ => None,
    }
}

/// A track's index: the one given, or the first of a kind.
fn resolve_track(seq: &Sequence, t: &TrackRef, step: usize) -> Result<usize, Refusal> {
    match t {
        TrackRef::Index(n) if *n < seq.tracks.len() => Ok(*n),
        TrackRef::Index(n) => Err(refuse(step, format!("no track {n}"), format!("the sequence has tracks 0 to {}", seq.tracks.len().saturating_sub(1)))),
        TrackRef::Kind(kind) => {
            let want = track_kind(kind).ok_or_else(|| refuse(step, format!("\"{kind}\" is not a track kind"), "use video, audio, text or subtitle, or a track index"))?;
            seq.tracks.iter().position(|t| t.kind == want).ok_or_else(|| refuse(step, format!("no {} track", kind.to_lowercase()), "add one with add_track, or give a track index"))
        }
    }
}

fn resolve_anchor(seq: &Sequence, fps: Fps, a: &Anchor, step: usize) -> Result<Timecode, Refusal> {
    Ok(match a {
        Anchor::Secs(s) => {
            if !s.is_finite() || *s < 0.0 {
                return Err(refuse(step, format!("{s} s is not a time on the timeline"), "give seconds from the start, zero or more"));
            }
            snap(*s, fps)
        }
        Anchor::Frame(f) => {
            if *f < 0 {
                return Err(refuse(step, format!("frame {f} is before the start"), "give a frame number, zero or more"));
            }
            frame_tc(*f, fps)
        }
        Anchor::ClipStart(r) => resolve_clip(seq, fps, r, step)?.2.timeline_in,
        Anchor::ClipEnd(r) => resolve_clip(seq, fps, r, step)?.2.timeline_out,
        Anchor::TrackEnd(t) => {
            let ti = resolve_track(seq, t, step)?;
            seq.tracks[ti].clips.iter().map(|c| c.timeline_out).max().unwrap_or_else(|| frame_tc(0, fps))
        }
        Anchor::Marker(label) => {
            let want = label.to_lowercase();
            seq.markers
                .iter()
                .find(|m| m.label.to_lowercase() == want)
                .map(|m| m.position)
                .ok_or_else(|| refuse(step, format!("no marker \"{label}\""), "markers are listed at the end of the timeline view"))?
        }
        Anchor::Offset { from, secs: off } => {
            let base = resolve_anchor(seq, fps, from, step)?;
            let t = base.to_secs_f64() + off;
            if t < 0.0 {
                return Err(refuse(step, "the offset lands before the start", "use a smaller offset"));
            }
            snap(t, fps)
        }
    })
}

/// A clip on `track` overlapping `[a, b)`, other than `except`.
fn overlapping(seq: &Sequence, track: usize, a: Timecode, b: Timecode, except: Option<ClipId>) -> Option<&Clip> {
    seq.tracks.get(track)?.clips.iter().find(|c| Some(c.id) != except && c.timeline_out > a && c.timeline_in < b)
}

fn after_of(seq: &Sequence, ids: &[ClipId]) -> Vec<ClipAfter> {
    ids.iter()
        .filter_map(|id| find_clip(seq, *id))
        .map(|(ti, _, c)| ClipAfter {
            clip_id: c.id,
            track: ti,
            timeline_in_secs: secs(c.timeline_in),
            timeline_out_secs: secs(c.timeline_out),
            source_in_secs: secs(c.source_in),
            source_out_secs: secs(c.source_out),
        })
        .collect()
}

/// Resolve `ops` against `seq` and `pool`, in order, each seeing the
/// ones before it. Nothing given is changed.
pub fn resolve(seq: &Sequence, pool: &MediaPool, ops: &[Op]) -> Result<Plan, Refusal> {
    resolve_with(seq, pool, ops, &NoTranscripts)
}

/// [`resolve`], with the transcripts `captions` reads.
pub fn resolve_with(seq: &Sequence, pool: &MediaPool, ops: &[Op], transcripts: &dyn Transcripts) -> Result<Plan, Refusal> {
    let mut sims = vec![seq.clone()];
    let mut sim_pool = pool.clone();
    let mut sim_fps = seq.fps;
    let mut steps = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        let sim = &sims[0];
        // The rate as the plan stands at this step: a place that set an
        // empty sequence's format changed it for every step after.
        let (what, edit, touched) = resolve_one(sim, &sim_pool, sim.fps, op, i, transcripts)?;
        let mut view = crate::apply::DocumentView { sequences: &mut sims, active: 0, media_pool: &mut sim_pool, fps: &mut sim_fps };
        view.apply_forward(&edit);
        let after = after_of(&sims[0], &touched);
        steps.push(Step { op: op.clone(), what, edit, after });
    }
    let picture_end_secs = sims[0].tracks.iter().filter(|t| t.kind == TrackKind::Video).flat_map(|t| t.clips.iter()).map(|c| secs(c.timeline_out)).fold(0.0, f64::max);
    Ok(Plan { steps, duration_secs: secs(sims[0].total_duration()), picture_end_secs })
}

fn resolve_one(seq: &Sequence, pool: &MediaPool, fps: Fps, op: &Op, i: usize, transcripts: &dyn Transcripts) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    match op {
        Op::Captions { track, from, to, max_chars, max_lines, replace } => {
            let from = from.as_ref().map(|a| resolve_anchor(seq, fps, a, i)).transpose()?;
            let to = to.as_ref().map(|a| resolve_anchor(seq, fps, a, i)).transpose()?;
            captions(seq, pool, fps, i, transcripts, track.as_ref(), from, to, (*max_chars).max(8), (*max_lines).max(1), *replace)
        }
        Op::Audio { clip_id, volume_db, gain_db, pan, fade_in, fade_out } => {
            let (_, _, given) = resolve_clip(seq, fps, clip_id, i)?;
            audio_settings(seq, pool, i, given, *volume_db, *gain_db, *pan, *fade_in, *fade_out)
        }
        Op::Duck { clip_id, by_db, fade_secs } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            duck(seq, pool, i, ti, clip, *by_db, *fade_secs)
        }
        Op::Transition { clip_id, kind, secs } => {
            let (ti, ci, clip) = resolve_clip(seq, fps, clip_id, i)?;
            transition(seq, pool, fps, i, ti, ci, clip, kind, *secs)
        }
        Op::Effect { clip_id, kind, amount } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            picture_effect(seq, pool, i, ti, clip, kind, *amount)
        }
        Op::Stabilize { clip_id, smoothing } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            stabilize(seq, pool, i, ti, clip, *smoothing)
        }
        Op::Redact { clip_id, from, to, x, y, w, h, kind, amount } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            let from = match from { Some(a) => Some(resolve_anchor(seq, fps, a, i)?), None => None };
            let to = match to { Some(a) => Some(resolve_anchor(seq, fps, a, i)?), None => None };
            redact(seq, pool, fps, i, ti, clip, from, to, [*x, *y, *w, *h], kind, *amount)
        }
        Op::Slip { clip_id, secs: by } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            slip(seq, pool, fps, i, ti, clip, *by)
        }
        Op::Role { clip_id, role } => {
            let (_, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            let new = match role.trim().to_lowercase().as_str() {
                "dialogue" | "speech" => klipp_core::ClipRole::Dialogue,
                "music" => klipp_core::ClipRole::Music,
                "sfx" | "effects" | "effect" => klipp_core::ClipRole::SFX,
                "ambience" | "atmos" => klipp_core::ClipRole::Ambience,
                "foley" => klipp_core::ClipRole::Foley,
                "voiceover" | "vo" => klipp_core::ClipRole::Voiceover,
                "broll" | "b-roll" => klipp_core::ClipRole::BRoll,
                "graphics" | "title" => klipp_core::ClipRole::Graphics,
                other => return Err(refuse(i, format!("\"{other}\" is not a role"), "use dialogue, music, sfx, ambience, foley, voiceover, broll or graphics")),
            };
            let what = format!("{} is {}", clip_name(seq, pool, clip), role.trim().to_lowercase());
            Ok((what, EditCommand::SetClipRole { clip_id: clip.id, old: clip.role, new }, vec![clip.id]))
        }
        Op::Format { width, height, fps: rate } => {
            if *width < 16 || *height < 16 || *width > 16384 || *height > 16384 {
                return Err(refuse(i, format!("{width}x{height} is not a picture size"), "give the delivery's width and height in pixels"));
            }
            let new_fps = match rate {
                Some(r) if *r > 0.0 && r.is_finite() => fps_from_f64(*r),
                Some(r) => return Err(refuse(i, format!("{r} is not a frame rate"), "give the rate in frames a second, or leave it out")),
                None => seq.fps,
            };
            let new = (klipp_core::Resolution { width: *width, height: *height }, new_fps);
            let what = format!("the sequence becomes {width}x{height} at {:.3} fps (was {}x{} at {:.3})", new_fps.as_f64(), seq.resolution.width, seq.resolution.height, seq.fps.as_f64());
            Ok((what, EditCommand::SetSequenceFormat { old: (seq.resolution, seq.fps), new }, Vec::new()))
        }
        Op::Reframe { clip_id, scale, x, y } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            reframe(seq, pool, i, ti, clip, *scale, *x, *y)
        }
        Op::Zoom { clip_id, from, to, x, y } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            zoom(seq, pool, i, ti, clip, *from, *to, *x, *y)
        }
        Op::Pan { clip_id, keys } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            pan(seq, pool, i, ti, clip, keys, "pan")
        }
        Op::Follow { clip_id, scale, hold_secs } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            let item = clip.media_id().and_then(|id| pool.get(&id)).ok_or_else(|| refuse(i, format!("{} has no media", clip_name(seq, pool, clip)), "follow a media clip"))?;
            let (s_in, s_out) = (secs(clip.source_in), secs(clip.source_out));
            let spots: Vec<(f64, f32, f32)> = transcripts.screen_beats(item).map(|b| b.spots).unwrap_or_default().into_iter().filter(|(t, _, _)| *t >= s_in && *t < s_out).map(|(t, x, y)| (t - s_in, x, y)).collect();
            if spots.is_empty() {
                return Err(refuse(i, format!("nothing happened on screen in {}", clip_name(seq, pool, clip)), "the beat sheet has no spot in this stretch (analyze first, or the screen held still); use pan with keys of your own"));
            }
            let keys = follow_keys(&spots, secs(clip.timeline_out) - secs(clip.timeline_in), *scale, *hold_secs);
            pan(seq, pool, i, ti, clip, &keys, "follow")
        }
        Op::Highlight { at, secs: hold, x, y, w, h, kind, color, stroke, track } => {
            let at = resolve_anchor(seq, fps, at, i)?;
            highlight(seq, fps, i, at, *hold, [*x, *y, *w, *h], kind, color.as_deref(), *stroke, track.as_ref())
        }
        Op::Ramp { clip_id, from, to, secs: over } => {
            let (ti, ci, clip) = resolve_clip(seq, fps, clip_id, i)?;
            ramp(seq, pool, fps, i, ti, ci, clip, *from, *to, *over)
        }
        Op::Look { clip_id, lut_path, name, opacity, remove } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            apply_look(seq, pool, i, ti, clip, lut_path, name.as_deref(), *opacity, *remove)
        }
        Op::Qualify { clip_id, target, hue, hue_width, sat_low, sat_high, lum_low, lum_high, invert, exposure, saturation, hue_shift, temperature, remove, window, window_feather } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            let q = QualifyArgs { target: target.as_deref(), hue: *hue, hue_width: *hue_width, sat_low: *sat_low, sat_high: *sat_high, lum_low: *lum_low, lum_high: *lum_high, invert: *invert, exposure: *exposure, saturation: *saturation, hue_shift: *hue_shift, temperature: *temperature, remove: *remove, window: window.as_deref(), window_feather: *window_feather };
            qualify(seq, pool, i, ti, clip, q)
        }
        Op::Retime { clip_id, speed } => {
            let (ti, ci, clip) = resolve_clip(seq, fps, clip_id, i)?;
            retime(seq, pool, fps, i, ti, ci, clip, *speed)
        }
        Op::CleanAudio { clip_id, wind, hiss, hum, deess } => {
            let (_, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            clean_audio(seq, pool, i, clip, *wind, *hiss, *hum, *deess)
        }
        Op::Grade { clip_id, exposure, contrast, saturation, temperature, tint, lift, gamma, gain, highlights, shadows, vibrance, reset } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            let knobs: [(&str, &str, Option<f32>, f32, f32); 11] = [
                ("exposure", "exposure", *exposure, -6.0, 6.0),
                // The grade's contrast is 0 at neutral (+1 doubles the distance
                // from mid grey); the op speaks in the multiplier the UI shows,
                // 1 as shot, like saturation.
                ("contrast", "contrast", contrast.map(|c| c - 1.0), -1.0, 3.0),
                ("saturation", "saturation", *saturation, 0.0, 4.0),
                // Mireds of assumed light, as the node and the app's own
                // slider use them: about 5 Lab b* at 25, 10 at 50. The
                // old -1..1 was a fiftieth of a visible step — a white
                // balance that did nothing at all.
                ("temperature", "temperature", *temperature, -50.0, 50.0),
                ("tint", "tint", *tint, -50.0, 50.0),
                ("lift", "lift_master", *lift, -1.0, 1.0),
                ("gamma", "gamma_master", *gamma, -1.0, 1.0),
                ("gain", "gain_master", *gain, -1.0, 1.0),
                ("highlights", "highlights", *highlights, -1.0, 1.0),
                ("shadows", "shadows", *shadows, -1.0, 1.0),
                ("vibrance", "vibrance", *vibrance, -1.0, 1.0),
            ];
            grade(seq, pool, i, ti, clip, &knobs, *reset)
        }
        Op::Style { clip_id, track, from, to, size, color, font, bold, align, x, y, backing, outline, full_frame } => {
            let look = Look {
                full_frame: full_frame.as_deref().map(|c| parse_color(c).ok_or_else(|| refuse(i, format!("\"{c}\" is not a colour"), "write the full-frame colour as #rrggbb"))).transpose()?,
                size: *size,
                color: color.as_deref().map(|c| parse_color(c).ok_or_else(|| refuse(i, format!("\"{c}\" is not a colour"), "write it as #rrggbb or #rrggbbaa"))).transpose()?,
                font: font.clone(),
                bold: *bold,
                align: align
                    .as_deref()
                    .map(|a| match a.to_lowercase().as_str() {
                        "left" => Ok(klipp_timeline::TextAlignment::Left),
                        "center" | "centre" => Ok(klipp_timeline::TextAlignment::Center),
                        "right" => Ok(klipp_timeline::TextAlignment::Right),
                        other => Err(refuse(i, format!("\"{other}\" is not an alignment"), "use left, center or right")),
                    })
                    .transpose()?,
                x: *x,
                y: *y,
                backing: *backing,
                outline: *outline,
            };
            let from = from.as_ref().map(|a| resolve_anchor(seq, fps, a, i)).transpose()?;
            let to = to.as_ref().map(|a| resolve_anchor(seq, fps, a, i)).transpose()?;
            let one = clip_id.as_ref().map(|r| resolve_clip(seq, fps, r, i)).transpose()?;
            let ti = track.as_ref().map(|t| resolve_track(seq, t, i)).transpose()?;
            style(seq, pool, i, one.map(|(ti, _, c)| (ti, c)), ti, from, to, &look)
        }
        Op::Title { text, at, style, secs, track, x, y, size, color, font, bold, align, backing, backing_color, full_frame, outline, outline_color, glow, glow_color, gradient, animate, animate_secs, stagger, loop_effect, glitch, chromatic } => {
            let at = resolve_anchor(seq, fps, at, i)?;
            // The look the style names fills what the op leaves out.
            let look = match style.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                Some(name) => Some(crate::title_style::look(seq.brief.as_ref(), name).ok_or_else(|| {
                    let names = crate::title_style::names(seq.brief.as_ref());
                    refuse(i, format!("\"{name}\" is not a title style"), format!("the styles are {}; set_brief with type_styles adds one", names.join(", ")))
                })?),
                None => None,
            };
            let look = look.unwrap_or_default();
            let full_frame_color = full_frame.clone().or_else(|| look.full_frame.filter(|f| *f).map(|_| look.fill.clone().unwrap_or_else(|| "#101418".into())));
            let backing_color = backing_color.clone().or_else(|| if look.full_frame.unwrap_or(false) { None } else { look.fill.clone() });
            let keep_out = faces_under(seq, pool, transcripts, crate::ops::secs(at), crate::ops::secs(at) + *secs);
            let args = TitleArgs {
                keep_out,
                text, secs: *secs, track: track.as_ref(), x: x.or(look.x), y: y.or(look.y), size: size.or(look.size),
                color: color.as_deref().or(look.color.as_deref()), font: font.clone().or(look.font.clone()), bold: bold.or(look.bold), align: align.as_deref().or(look.align.as_deref()),
                backing: backing.or(if backing_color.is_some() { None } else { look.backing }), backing_color: backing_color.as_deref(), full_frame: full_frame_color.as_deref(),
                outline: outline.or(look.outline), outline_color: outline_color.as_deref().or(look.outline_color.as_deref()), glow: glow.or(look.glow), glow_color: glow_color.as_deref().or(look.glow_color.as_deref()),
                gradient: gradient.as_ref().or(look.gradient.as_ref()), animate: animate.as_deref().or(look.animate.as_deref()), animate_secs: animate_secs.or(look.animate_secs), stagger: stagger.as_deref().or(look.stagger.as_deref()),
                loop_effect: loop_effect.as_deref().or(look.loop_effect.as_deref()), glitch: *glitch, chromatic: *chromatic,
            };
            title_op(seq, fps, i, at, args)
        }
        Op::LowerThird { name, role, at, secs: hold, track, side, x, y } => {
            let at = resolve_anchor(seq, fps, at, i)?;
            let place = lower_third_place(i, side.as_deref(), *x, *y, role.as_deref().is_some_and(|r| !r.trim().is_empty()))?;
            lower_third(seq, fps, i, name, role.as_deref(), at, *hold, track.as_ref(), place)
        }
        Op::SetText { clip_id, text } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            set_text(seq, pool, i, ti, clip, text)
        }
        Op::AddTrack { kind, name } => {
            let want = track_kind(kind).ok_or_else(|| refuse(i, format!("\"{kind}\" is not a track kind"), "use video, audio, text or subtitle"))?;
            // Pictures and titles go on top; sound and subtitles at the bottom.
            let at_top = matches!(want, TrackKind::Video | TrackKind::Text);
            let count = seq.tracks.iter().filter(|t| t.kind == want).count();
            let name = name.clone().unwrap_or_else(|| {
                let label = match want {
                    TrackKind::Video => "Video",
                    TrackKind::Audio => "Audio",
                    TrackKind::Text => "Text",
                    TrackKind::Subtitle => "Subtitle",
                };
                format!("{label} {}", count + 1)
            });
            let track = klipp_timeline::Track::new(&name, want);
            let index = if at_top { 0 } else { seq.tracks.len() };
            let what = format!("add {} track \"{name}\" as track {index}{}", kind.to_lowercase(), if at_top && !seq.tracks.is_empty() { "; every other track's index moves down by one" } else { "" });
            Ok((what, EditCommand::AddTrack { index, track }, Vec::new()))
        }

        Op::Place { media_id, track, at, source_in, source_out, insert, with_audio } => {
            let item = pool.get(media_id).ok_or_else(|| refuse(i, format!("no media item {}", media_id.0), "media ids are in list_media"))?;
            if item.offline {
                return Err(refuse(i, format!("{} is offline", item.filename), "relink the media first"));
            }
            let ti = resolve_track(seq, track, i)?;
            let t = &seq.tracks[ti];
            if t.locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it, or use another track"));
            }
            let media_dur = secs(item.duration);
            let s_in = source_in.unwrap_or(0.0);
            let s_out = source_out.unwrap_or(media_dur);
            if !s_in.is_finite() || !s_out.is_finite() || s_in < 0.0 || s_out <= s_in
                || (!item.is_still && (s_in >= media_dur || s_out > media_dur + 1e-6)) {
                return Err(refuse(i, format!("source {s_in:.2}-{s_out:.2} s is not inside {} ({media_dur:.2} s)", item.filename), "give source_in and source_out within the file, in order"));
            }
            // An empty sequence takes the first picture's size and rate,
            // as an append in the app does; that place is already snapped
            // at the rate the sequence is about to have.
            let no_video_yet = !seq.tracks.iter().any(|t| t.kind == TrackKind::Video && !t.clips.is_empty());
            let autoset = if no_video_yet && item.resolution.width > 0 && item.fps.as_f64() > 0.0 && (seq.resolution != item.resolution || seq.fps != item.fps) {
                Some(((seq.resolution, seq.fps), (item.resolution, item.fps)))
            } else {
                None
            };
            let fps = autoset.map(|(_, (_, f))| f).unwrap_or(fps);
            // Stills have no file end, but their authored time must
            // still fit the integer frame clock (including NTSC ticks).
            let max_frames = i64::MAX / i64::from(fps.den.max(1));
            if s_out * fps.as_f64() >= max_frames as f64 {
                return Err(refuse(i, "the source range exceeds the frame clock", "use a representable source range"));
            }
            let src_in = snap(s_in, fps);
            let src_out = snap(s_out, fps);
            if src_out <= src_in {
                return Err(refuse(i, "the source range is shorter than a frame", "make it at least one frame"));
            }
            let tl_in = resolve_anchor(seq, fps, at, i)?;
            let duration = (src_out - src_in).rescale_to(tl_in.rate);
            let value = tl_in.value.checked_add(duration.value).ok_or_else(|| refuse(i, "the placement exceeds the frame clock", "use an earlier start or shorter duration"))?;
            let tl_out = Timecode { value, rate: tl_in.rate };
            let mut clip = Clip::media(*media_id, tl_in, tl_out, src_in, src_out);
            // The sound with the picture, linked both ways, on the first
            // audio track — made at the bottom when there is none.
            let bring_audio = *with_audio && item.has_audio && t.kind != TrackKind::Audio;
            // A video clip with no linked audio clip plays the sound in
            // its file; left out means silent, so the clip is muted.
            let muted = !*with_audio && item.has_audio && t.kind != TrackKind::Audio;
            if muted {
                clip.volume = 0.0;
            }
            let mut audio: Option<(Clip, usize)> = None;
            let mut created_audio_track_at = None;
            if bring_audio {
                let ati = match seq.tracks.iter().position(|t| t.kind == TrackKind::Audio) {
                    Some(ati) => ati,
                    None => {
                        created_audio_track_at = Some(seq.tracks.len());
                        seq.tracks.len()
                    }
                };
                if created_audio_track_at.is_none() {
                    if seq.tracks[ati].locked {
                        return Err(refuse(i, format!("audio track {ati} is locked"), "unlock it, or place with with_audio false"));
                    }
                    if !*insert {
                        if let Some(c) = overlapping(seq, ati, tl_in, tl_out, None) {
                            return Err(refuse(i, format!("its sound would overlap {} at {:.2}-{:.2} s on track {ati}", clip_name(seq, pool, c), secs(c.timeline_in), secs(c.timeline_out)), "place it later, or with with_audio false"));
                        }
                    }
                }
                let mut a = Clip::media(*media_id, tl_in, tl_out, src_in, src_out);
                a.linked_clip_id = Some(clip.id);
                clip.linked_clip_id = Some(a.id);
                audio = Some((a, ati));
            }
            let sound_note = match (&audio, created_audio_track_at) {
                (Some(_), Some(ati)) => format!(", its sound linked on a new audio track {ati}"),
                (Some((_, ati)), None) => format!(", its sound linked on track {ati}"),
                _ if muted => ", its sound muted".to_string(),
                _ => String::new(),
            };
            let format_note = autoset.map(|(_, (r, f))| format!("; the sequence becomes {}x{} at {:.3} fps", r.width, r.height, f.as_f64())).unwrap_or_default();
            let ids: Vec<ClipId> = std::iter::once(clip.id).chain(audio.iter().map(|(a, _)| a.id)).collect();
            if *insert {
                let mut scratch = seq.clone();
                let edit = scratch
                    .insert_edit(ti, clip, tl_in)
                    .ok_or_else(|| refuse(i, format!("track {ti} would not take an insert"), "unlock the track, or place without insert"))?;
                let what = format!(
                    "insert {} on track {ti} at {:.2} s ({:.2}-{:.2} s of the file), ending at {:.2} s; {} clip(s) split, {} moved {:.2} s later{sound_note}",
                    short_name(&item.filename),
                    secs(tl_in),
                    secs(src_in),
                    secs(src_out),
                    secs(tl_out),
                    edit.splits.len(),
                    edit.shifted.len(),
                    secs(tl_out - tl_in)
                );
                return Ok((what, EditCommand::LegacyInsertEdit { edit, audio, created_audio_track_at }, ids));
            }
            if let Some(c) = overlapping(seq, ti, tl_in, tl_out, None) {
                return Err(refuse(
                    i,
                    format!("it would overlap {} at {:.2}-{:.2} s", clip_name(seq, pool, c), secs(c.timeline_in), secs(c.timeline_out)),
                    format!("place it at clip_end of {}, remove that clip first, or place with insert", c.id.0),
                ));
            }
            let what = format!(
                "place {} on track {ti} at {:.2} s ({:.2}-{:.2} s of the file), ending at {:.2} s{sound_note}{format_note}",
                short_name(&item.filename),
                secs(tl_in),
                secs(src_in),
                secs(src_out),
                secs(tl_out)
            );
            let edit = if audio.is_some() || autoset.is_some() {
                EditCommand::LinkedImport { video: Some((clip, ti)), audio, created_video_track_at: None, created_audio_track_at, autoset }
            } else {
                EditCommand::LegacyAppendClip { clip, track_index: ti }
            };
            Ok((what, edit, ids))
        }

        Op::Trim { clip_id: clip_ref, edge, to, ripple, linked } => {
            let (ti, ci, clip) = resolve_clip(seq, fps, clip_ref, i)?;
            let target = resolve_anchor(seq, fps, to, i)?;
            let (what, edit, mut ids) = trim_clip(seq, pool, fps, i, ti, ci, clip, *edge, target, *ripple)?;
            // The clip it is linked to goes with it, by the same amount —
            // from wherever its own edge is, so a J-cut keeps its lead.
            // A trim that moved the picture alone left the two out of
            // sync, and said nothing.
            let partner = clip.linked_clip_id.filter(|_| *linked).and_then(|lid| find_clip(seq, lid));
            let Some((pti, pci, p)) = partner else { return Ok((what, edit, ids)) };
            let delta = target - match edge { Edge::In => clip.timeline_in, Edge::Out => clip.timeline_out };
            let p_target = match edge { Edge::In => p.timeline_in, Edge::Out => p.timeline_out } + delta;
            let (p_what, p_edit, p_ids) = trim_clip(seq, pool, fps, i, pti, pci, p, *edge, p_target, *ripple).map_err(|mut r| {
                r.why = format!("its linked {} cannot follow: {}", if seq.tracks[pti].kind == TrackKind::Audio { "sound" } else { "picture" }, r.why);
                r.hint = format!("{}; or trim with linked false to move this clip alone", r.hint);
                r
            })?;
            ids.extend(p_ids);
            Ok((format!("{what}; with it, {p_what}"), EditCommand::Batch(vec![edit, p_edit]), ids))
        }

        Op::JCut { incoming_clip, frames } => split_edit(seq, pool, fps, i, clip_ref_id(seq, fps, incoming_clip, i)?, *frames, true),
        Op::LCut { outgoing_clip, frames } => split_edit(seq, pool, fps, i, clip_ref_id(seq, fps, outgoing_clip, i)?, *frames, false),

        Op::Split { clip_id: clip_ref, at } => {
            let (ti, ci, clip) = resolve_clip(seq, fps, clip_ref, i)?;
            let clip_id = &clip.id;
            if seq.tracks[ti].locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
            }
            let cut = resolve_anchor(seq, fps, at, i)?;
            let mut scratch = seq.tracks[ti].clone();
            let Some((mut a, mut b)) = scratch.blade_at(*clip_id, cut) else {
                return Err(refuse(
                    i,
                    format!("{:.2} s is not inside {} ({:.2}-{:.2} s) by at least a frame", secs(cut), clip_name(seq, pool, clip), secs(clip.timeline_in), secs(clip.timeline_out)),
                    "split at a time strictly inside the clip",
                ));
            };
            // The linked sound (or picture) is cut at the same time, and
            // the halves are linked crosswise, as one split in the app is.
            let mut partner_edit = None;
            let mut ids = vec![a.id, b.id];
            if let Some((pti, pci, partner)) = clip.linked_clip_id.and_then(|pid| find_clip(seq, pid)) {
                if !seq.tracks[pti].locked {
                    let mut pscratch = seq.tracks[pti].clone();
                    if let Some((mut pa, mut pb)) = pscratch.blade_at(partner.id, cut) {
                        a.linked_clip_id = Some(pa.id);
                        pa.linked_clip_id = Some(a.id);
                        b.linked_clip_id = Some(pb.id);
                        pb.linked_clip_id = Some(b.id);
                        ids.push(pa.id);
                        ids.push(pb.id);
                        partner_edit = Some(EditCommand::LegacyBladeClip { track_index: pti, original_clip: partner.clone(), clip_a: pa, clip_b: pb, position: pci });
                    }
                }
            }
            let what = format!(
                "split {} at {:.2} s into {:.2}-{:.2} s and {:.2}-{:.2} s{}",
                clip_name(seq, pool, clip),
                secs(cut),
                secs(a.timeline_in),
                secs(a.timeline_out),
                secs(b.timeline_in),
                secs(b.timeline_out),
                if partner_edit.is_some() { ", its linked sound with it" } else { "" }
            );
            let main = EditCommand::LegacyBladeClip { track_index: ti, original_clip: clip.clone(), clip_a: a, clip_b: b, position: ci };
            let edit = match partner_edit {
                Some(p) => EditCommand::Batch(vec![main, p]),
                None => main,
            };
            Ok((what, edit, ids))
        }

        Op::Remove { clip_id: clip_ref, ripple } => {
            let (ti, ci, clip) = resolve_clip(seq, fps, clip_ref, i)?;
            let _ = clip_ref;
            if seq.tracks[ti].locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
            }
            let name = clip_name(seq, pool, clip);
            if *ripple {
                let gap = clip.timeline_out - clip.timeline_in;
                let shifted: Vec<(ClipId, Timecode, Timecode)> =
                    seq.tracks[ti].clips[ci + 1..].iter().map(|c| (c.id, c.timeline_in, c.timeline_out)).collect();
                let what = format!("remove {} ({:.2}-{:.2} s) and close the gap: {} clips move {:.2} s earlier", name, secs(clip.timeline_in), secs(clip.timeline_out), shifted.len(), secs(gap));
                let ids: Vec<ClipId> = shifted.iter().map(|s| s.0).collect();
                Ok((what, EditCommand::LegacyRippleRemove { removed_clip: clip.clone(), track_index: ti, position: ci, shifted_clips: shifted }, ids))
            } else {
                let what = format!("remove {} ({:.2}-{:.2} s), leaving the gap", name, secs(clip.timeline_in), secs(clip.timeline_out));
                Ok((what, EditCommand::LegacyRemoveClip { clip: clip.clone(), track_index: ti, position: ci }, Vec::new()))
            }
        }

        Op::Move { clip_id: clip_ref, to } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_ref, i)?;
            let clip_id = &clip.id;
            if seq.tracks[ti].locked {
                return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
            }
            let new_in = resolve_anchor(seq, fps, to, i)?;
            let len = clip.timeline_out - clip.timeline_in;
            let new_out = new_in + len;
            if let Some(c) = overlapping(seq, ti, new_in, new_out, Some(*clip_id)) {
                return Err(refuse(i, format!("it would overlap {} at {:.2}-{:.2} s", clip_name(seq, pool, c), secs(c.timeline_in), secs(c.timeline_out)), format!("move it to clip_end of {}, or remove that clip first", c.id.0)));
            }
            let delta = new_in - clip.timeline_in;
            let mut touched = vec![*clip_id];
            let linked_partner = clip.linked_clip_id.and_then(|pid| {
                let (pti, _, p) = find_clip(seq, pid)?;
                let p_in = p.timeline_in + delta;
                let p_out = p.timeline_out + delta;
                if seq.tracks[pti].locked || overlapping(seq, pti, p_in, p_out, Some(pid)).is_some() {
                    return None;
                }
                touched.push(pid);
                Some(LinkedMovePartner { partner_id: pid, old_timeline_in: p.timeline_in, old_timeline_out: p.timeline_out, new_timeline_in: p_in, new_timeline_out: p_out })
            });
            let what = format!(
                "move {} from {:.2} s to {:.2} s (now {:.2}-{:.2} s){}",
                clip_name(seq, pool, clip),
                secs(clip.timeline_in),
                secs(new_in),
                secs(new_in),
                secs(new_out),
                if linked_partner.is_some() { ", its linked partner with it" } else { "" }
            );
            let edit = EditCommand::LegacyMoveClip {
                clip_id: *clip_id,
                old_timeline_in: clip.timeline_in,
                old_timeline_out: clip.timeline_out,
                new_timeline_in: new_in,
                new_timeline_out: new_out,
                linked_partner,
            };
            Ok((what, edit, touched))
        }
        Op::Compose { clip_id, poses } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            compose(seq, pool, i, ti, clip, poses)
        }
        Op::Camera { keys } => camera(seq, i, keys),
        Op::Crop { clip_id, left, right, top, bottom } => {
            let (ti, _, clip) = resolve_clip(seq, fps, clip_id, i)?;
            crop(seq, pool, i, ti, clip, [*left, *right, *top, *bottom])
        }
        Op::Generator { data, at, secs: hold, end, track } => {
            crate::generators::validate(data).map_err(|why| refuse(i, why, "use valid generator parameters"))?;
            let start = resolve_anchor(seq, fps, at, i)?;
            let finish = match (hold, end) {
                (Some(d), None) if d.is_finite() && *d > 0.0 && *d <= 86_400.0 => {
                    let duration = snap(*d, fps).rescale_to(start.rate);
                    let value = start.value.checked_add(duration.value).ok_or_else(|| refuse(i, "generator time overflows", "use a shorter range"))?;
                    Timecode { value, rate: start.rate }
                }
                (None, Some(anchor)) => resolve_anchor(seq, fps, anchor, i)?,
                _ => return Err(refuse(i, "a generator needs exactly one of secs or end", "give secs above zero and at most one day, or an end anchor")),
            };
            if finish <= start {
                return Err(refuse(i, "a generator must last at least one frame", "put its end after its start"));
            }
            let ti = match track {
                Some(t) => resolve_track(seq, t, i)?,
                None => seq.tracks.last().filter(|t| t.kind == TrackKind::Video && t.visible && !t.locked && t.name == "Background").map(|_| seq.tracks.len() - 1).unwrap_or(seq.tracks.len()),
            };
            let created = ti == seq.tracks.len();
            if !created {
                if seq.tracks[ti].kind != TrackKind::Video || seq.tracks[ti].locked {
                    return Err(refuse(i, "a generator needs an unlocked video track", "choose a video track or omit track for a background track"));
                }
                if overlapping(seq, ti, start, finish, None).is_some() {
                    return Err(refuse(i, "the generator would overlap a clip", "use an empty range or a different video track"));
                }
            }
            let mut clip = Clip::gap(start, finish);
            clip.kind = ClipKind::Generator(data.clone());
            let id = clip.id;
            let append = EditCommand::LegacyAppendClip { clip, track_index: ti };
            let edit = if created {
                EditCommand::Batch(vec![EditCommand::AddTrack { index: ti, track: klipp_timeline::Track::new("Background", TrackKind::Video) }, append])
            } else { append };
            Ok((format!("place generator on video track {ti} at {:.2}-{:.2} s{}", secs(start), secs(finish), if created { "; create Background below the existing tracks" } else { "" }), edit, vec![id]))
        }

    }
}

fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub(crate) fn linear_to_db(v: f32) -> f32 {
    if v <= 0.0 { f32::NEG_INFINITY } else { 20.0 * v.log10() }
}

/// The clip whose sound settings `clip` plays with: the video clip it
/// is linked to when it has one, else itself. Playback and the render
/// read them there.
pub(crate) fn sound_settings_clip<'a>(seq: &'a Sequence, clip: &'a Clip) -> &'a Clip {
    clip.linked_clip_id
        .and_then(|lid| seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).flat_map(|t| t.clips.iter()).find(|c| c.id == lid))
        .unwrap_or(clip)
}

#[allow(clippy::too_many_arguments)]
fn audio_settings(
    seq: &Sequence,
    pool: &MediaPool,
    i: usize,
    given: &Clip,
    volume_db: Option<f32>,
    gain_db: Option<f32>,
    pan: Option<f32>,
    fade_in: Option<f32>,
    fade_out: Option<f32>,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let clip = sound_settings_clip(seq, given);
    if !clip.media_id().and_then(|id| pool.get(&id)).is_some_and(|m| m.has_audio) {
        return Err(refuse(i, format!("{} has no sound", clip_name(seq, pool, clip)), "name a clip whose media has audio"));
    }
    if volume_db.is_none() && gain_db.is_none() && pan.is_none() && fade_in.is_none() && fade_out.is_none() {
        return Err(refuse(i, "nothing to change", "give volume_db, gain_db, pan, fade_in or fade_out"));
    }
    for (name, v, lo, hi) in [("volume_db", volume_db, -96.0, 12.0), ("gain_db", gain_db, -96.0, 24.0), ("pan", pan, -1.0, 1.0)] {
        if let Some(v) = v {
            if !v.is_finite() || v < lo || v > hi {
                return Err(refuse(i, format!("{name} {v} is out of range"), format!("give {name} between {lo} and {hi}")));
            }
        }
    }
    let length = secs(clip.timeline_out - clip.timeline_in);
    let (fi, fo) = (fade_in.unwrap_or(clip.fade_in_secs), fade_out.unwrap_or(clip.fade_out_secs));
    if fi < 0.0 || fo < 0.0 || !fi.is_finite() || !fo.is_finite() {
        return Err(refuse(i, "a fade cannot be negative", "give fades in seconds, zero or more"));
    }
    if (fi + fo) as f64 > length + 1e-6 {
        return Err(refuse(i, format!("fades of {fi:.2} and {fo:.2} s do not fit a clip of {length:.2} s"), "shorten the fades"));
    }
    let new_volume = volume_db.map(db_to_linear).unwrap_or(clip.volume);
    let new_gain = gain_db.map(db_to_linear).unwrap_or(clip.gain);
    let new_pan = pan.unwrap_or(clip.pan);
    let mut parts = Vec::new();
    if volume_db.is_some() {
        parts.push(format!("volume {:.1} dB (was {:.1})", linear_to_db(new_volume), linear_to_db(clip.volume)));
    }
    if gain_db.is_some() {
        parts.push(format!("gain {:.1} dB (was {:.1})", linear_to_db(new_gain), linear_to_db(clip.gain)));
    }
    if pan.is_some() {
        parts.push(format!("pan {new_pan:.2}"));
    }
    if fade_in.is_some() {
        parts.push(format!("fade in {fi:.2} s"));
    }
    if fade_out.is_some() {
        parts.push(format!("fade out {fo:.2} s"));
    }
    let on = if clip.id == given.id { String::new() } else { " (set on the video clip it is linked to)".to_string() };
    let what = format!("sound of {}: {}{on}", clip_name(seq, pool, given), parts.join(", "));
    let edit = EditCommand::EditClipAudio {
        clip_id: clip.id,
        old_gain: clip.gain,
        old_volume: clip.volume,
        old_pan: clip.pan,
        old_fade_in: clip.fade_in_secs,
        old_fade_out: clip.fade_out_secs,
        old_channel_config: clip.channel_config,
        new_gain,
        new_volume,
        new_pan,
        new_fade_in: fi,
        new_fade_out: fo,
        new_channel_config: clip.channel_config,
    };
    Ok((what, edit, vec![given.id]))
}

/// Whether `clip` on `track` is heard: enabled, with sound, not silent,
/// and — on a video track — not standing in for a linked audio clip.
pub(crate) fn is_heard(seq: &Sequence, pool: &MediaPool, track: &klipp_timeline::Track, clip: &Clip) -> bool {
    if track.muted || !clip.enabled || !matches!(track.kind, TrackKind::Audio | TrackKind::Video) {
        return false;
    }
    if !clip.media_id().and_then(|id| pool.get(&id)).is_some_and(|m| m.has_audio) {
        return false;
    }
    if track.kind == TrackKind::Video {
        let partner_on_audio = clip.linked_clip_id.is_some_and(|lid| seq.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|c| c.id == lid)));
        if partner_on_audio {
            return false;
        }
    }
    sound_settings_clip(seq, clip).volume > 0.0
}

fn duck(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, music: &Clip, by_db: f32, fade_secs: f64) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    if !(1.0..=40.0).contains(&by_db) {
        return Err(refuse(i, format!("{by_db} dB is not a duck"), "music under speech comes down six to twelve decibels"));
    }
    if !(0.05..=5.0).contains(&fade_secs) {
        return Err(refuse(i, format!("a move of {fade_secs} s"), "give fade_secs between 0.05 and 5; a duck is a fade, never a step"));
    }
    if !is_heard(seq, pool, &seq.tracks[ti], music) {
        return Err(refuse(i, format!("{} is not heard", clip_name(seq, pool, music)), "name the music clip: one with sound, not muted"));
    }
    let settings = sound_settings_clip(seq, music);
    let (m_in, m_out) = (secs(music.timeline_in), secs(music.timeline_out));
    // Where anything else is heard over the music, merged where the
    // gaps are too short to come up and go down again.
    let mut over: Vec<(f64, f64)> = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
        .filter(|(t, c)| c.id != music.id && Some(c.id) != music.linked_clip_id && is_heard(seq, pool, t, c))
        .map(|(_, c)| (secs(c.timeline_in).max(m_in), secs(c.timeline_out).min(m_out)))
        .filter(|(a, b)| b > a)
        .collect();
    if over.is_empty() {
        return Err(refuse(i, "nothing is heard over it", "place the speech first; a duck follows what is on the timeline"));
    }
    over.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for (a, b) in over {
        match spans.last_mut() {
            Some(last) if a - last.1 < fade_secs * 2.5 => last.1 = last.1.max(b),
            _ => spans.push((a, b)),
        }
    }
    let base = settings.volume;
    let low = base * db_to_linear(-by_db);
    // Keyframes in the clip's own time: down before the speech starts,
    // up after it ends.
    let mut kfs: Vec<klipp_core::Keyframe> = Vec::new();
    let mut put = |t: f64, v: f32| klipp_core::keyframe::upsert_keyframe(&mut kfs, (t - m_in).clamp(0.0, m_out - m_in), v);
    for (a, b) in &spans {
        if *a - fade_secs > m_in {
            put(a - fade_secs, base);
        }
        put(*a, low);
        put(*b, low);
        if *b + fade_secs < m_out {
            put(b + fade_secs, base);
        }
    }
    kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    let old_state = crate::history::KeyframeSnapshot::from_clip(settings);
    let mut new_state = old_state.clone();
    new_state.volume_keyframes = kfs;
    let on = if settings.id == music.id { String::new() } else { " (on the video clip it is linked to)".to_string() };
    let what = format!(
        "duck {} by {by_db:.0} dB under {} stretch{} of other sound ({}), moving over {fade_secs:.2} s{on}",
        clip_name(seq, pool, music),
        spans.len(),
        if spans.len() == 1 { "" } else { "es" },
        spans.iter().map(|(a, b)| format!("{a:.2}-{b:.2} s")).collect::<Vec<_>>().join(", "),
    );
    let edit = EditCommand::EditKeyframes {
        clip_id: settings.id,
        linked_clip_id: None,
        description: "Duck",
        old_state,
        new_state,
        old_linked_state: None,
        new_linked_state: None,
    };
    Ok((what, edit, vec![music.id]))
}

/// One clip's edge to `target`: the geometry, the refusals and the
/// recorded edit. [`Op::Trim`] is this for the clip named and, by the
/// same amount, for the clip it is linked to.
#[allow(clippy::too_many_arguments)]
fn trim_clip(seq: &Sequence, pool: &MediaPool, fps: Fps, i: usize, ti: usize, ci: usize, clip: &Clip, edge: Edge, target: Timecode, ripple: bool) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let clip_id = &clip.id;
    if seq.tracks[ti].locked {
        return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
    }
    let media_dur = clip.media_id().and_then(|m| pool.get(&m)).filter(|m| !m.is_still).map(|m| m.duration);
    let (new_tl_in, new_tl_out, new_src_in, new_src_out) = match edge {
        Edge::In => {
            if target >= clip.timeline_out - one_frame(fps) {
                return Err(refuse(i, "the in point would pass the out point", "trim to a time before the clip's end"));
            }
            let delta = target - clip.timeline_in;
            let src_in = clip.source_in + delta;
            if media_dur.is_none() {
                // A title, generator or still has no source head
                // to run out of: it starts when it is
                // told to and is as long as its place on the track.
                let len = clip.timeline_out - target;
                let tl_in = if ripple { clip.timeline_in } else { target };
                (tl_in, tl_in + len, clip.source_in, clip.source_in + len)
            } else if src_in.value < 0 {
                return Err(refuse(i, format!("only {:.2} s of the file lies before this clip's in point", secs(clip.source_in)), "trim less, or move the clip instead"));
            } else if ripple {
                // The clip stays where it starts and is shorter from
                // the front: the later clips close up by the change.
                (clip.timeline_in, clip.timeline_out - delta, src_in, clip.source_out)
            } else {
                (target, clip.timeline_out, src_in, clip.source_out)
            }
        }
        Edge::Out => {
            if target <= clip.timeline_in + one_frame(fps) {
                return Err(refuse(i, "the out point would pass the in point", "trim to a time after the clip's start"));
            }
            let delta = target - clip.timeline_out;
            let src_out = clip.source_out + delta;
            if let Some(d) = media_dur {
                if src_out > d {
                    return Err(refuse(i, format!("only {:.2} s of the file lies after this clip's out point", secs(d - clip.source_out)), "trim less, or place more of the file"));
                }
            }
            (clip.timeline_in, target, clip.source_in, src_out)
        }
    };
    if ripple {
        let shifted: Vec<(ClipId, Timecode, Timecode)> =
            seq.tracks[ti].clips[ci + 1..].iter().map(|c| (c.id, c.timeline_in, c.timeline_out)).collect();
        let delta = new_tl_out - clip.timeline_out;
        // The earlier neighbour must not be run into by a head trim
        // that makes the clip longer (it cannot: the start stays).
        let what = format!(
            "ripple trim the {} of {} to {:.2} s (now {:.2}-{:.2} s); {} later clip(s) move {:.2} s",
            match edge { Edge::In => "in", Edge::Out => "out" },
            clip_name(seq, pool, clip),
            secs(target),
            secs(new_tl_in),
            secs(new_tl_out),
            shifted.len(),
            secs(delta)
        );
        let ids: Vec<ClipId> = std::iter::once(*clip_id).chain(shifted.iter().map(|s| s.0)).collect();
        let edit = EditCommand::LegacyRippleTrimClip {
            clip_id: *clip_id,
            old_source_in: clip.source_in,
            old_source_out: clip.source_out,
            old_timeline_in: clip.timeline_in,
            old_timeline_out: clip.timeline_out,
            new_source_in: new_src_in,
            new_source_out: new_src_out,
            new_timeline_in: new_tl_in,
            new_timeline_out: new_tl_out,
            shifted_clips: shifted,
        };
        return Ok((what, edit, ids));
    }
    if let Some(c) = overlapping(seq, ti, new_tl_in, new_tl_out, Some(*clip_id)) {
        return Err(refuse(i, format!("it would run into {} at {:.2} s", clip_name(seq, pool, c), secs(c.timeline_in)), "trim less, move that clip, or trim with ripple"));
    }
    let what = format!(
        "trim the {} of {} to {:.2} s (now {:.2}-{:.2} s)",
        match edge { Edge::In => "in", Edge::Out => "out" },
        clip_name(seq, pool, clip),
        secs(target),
        secs(new_tl_in),
        secs(new_tl_out)
    );
    let edit = EditCommand::LegacyTrimClip {
        clip_id: *clip_id,
        old_source_in: clip.source_in,
        old_source_out: clip.source_out,
        old_timeline_in: clip.timeline_in,
        old_timeline_out: clip.timeline_out,
        new_source_in: new_src_in,
        new_source_out: new_src_out,
        new_timeline_in: new_tl_in,
        new_timeline_out: new_tl_out,
    };
    Ok((what, edit, vec![*clip_id]))
}

/// A word placed on the timeline, and whether a cut in the sound comes
/// before it.
struct HeardWord {
    start: f64,
    end: f64,
    text: String,
    after_cut: bool,
}

/// The words heard on the timeline, in order: every enabled clip whose
/// sound plays — audio-track clips, and video clips with no audio clip
/// linked to them — that is not muted, its transcript's words moved from
/// the file's time to where the clip puts them.
fn heard_words(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts) -> Vec<HeardWord> {
    let mut cache: std::collections::HashMap<MediaId, Option<Vec<SpokenWord>>> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for track in &seq.tracks {
        if track.muted || !matches!(track.kind, TrackKind::Audio | TrackKind::Video) {
            continue;
        }
        for clip in &track.clips {
            let ClipKind::Media { media_id } = &clip.kind else { continue };
            if !clip.enabled || clip.volume == 0.0 {
                continue;
            }
            if track.kind == TrackKind::Video {
                let partner_on_audio = clip.linked_clip_id.is_some_and(|lid| {
                    seq.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|c| c.id == lid))
                });
                if partner_on_audio {
                    continue;
                }
            }
            let Some(item) = pool.get(media_id) else { continue };
            let Some(words) = cache.entry(*media_id).or_insert_with(|| transcripts.words(item)).as_ref() else { continue };
            let (s_in, s_out) = (secs(clip.source_in), secs(clip.source_out));
            let (t_in, t_out) = (secs(clip.timeline_in), secs(clip.timeline_out));
            let speed = if clip.speed > 0.0 { clip.speed } else { 1.0 };
            let mut first = true;
            for w in words.iter().filter(|w| w.start_secs >= s_in - 0.05 && w.start_secs < s_out) {
                let text = w.text.trim();
                if text.is_empty() {
                    continue;
                }
                let start = (t_in + (w.start_secs - s_in) / speed).max(t_in);
                let end = (t_in + (w.end_secs - s_in) / speed).clamp(start, t_out);
                out.push(HeardWord { start, end, text: text.to_string(), after_cut: first });
                first = false;
            }
        }
    }
    out.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// A caption: its time on the timeline and its lines.
#[derive(Debug, Clone, PartialEq)]
struct Cue {
    start: f64,
    end: f64,
    text: String,
}

/// Gather words into cues: a cue closes at a cut in the sound, a pause,
/// the end of a sentence once it has some length, when it is full, or
/// when it has been up long enough.
fn cues_from_words(words: &[HeardWord], max_chars: usize, max_lines: usize, sound_ends: f64) -> Vec<Cue> {
    const PAUSE_SECS: f64 = 0.7;
    const LONGEST_SECS: f64 = 6.0;
    const SENTENCE_MIN_CHARS: usize = 18;
    let room = max_chars * max_lines;
    let mut cues: Vec<Cue> = Vec::new();
    let mut cur: Vec<&HeardWord> = Vec::new();
    let close = |cur: &mut Vec<&HeardWord>, cues: &mut Vec<Cue>| {
        if let (Some(a), Some(b)) = (cur.first(), cur.last()) {
            let text = cur.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
            cues.push(Cue { start: a.start, end: b.end, text });
        }
        cur.clear();
    };
    for w in words {
        if let Some(last) = cur.last() {
            let len: usize = cur.iter().map(|x| x.text.chars().count() + 1).sum();
            let sentence_done = last.text.ends_with(['.', '?', '!']) && len >= SENTENCE_MIN_CHARS;
            let full = len + w.text.chars().count() > room;
            let long = w.end - cur[0].start > LONGEST_SECS;
            if w.after_cut || w.start - last.end > PAUSE_SECS || sentence_done || full || long {
                close(&mut cur, &mut cues);
            }
        }
        cur.push(w);
    }
    close(&mut cur, &mut cues);
    // Held a moment past the last word, at least long enough to read,
    // and never into the next cue.
    let n = cues.len();
    for k in 0..n {
        // The last cue ends with the sound: a caption held past it would
        // make the timeline longer than the piece.
        let next = if k + 1 < n { cues[k + 1].start } else { sound_ends };
        let want = (cues[k].end + 0.2).max(cues[k].start + 1.0);
        cues[k].end = want.min(next).max(cues[k].start);
        cues[k].text = wrap_lines(&cues[k].text, max_chars, max_lines);
    }
    cues
}

/// `text` on at most `max_lines` lines of about `max_chars`, the lines
/// as even as the words allow.
fn wrap_lines(text: &str, max_chars: usize, max_lines: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars || max_lines <= 1 {
        return text.to_string();
    }
    let lines_needed = total.div_ceil(max_chars).clamp(2, max_lines);
    let target = total.div_ceil(lines_needed);
    let mut lines: Vec<String> = vec![String::new()];
    for word in text.split_whitespace() {
        let more_lines = lines.len() < lines_needed;
        let cur = lines.last_mut().expect("one line at least");
        let len = cur.chars().count();
        if len > 0 && len + 1 + word.chars().count() > target.max(1) + 3 && more_lines {
            lines.push(word.to_string());
        } else {
            if len > 0 {
                cur.push(' ');
            }
            cur.push_str(word);
        }
    }
    lines.join("\n")
}

/// A caption's look: white on a dark backing, centred low in the
/// frame. The backing is what keeps it readable over a picture with
/// writing of its own down there (a name plate, a ticker); an outline
/// alone is lost in it.
fn caption_title(text: &str) -> klipp_timeline::TitleData {
    let mut tb = klipp_timeline::TextBox {
        text: text.to_string(),
        font_size: 40.0,
        font_name: String::new(),
        alignment: klipp_timeline::TextAlignment::Center,
        ..Default::default()
    };
    tb.outlines.push(klipp_timeline::TextOutline { color: [0, 0, 0, 200], width: 2.0, enabled: true, width_keyframes: Vec::new() });
    let mut td = klipp_timeline::TitleData::single_text(tb);
    // Composited in linear light, where an alpha of 0.72 leaves the
    // picture at half its brightness to the eye; 0.92 is what looks
    // like a three-quarters black.
    td.background = klipp_timeline::TitleBackground::Color { color: [0, 0, 0, 235], padding: 12.0 };
    if let Some(o) = td.objects.first_mut() {
        o.transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.88 };
    }
    td
}

fn title_clip(td: klipp_timeline::TitleData, tl_in: Timecode, tl_out: Timecode, fps: Fps) -> Clip {
    Clip {
        id: ClipId::new(),
        kind: ClipKind::Title(td),
        source_in: frame_tc(0, fps),
        source_out: tl_out - tl_in,
        ..Clip::gap(tl_in, tl_out)
    }
}

#[allow(clippy::too_many_arguments)]
fn captions(
    seq: &Sequence,
    pool: &MediaPool,
    fps: Fps,
    i: usize,
    transcripts: &dyn Transcripts,
    track: Option<&TrackRef>,
    from: Option<Timecode>,
    to: Option<Timecode>,
    max_chars: usize,
    max_lines: usize,
    replace: bool,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let (from_s, to_s) = (from.map(secs).unwrap_or(0.0), to.map(secs).unwrap_or(f64::MAX));
    if to_s <= from_s {
        return Err(refuse(i, "the range ends before it starts", "give from before to"));
    }
    let words: Vec<HeardWord> = heard_words(seq, pool, transcripts).into_iter().filter(|w| w.start >= from_s && w.start < to_s).collect();
    if words.is_empty() {
        return Err(refuse(i, "no transcribed speech is heard on the timeline there", "captions come from the transcripts: run analyze and wait for it, and place the speech first"));
    }
    let sound_ends = secs(seq.total_duration());
    let cues = cues_from_words(&words, max_chars, max_lines, sound_ends);
    // The track: the one named, else the first subtitle track, else a new one at the bottom.
    let existing = match track {
        Some(t) => Some(resolve_track(seq, t, i)?),
        None => seq.tracks.iter().position(|t| t.kind == TrackKind::Subtitle),
    };
    if let Some(ti) = existing {
        let t = &seq.tracks[ti];
        if !matches!(t.kind, TrackKind::Subtitle | TrackKind::Text) {
            return Err(refuse(i, format!("track {ti} is not a subtitle or text track"), "name a subtitle track, or leave the track out"));
        }
        if t.locked {
            return Err(refuse(i, format!("track {ti} is locked"), "unlock it, or use another track"));
        }
    }
    let ti = existing.unwrap_or(seq.tracks.len());
    let mut edits = Vec::with_capacity(cues.len() + 1);
    if existing.is_none() {
        let n = seq.tracks.iter().filter(|t| t.kind == TrackKind::Subtitle).count() + 1;
        edits.push(EditCommand::AddTrack { index: ti, track: klipp_timeline::Track::new(&format!("Subtitle {n}"), TrackKind::Subtitle) });
    }
    // The captions already there go first, when asked: last to first, so
    // each one's place in the track is still its place when it is taken.
    let mut going: Vec<ClipId> = Vec::new();
    if let (true, Some(ti)) = (replace, existing) {
        let there: Vec<(usize, &Clip)> = seq.tracks[ti].clips.iter().enumerate().filter(|(_, c)| c.title_data().is_some() && secs(c.timeline_out) > from_s && secs(c.timeline_in) < to_s).collect();
        for (position, c) in there.into_iter().rev() {
            going.push(c.id);
            edits.push(EditCommand::LegacyRemoveClip { clip: c.clone(), track_index: ti, position });
        }
    }
    let mut ids = Vec::new();
    let mut last_out: Option<Timecode> = None;
    for cue in &cues {
        let mut tl_in = snap(cue.start, fps);
        if let Some(prev) = last_out {
            if tl_in < prev {
                tl_in = prev;
            }
        }
        let tl_out = snap(cue.end, fps).max(tl_in + one_frame(fps));
        if existing.is_some() {
            let in_the_way = seq.tracks[ti].clips.iter().find(|c| !going.contains(&c.id) && c.timeline_in < tl_out && c.timeline_out > tl_in);
            if let Some(c) = in_the_way {
                return Err(refuse(i, format!("a caption at {:.2}-{:.2} s would overlap {} on track {ti}", secs(tl_in), secs(tl_out), clip_name(seq, pool, c)), "make them again with replace true, give from and to around them, or name another track"));
            }
        }
        let clip = title_clip(caption_title(&cue.text), tl_in, tl_out, fps);
        ids.push(clip.id);
        last_out = Some(tl_out);
        edits.push(EditCommand::LegacyAppendClip { clip, track_index: ti });
    }
    // Every cue, to be read: what they say is the point of them, and a
    // caller that cannot see them cannot proofread them.
    let listing: String = cues.iter().map(|c| format!("\n     {:7.2}-{:<7.2} {}", c.start, c.end, c.text.replace('\n', " / "))).collect();
    let what = format!(
        "captions: {} cues from {:.2} to {:.2} s on {}subtitle track {ti}, at most {max_lines} lines of {max_chars}{}; a \" / \" below marks a line break (set_text takes a real newline); correct one with set_text:{listing}",
        cues.len(),
        cues.first().map(|c| c.start).unwrap_or(0.0),
        cues.last().map(|c| c.end).unwrap_or(0.0),
        if existing.is_none() { "a new " } else { "" },
        if going.is_empty() { String::new() } else { format!(", in place of the {} that were there", going.len()) },
    );
    // The plan reports the first and last cue, not hundreds.
    let touched: Vec<ClipId> = ids.first().into_iter().chain(ids.last().filter(|_| ids.len() > 1)).copied().collect();
    Ok((what, EditCommand::Batch(edits), touched))
}

/// New words for a title or a caption: the clip's first text box takes
/// them, and nothing else about it changes.
fn set_text(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, text: &str) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let Some(old) = clip.title_data() else {
        return Err(refuse(i, format!("{} is not a title or a caption", clip_name(seq, pool, clip)), "name a clip on a text or subtitle track"));
    };
    if seq.tracks[ti].locked {
        return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
    }
    let text = text.trim();
    if text.is_empty() {
        return Err(refuse(i, "a title needs words", "to take it away, remove the clip"));
    }
    let mut new = old.clone();
    let Some(tb) = new.objects.iter_mut().find_map(|o| match &mut o.content {
        klipp_timeline::TitleObjectContent::TextBox(tb) => Some(tb),
        _ => None,
    }) else {
        return Err(refuse(i, format!("{} has no text in it", clip_name(seq, pool, clip)), "name a clip with a text box"));
    };
    let was = tb.text.replace('\n', " / ");
    tb.text = text.to_string();
    let what = format!("the text at {:.2}-{:.2} s on track {ti} reads \"{}\" (was \"{was}\")", secs(clip.timeline_in), secs(clip.timeline_out), text.replace('\n', " / "));
    let edit = EditCommand::EditTitle { clip_id: clip.id, description: "Edit Title Text", old_data: old.clone(), new_data: new };
    Ok((what, edit, vec![clip.id]))
}

/// Set a picture clip's builtin effects to `new`, recorded as one
/// undoable edit; the enabled-flags vec is grown to match.
/// A little past the box on every side, so the mosaic's blocks cover
/// the text's edges too.
const REDACT_MARGIN: f32 = 0.006;

#[allow(clippy::too_many_arguments)]
fn redact(seq: &Sequence, pool: &MediaPool, fps: Fps, i: usize, ti: usize, clip: &Clip, from: Option<Timecode>, to: Option<Timecode>, rect: [f32; 4], kind: &str, amount: Option<f32>) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    use klipp_effects::BuiltinEffect as E;
    let ClipKind::Media { media_id } = &clip.kind else {
        return Err(refuse(i, format!("{} is not a media clip", clip_name(seq, pool, clip)), "redact a picture from a file"));
    };
    if (clip.speed - 1.0).abs() > 1e-6 || !clip.speed_keyframes.is_empty() {
        return Err(refuse(i, format!("{} is retimed", clip_name(seq, pool, clip)), "redact it before the retime, or cover the patch with a title"));
    }
    let [x, y, w, h] = rect;
    if !(w > 0.0 && h > 0.0) || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err(refuse(i, "the box has no size or is off the frame", "x, y, w, h are fractions of the frame from the top left; check's sensitive_on_screen finding gives them"));
    }
    let a = from.unwrap_or(clip.timeline_in).max(clip.timeline_in);
    let b = to.unwrap_or(clip.timeline_out).min(clip.timeline_out);
    if b <= a {
        return Err(refuse(i, format!("{:.2}-{:.2} s is not a stretch inside {}", secs(a), secs(b), clip_name(seq, pool, clip)), "give from and to inside the clip, in order"));
    }
    let src_in = clip.source_in + (a - clip.timeline_in);
    let src_out = clip.source_in + (b - clip.timeline_in);
    let strength = amount.unwrap_or(0.5).clamp(0.0, 1.0);
    let hide = match kind.to_lowercase().as_str() {
        "mosaic" | "pixelate" => E::Mosaic { block_size: 12.0 + strength * 48.0 },
        "blur" => E::GaussianBlur { radius: 10.0 + strength * 50.0 },
        other => return Err(refuse(i, format!("\"{other}\" is not a way to redact"), "use mosaic or blur")),
    };
    let left = (x - REDACT_MARGIN).clamp(0.0, 1.0);
    let top = (y - REDACT_MARGIN).clamp(0.0, 1.0);
    let right = (1.0 - (x + w + REDACT_MARGIN)).clamp(0.0, 1.0);
    let bottom = (1.0 - (y + h + REDACT_MARGIN)).clamp(0.0, 1.0);
    // The hidden patch is worked at full strength first, then cut to
    // the box: the crop is applied last so the mosaic's blocks do not
    // bleed the outside in.
    let mut cover = Clip::media(*media_id, a, b, src_in, src_out);
    cover.volume = 0.0;
    cover.builtin_effects = vec![hide, E::Crop { left, right, top, bottom, feather: 0.0 }];
    cover.builtin_effects_enabled = vec![true, true];
    // A video track above with room, else a new one on top.
    let above = (0..ti).rev().find(|k| seq.tracks[*k].kind == TrackKind::Video && !seq.tracks[*k].locked && overlapping(seq, *k, a, b, None).is_none());
    let (video, created) = match above {
        Some(k) => ((cover, k), None),
        None => ((cover, 0), Some(0)),
    };
    let ids = vec![video.0.id];
    let where_ = match created {
        Some(_) => "a new video track on top; every other track's index moves down by one".to_string(),
        None => format!("track {}", video.1),
    };
    let what = format!(
        "redact {} of {} from {:.2} to {:.2} s: {kind} over the box at {:.0}%,{:.0}% ({:.0}% by {:.0}%), a cropped copy on {where_}",
        short_name(&pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_default()), clip_name(seq, pool, clip), secs(a), secs(b), x * 100.0, y * 100.0, w * 100.0, h * 100.0
    );
    let _ = fps;
    Ok((what, EditCommand::LinkedImport { video: Some(video), audio: None, created_video_track_at: created, created_audio_track_at: None, autoset: None }, ids))
}

fn set_builtin_effects(clip: &Clip, new: Vec<klipp_effects::BuiltinEffect>) -> EditCommand {
    let mut new_enabled = clip.builtin_effects_enabled.clone();
    new_enabled.resize(new.len(), true);
    EditCommand::SetBuiltinEffects {
        clip_id: clip.id,
        old_effects: clip.builtin_effects.clone(),
        old_enabled: clip.builtin_effects_enabled.clone(),
        new_effects: new,
        new_enabled,
    }
}

fn require_picture(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, verb: &str) -> Result<(), Refusal> {
    if seq.tracks[ti].locked {
        return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
    }
    if !matches!(clip.kind, ClipKind::Media { .. }) {
        return Err(refuse(i, format!("{} is not a picture clip", clip_name(seq, pool, clip)), format!("{verb} a media clip")));
    }
    Ok(())
}

fn picture_effect(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, kind: &str, amount: Option<f32>) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    require_picture(seq, pool, i, ti, clip, "add the effect to")?;
    let a = amount.unwrap_or(match kind.to_lowercase().as_str() { "sharpen" => 0.4, "glow" | "grain" => 0.4, _ => 0.5 });
    if !a.is_finite() || !(0.0..=1.0).contains(&a) {
        return Err(refuse(i, format!("amount {a} is out of range"), "give amount between 0 and 1"));
    }
    use klipp_effects::BuiltinEffect as E;
    let fx = match kind.to_lowercase().as_str() {
        "vignette" => E::Vignette { amount: -a * 0.8, size: 0.8, softness: 0.5, roundness: 0.7 },
        "grain" | "film_grain" => E::FilmGrain { amount: a * 0.5, size: 1.0, color: 0, shadows: 0.3, seed: 1 },
        "glow" => E::Glow { radius: 30.0, intensity: a },
        // The three that treat a clip as a layer rather than as the
        // frame: they only show once the picture is smaller than the
        // frame, which is what `compose` is for.
        "corners" | "rounded_corners" => E::RoundedCorners { radius: a * 0.25, feather: 0.002 },
        "shadow" | "drop_shadow" => E::DropShadow { offset_x: 0.0, offset_y: a * 0.02, blur: a * 0.04, color: [0, 0, 0], opacity: 0.2 + a * 0.6 },
        // A cool light rather than white: white over a dark ground reads
        // as grey haze, not as glow.
        "outer_glow" => E::OuterGlow { radius: a * 0.06, intensity: a, color: [140, 150, 255] },
        "bloom" => E::Bloom { threshold: 0.7, radius: a * 40.0 + 10.0, intensity: a },
        "diffusion" | "pro_mist" => E::Diffusion { amount: a, radius: 20.0, threshold: 0.6, desaturation: 0.1 },
        "halation" | "film_halation" => E::FilmHalation { threshold: 0.75, radius: a * 30.0 + 10.0, intensity: a, hue: 0.05 },
        "light_rays" | "god_rays" => E::LightRays { center_x: 0.5, center_y: 0.35, threshold: 0.7, samples: 48, decay: 0.95, intensity: a },
        "lens_flare" => E::LensFlare { position_x: 0.7, position_y: 0.3, brightness: a, flare_size: 1.0, ghost_count: 4, ghost_intensity: a * 0.5, chromatic_aberration: 0.3 },
        "anamorphic" | "anamorphic_streak" | "streak" => E::AnamorphicStreak { threshold: 0.75, length: a, intensity: a, tint: 0.6 },
        "light_wrap" => E::LightWrap { width: a * 20.0, intensity: a, softness: 0.5 },
        "blur" => E::GaussianBlur { radius: a * 20.0 },
        "radial_blur" | "zoom_blur" => E::RadialBlur { center_x: 0.5, center_y: 0.5, amount: a * 0.3, samples: 24, mode: 0 },
        "spin_blur" => E::RadialBlur { center_x: 0.5, center_y: 0.5, amount: a * 0.3, samples: 24, mode: 1 },
        "directional_blur" | "motion_blur" => E::DirectionalBlur { angle: 0.0, distance: a * 40.0, samples: 24 },
        "sharpen" => E::Sharpen { amount: a },
        "chromatic_aberration" | "rgb_split" | "fringe" => E::ChromaticAberration { red_shift: a * 8.0, blue_shift: -a * 8.0, center_x: 0.5, center_y: 0.5 },
        "tilt_shift" => E::TiltShift { center: 0.5, width: 0.3, softness: 0.2, radius: a * 20.0 },
        "mosaic" | "pixelate" => E::Mosaic { block_size: a * 40.0 + 2.0 },
        "posterize" => E::Posterize { levels: (32.0 - a * 28.0).max(2.0) },
        "invert" => E::Invert { mix: a },
        "mirror" => E::Mirror { horizontal: 1, vertical: 0 },
        "lens_distortion" | "barrel" => E::LensDistortion { distortion: a * 0.5, scale: 1.0 },
        "emboss" => E::Emboss { angle: 0.0, strength: a, mix: a },
        "edge_detect" | "edges" => E::EdgeDetect { strength: a, mix: a },
        "denoise" => E::Denoise { spatial_sigma: a * 4.0, color_sigma: a * 0.1, iterations: 1 },
        "vhs" => E::Vhs { tracking: a * 0.5, bleed: a, noise: a * 0.5, scanlines: a, seed: 1 },
        "old_film" => E::OldFilm { flicker: a * 0.5, scratches: a, dust: a, weave: a * 0.5, sepia: a * 0.5, seed: 1 },
        other => return Err(refuse(i, format!("\"{other}\" is not an effect"), "use e.g. vignette, grain, glow, bloom, corners, shadow, outer_glow, diffusion, halation, light_rays, lens_flare, anamorphic, blur, radial_blur, directional_blur, sharpen, chromatic_aberration, tilt_shift, mosaic, posterize, invert, mirror, lens_distortion, edge_detect, denoise, vhs or old_film")),
    };
    // The same kind of effect replaces the one that is there.
    let same_kind = |e: &E| std::mem::discriminant(e) == std::mem::discriminant(&fx);
    let mut effects: Vec<E> = clip.builtin_effects.iter().filter(|e| !same_kind(e)).cloned().collect();
    effects.push(fx);
    let what = format!("{} {a:.2} on {}", kind.to_lowercase(), clip_name(seq, pool, clip));
    let edit = set_builtin_effects(clip, effects);
    Ok((what, edit, vec![clip.id]))
}

fn reframe(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, scale: f32, x: f32, y: f32) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    require_picture(seq, pool, i, ti, clip, "reframe")?;
    if !scale.is_finite() || !(1.0..=4.0).contains(&scale) {
        return Err(refuse(i, format!("a scale of {scale}"), "give scale between 1 and 4"));
    }
    for (name, v) in [("x", x), ("y", y)] {
        if !v.is_finite() || !(-1.0..=1.0).contains(&v) {
            return Err(refuse(i, format!("{name} {v} is out of range"), format!("give {name} between -1 and 1")));
        }
    }
    if (scale - 1.0).abs() < 1e-4 && x.abs() < 1e-4 && y.abs() < 1e-4 {
        return Err(refuse(i, "nothing to reframe", "give a scale above 1, or an x or y to pan"));
    }
    if (scale - 1.0).abs() < 1e-4 {
        return Err(refuse(i, "a pan at scale 1 has nowhere to go", "give a scale above 1: the frame pans into what the scale pushes past its edge"));
    }
    use klipp_effects::BuiltinEffect as E;
    // `x` and `y` are how far the frame goes toward that edge, as a
    // share of how far it can: at 1 the picture's edge meets the
    // frame's. Moving the frame right moves the picture left.
    let (w, h) = (seq.resolution.width as f32, seq.resolution.height as f32);
    let fx = E::Transform { position_x: -x * reach(scale, w), position_y: -y * reach(scale, h), scale_x: scale, scale_y: scale, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
    let mut effects: Vec<E> = clip.builtin_effects.iter().filter(|e| !matches!(e, E::Transform { .. })).cloned().collect();
    // Transform goes first, so a crop or effect after it sees the reframed picture.
    effects.insert(0, fx);
    let what = format!("reframe {}: scale {scale:.2}, pan {x:.2}, {y:.2}", clip_name(seq, pool, clip));
    Ok((what, set_builtin_effects(clip, effects), vec![clip.id]))
}

/// An animated crop through `keys`: at each, `scale` centred on the frame
/// point; a scaled picture shifted so far that its edge would show is
/// held at the edge.
/// Cut a picture down to part of itself, or take the crop off.
fn crop(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, sides: [f32; 4]) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    require_picture(seq, pool, i, ti, clip, "crop")?;
    let [left, right, top, bottom] = sides;
    for (name, v) in [("left", left), ("right", right), ("top", top), ("bottom", bottom)] {
        if !v.is_finite() || !(0.0..1.0).contains(&v) {
            return Err(refuse(i, format!("{name} {v} is out of range"), format!("give {name} from 0 up to 1, the share of the picture that comes off that side")));
        }
    }
    if left + right >= 0.99 || top + bottom >= 0.99 {
        return Err(refuse(i, "the crop leaves nothing", "take less off opposite sides than the whole picture"));
    }
    use klipp_effects::BuiltinEffect as E;
    let mut effects: Vec<E> = clip.builtin_effects.iter().filter(|e| !matches!(e, E::Crop { .. })).cloned().collect();
    let off = sides.iter().all(|v| *v <= 0.0);
    let what = if off {
        if effects.len() == clip.builtin_effects.len() {
            return Err(refuse(i, "there is no crop to take off", "give a side to crop"));
        }
        format!("take the crop off {}", clip_name(seq, pool, clip))
    } else {
        // First in the chain, so what follows — corners, a shadow — sees
        // the picture as cropped.
        effects.insert(0, E::Crop { left, right, top, bottom, feather: 0.0 });
        format!("crop {}: {left:.3} left, {right:.3} right, {top:.3} top, {bottom:.3} bottom", clip_name(seq, pool, clip))
    };
    Ok((what, set_builtin_effects(clip, effects), vec![clip.id]))
}

/// Move a camera over the whole sequence, or take it away.
fn camera(seq: &Sequence, i: usize, keys: &[CameraKey]) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let old = seq.camera.clone();
    if keys.is_empty() {
        if old.is_still() {
            return Err(refuse(i, "there is no camera to take away", "give keys to move a camera over the sequence"));
        }
        let what = "the camera is taken off the sequence".to_string();
        return Ok((what, EditCommand::SetSequenceCamera { old: Box::new(old), new: Box::default() }, Vec::new()));
    }
    let end = seq.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| secs(c.timeline_out)).fold(0.0f64, f64::max);
    for k in keys {
        if !k.secs.is_finite() || k.secs < 0.0 || k.secs > end + 1e-6 {
            return Err(refuse(i, format!("a key at {:.2} s is off the timeline", k.secs), format!("keys are seconds on the timeline, 0 to {end:.2}")));
        }
        for (name, v) in [("x", k.x), ("y", k.y)] {
            if !v.is_finite() || !(-1.0..=2.0).contains(&v) {
                return Err(refuse(i, format!("{name} {v} is out of range"), format!("{name} is where the camera looks, a fraction of the frame, -1 to 2")));
            }
        }
        if !k.zoom.is_finite() || !(0.1..=8.0).contains(&k.zoom) {
            return Err(refuse(i, format!("a zoom of {}", k.zoom), "give zoom between 0.1 and 8; 1 is the scene as composed"));
        }
        if !k.roll_deg.is_finite() || !(-360.0..=360.0).contains(&k.roll_deg) {
            return Err(refuse(i, format!("a roll of {} degrees", k.roll_deg), "give roll_deg between -360 and 360"));
        }
    }
    let mut sorted: Vec<CameraKey> = keys.to_vec();
    sorted.sort_by(|a, b| a.secs.total_cmp(&b.secs));
    let mut new = klipp_timeline::Camera::default();
    for k in &sorted {
        klipp_core::keyframe::upsert_keyframe(&mut new.x_keyframes, k.secs, k.x);
        klipp_core::keyframe::upsert_keyframe(&mut new.y_keyframes, k.secs, k.y);
        klipp_core::keyframe::upsert_keyframe(&mut new.zoom_keyframes, k.secs, k.zoom);
        klipp_core::keyframe::upsert_keyframe(&mut new.roll_keyframes, k.secs, k.roll_deg);
    }
    let poses: Vec<String> = sorted.iter().map(|k| format!("{:.2}s {:.2}x at {:.2},{:.2}", k.secs, k.zoom, k.x, k.y)).collect();
    let held = if sorted.len() == 1 { ", held" } else { "" };
    let what = format!("camera over the sequence: {}{held}", poses.join(" \u{2192} "));
    Ok((what, EditCommand::SetSequenceCamera { old: Box::new(old), new: Box::new(new) }, Vec::new()))
}

/// Place a picture in the frame at one pose, or animate it between
/// several. Unlike `reframe`, the picture may be smaller than the frame
/// and may sit anywhere in it, including past its edge; what shows
/// behind is whatever is on a lower track.
fn compose(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, poses: &[ComposePose]) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    require_picture(seq, pool, i, ti, clip, "compose")?;
    if poses.is_empty() {
        return Err(refuse(i, "a compose needs a pose", "give at least one pose: where the picture sits and how much of the frame it covers"));
    }
    let dur = (secs(clip.timeline_out) - secs(clip.timeline_in)).max(0.05);
    for p in poses {
        if !p.secs.is_finite() || p.secs < 0.0 || p.secs > dur + 1e-6 {
            return Err(refuse(i, format!("a pose at {:.2} s is outside the clip", p.secs), format!("poses are seconds into the clip, 0 to {dur:.2}")));
        }
        for (name, v) in [("x", p.x), ("y", p.y)] {
            if !v.is_finite() || !(-1.0..=2.0).contains(&v) {
                return Err(refuse(i, format!("{name} {v} is out of range"), format!("{name} is a fraction of the frame, -1 to 2 so a picture can start off screen")));
            }
        }
        for (name, v) in [("width", p.width), ("height", p.height)] {
            if !v.is_finite() || !(0.01..=4.0).contains(&v) {
                return Err(refuse(i, format!("{name} {v} is out of range"), format!("give {name} between 0.01 and 4, a fraction of the frame")));
            }
        }
        // The engine scales a picture on one axis, the same on x and y,
        // so an uneven rectangle would silently be squared off.
        if (p.width - p.height).abs() > 1e-4 {
            return Err(refuse(i, format!("width {:.2} and height {:.2} differ", p.width, p.height), "give the same width and height: a picture is scaled evenly, never stretched"));
        }
        if !p.rotation_deg.is_finite() || !(-360.0..=360.0).contains(&p.rotation_deg) {
            return Err(refuse(i, format!("a turn of {} degrees", p.rotation_deg), "give rotation_deg between -360 and 360"));
        }
        if !p.opacity.is_finite() || !(0.0..=1.0).contains(&p.opacity) {
            return Err(refuse(i, format!("an opacity of {}", p.opacity), "give opacity between 0 and 1"));
        }
    }
    let mut sorted: Vec<ComposePose> = poses.to_vec();
    sorted.sort_by(|a, b| a.secs.total_cmp(&b.secs));
    let old_state = crate::history::KeyframeSnapshot::from_clip(clip);
    let mut new_state = old_state.clone();
    // Position is pixels from the centre of the frame, which is where
    // the anchor sits; the fractions are turned into them here so a plan
    // written once holds at any delivery size.
    let (fw, fh) = (seq.resolution.width as f32, seq.resolution.height as f32);
    let (mut px, mut py, mut sk, mut rk, mut ok) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for p in &sorted {
        klipp_core::keyframe::upsert_keyframe(&mut px, p.secs, (p.x - 0.5) * fw);
        klipp_core::keyframe::upsert_keyframe(&mut py, p.secs, (p.y - 0.5) * fh);
        klipp_core::keyframe::upsert_keyframe(&mut sk, p.secs, p.width);
        klipp_core::keyframe::upsert_keyframe(&mut rk, p.secs, p.rotation_deg);
        klipp_core::keyframe::upsert_keyframe(&mut ok, p.secs, p.opacity);
    }
    new_state.transform_pos_x_keyframes = px;
    new_state.transform_pos_y_keyframes = py;
    new_state.transform_scale_keyframes = sk;
    new_state.transform_rotation_keyframes = rk;
    new_state.opacity_keyframes = ok;
    let poses_text: Vec<String> = sorted
        .iter()
        .map(|p| {
            let turn = if p.rotation_deg.abs() > 1e-4 { format!(" turned {:.0} degrees", p.rotation_deg) } else { String::new() };
            let fade = if p.opacity < 0.999 { format!(" at {:.0}%", p.opacity * 100.0) } else { String::new() };
            format!("{:.2}s {:.2} of the frame at {:.2},{:.2}{turn}{fade}", p.secs, p.width, p.x, p.y)
        })
        .collect();
    let what = format!("compose {}: {}", clip_name(seq, pool, clip), poses_text.join(" \u{2192} "));
    let edit = EditCommand::EditKeyframes {
        clip_id: clip.id,
        linked_clip_id: None,
        description: "Compose",
        old_state,
        new_state,
        old_linked_state: None,
        new_linked_state: None,
    };
    Ok((what, edit, vec![clip.id]))
}

fn pan(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, keys: &[PanKey], verb: &str) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    require_picture(seq, pool, i, ti, clip, verb)?;
    if keys.len() < 2 {
        return Err(refuse(i, format!("a {verb} needs at least two keys"), "give the pose at two or more times"));
    }
    let dur = (secs(clip.timeline_out) - secs(clip.timeline_in)).max(0.05);
    for k in keys {
        if !k.secs.is_finite() || k.secs < 0.0 || k.secs > dur + 1e-6 {
            return Err(refuse(i, format!("a key at {:.2} s is outside the clip", k.secs), format!("keys are seconds into the clip, 0 to {dur:.2}")));
        }
        if !k.scale.is_finite() || !(1.0..=4.0).contains(&k.scale) {
            return Err(refuse(i, format!("scale {} is out of range", k.scale), "give scale between 1 and 4"));
        }
        if !k.x.is_finite() || !k.y.is_finite() || !(0.0..=1.0).contains(&k.x) || !(0.0..=1.0).contains(&k.y) {
            return Err(refuse(i, format!("point {:.2},{:.2} is outside the frame", k.x, k.y), "x and y are fractions of the frame, 0 to 1"));
        }
    }
    let mut sorted: Vec<PanKey> = keys.to_vec();
    sorted.sort_by(|a, b| a.secs.total_cmp(&b.secs));
    let old_state = crate::history::KeyframeSnapshot::from_clip(clip);
    let mut new_state = old_state.clone();
    let (mut sk, mut px, mut py) = (Vec::new(), Vec::new(), Vec::new());
    for k in &sorted {
        let (x, y) = pan_position(k.scale, k.x, k.y, seq.resolution.width as f32, seq.resolution.height as f32);
        klipp_core::keyframe::upsert_keyframe(&mut sk, k.secs, k.scale);
        klipp_core::keyframe::upsert_keyframe(&mut px, k.secs, x);
        klipp_core::keyframe::upsert_keyframe(&mut py, k.secs, y);
    }
    new_state.transform_scale_keyframes = sk;
    new_state.transform_pos_x_keyframes = px;
    new_state.transform_pos_y_keyframes = py;
    let poses: Vec<String> = sorted.iter().map(|k| format!("{:.2}s {:.2}x@{:.2},{:.2}", k.secs, k.scale, k.x, k.y)).collect();
    let what = format!("{verb} {}: {}", clip_name(seq, pool, clip), poses.join(" \u{2192} "));
    let edit = EditCommand::EditKeyframes {
        clip_id: clip.id,
        linked_clip_id: None,
        description: "Pan",
        old_state,
        new_state,
        old_linked_state: None,
        new_linked_state: None,
    };
    Ok((what, edit, vec![clip.id]))
}

/// The transform position, in pixels of a `w` by `h` frame, that puts
/// the frame point (`x`, `y`) in the middle at `scale` — held back where
/// the picture's edge would show.
///
/// The transform puts a point `d` from the middle at `scale·d + p` on
/// screen, so a subject at `d` is centred by `p = -scale·d`: a subject
/// right of the middle moves the picture left.
pub fn pan_position(scale: f32, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    let (rx, ry) = (reach(scale, w), reach(scale, h));
    ((-(x - 0.5) * scale * w).clamp(-rx, rx), (-(y - 0.5) * scale * h).clamp(-ry, ry))
}

/// How far a picture at `scale` can move from the middle of a frame
/// `dim` pixels across before its edge shows.
fn reach(scale: f32, dim: f32) -> f32 {
    (scale - 1.0).max(0.0) * dim * 0.5
}

/// A speed ramp: speed keyframes from `from` to `to` over `over` seconds
/// of timeline, then `to`; the clip's end on the timeline follows the
/// integral so the whole stretch of the file is still played.
#[allow(clippy::too_many_arguments)]
fn ramp(seq: &Sequence, pool: &MediaPool, fps: Fps, i: usize, ti: usize, ci: usize, clip: &Clip, from: f64, to: f64, over: Option<f64>) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    if seq.tracks[ti].locked {
        return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
    }
    if !matches!(clip.kind, ClipKind::Media { .. }) {
        return Err(refuse(i, format!("{} cannot be ramped", clip_name(seq, pool, clip)), "ramp a media clip"));
    }
    for (name, v) in [("from", from), ("to", to)] {
        if !v.is_finite() || !(0.1..=8.0).contains(&v) {
            return Err(refuse(i, format!("a {name} speed of {v}"), format!("give {name} between 0.1 and 8")));
        }
    }
    if (from - to).abs() < 1e-6 {
        return Err(refuse(i, "from and to are the same speed", "give two speeds, or use retime for one"));
    }
    let source_len = secs(clip.source_out - clip.source_in);
    // The ramp's length on the timeline: as asked, else the whole clip
    // played at the mean of the two speeds.
    let over = over.unwrap_or(source_len / ((from + to) / 2.0));
    if !over.is_finite() || over <= 0.0 {
        return Err(refuse(i, format!("a ramp over {over} s"), "give secs above zero"));
    }
    let mut kfs = Vec::new();
    klipp_core::keyframe::upsert_keyframe(&mut kfs, 0.0, from as f32);
    klipp_core::keyframe::upsert_keyframe(&mut kfs, over, to as f32);
    let new_len = klipp_timeline::effective_timeline_duration_for_keyframes(&kfs, source_len, fps.as_f64(), clip.speed);
    let new_out = clip.timeline_in + snap(new_len, fps);
    if new_out <= clip.timeline_in + one_frame(fps) {
        return Err(refuse(i, "the clip would be shorter than a frame", "use slower speeds"));
    }
    let runs_into = seq.tracks[ti].clips.get(ci + 1).filter(|n| new_out > n.timeline_in).map(|n| clip_name(seq, pool, n));
    let old_state = crate::history::KeyframeSnapshot::from_clip(clip);
    let mut new_state = old_state.clone();
    new_state.speed_keyframes = kfs;
    let what = format!(
        "ramp {} from {from:.2}x to {to:.2}x over {over:.2} s (now {:.2}-{:.2} s){}",
        clip_name(seq, pool, clip),
        secs(clip.timeline_in),
        secs(new_out),
        runs_into.map(|n| format!("; it now overlaps {n} — move or ripple to close it")).unwrap_or_default(),
    );
    let edit = EditCommand::Batch(vec![
        EditCommand::EditKeyframes { clip_id: clip.id, linked_clip_id: None, description: "Speed Ramp", old_state, new_state, old_linked_state: None, new_linked_state: None },
        EditCommand::SetClipSpeed { clip_id: clip.id, old_speed: clip.speed, new_speed: clip.speed, old_timeline_out: clip.timeline_out, new_timeline_out: new_out },
    ]);
    Ok((what, edit, vec![clip.id]))
}

/// A highlight: shapes on a title clip over the region, faded in and out.
#[allow(clippy::too_many_arguments)]
fn highlight(seq: &Sequence, fps: Fps, i: usize, at: Timecode, hold: f64, region: [f32; 4], kind: &str, color: Option<&str>, stroke: Option<f32>, track: Option<&TrackRef>) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    use klipp_timeline::{ObjectTransform, Shape, ShapeKind, ShapeStroke, TitleData, TitleObjectContent, TitlePosition};
    let [x, y, w, h] = region;
    for (name, v) in [("x", x), ("y", y)] {
        if !v.is_finite() || !(0.0..=1.0).contains(&v) {
            return Err(refuse(i, format!("{name} {v} is outside the frame"), "x and y are the region's centre as fractions of the frame"));
        }
    }
    for (name, v) in [("w", w), ("h", h)] {
        if !v.is_finite() || !(0.01..=1.0).contains(&v) {
            return Err(refuse(i, format!("{name} {v} is not a size"), "w and h are fractions of the frame, above 0 to 1"));
        }
    }
    if !hold.is_finite() || hold < 0.2 {
        return Err(refuse(i, format!("{hold} s is too short"), "hold it at least a fraction of a second"));
    }
    let color = color.map(|c| parse_color(c).ok_or_else(|| refuse(i, format!("\"{c}\" is not a colour"), "write it as #rrggbb"))).transpose()?
        .or_else(|| seq.brief.as_ref().and_then(|b| b.primary_color()).and_then(parse_color))
        .unwrap_or([255, 42, 120, 255]);
    let stroke = stroke.unwrap_or(6.0);
    if !stroke.is_finite() || !(1.0..=40.0).contains(&stroke) {
        return Err(refuse(i, format!("stroke {stroke} is out of range"), "give a stroke width 1 to 40"));
    }
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: klipp_timeline::TitleBackground::None };
    let boxed = |cx: f32, cy: f32, bw: f32, bh: f32| ObjectTransform { center: TitlePosition { x: cx, y: cy }, size: TitlePosition { x: bw, y: bh }, ..klipp_timeline::default_text_transform() };
    match kind.to_lowercase().as_str() {
        "box" | "ring" => {
            // A ring is a capsule: corners rounded to half the shorter side.
            let radius = if kind.eq_ignore_ascii_case("ring") { (w.min(h) * 1080.0 * 0.5).max(1.0) } else { 12.0 };
            td.add_object(
                TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: radius }, fill: None, stroke: Some(ShapeStroke { color, width: stroke }) }),
                boxed(x, y, w, h),
            );
        }
        "spot" => {
            // Everything outside the region dimmed: four panels round it.
            let dim = [0, 0, 0, 150];
            let (x0, x1, y0, y1) = ((x - w / 2.0).clamp(0.0, 1.0), (x + w / 2.0).clamp(0.0, 1.0), (y - h / 2.0).clamp(0.0, 1.0), (y + h / 2.0).clamp(0.0, 1.0));
            let panel = |cx: f32, cy: f32, bw: f32, bh: f32, td: &mut TitleData| {
                if bw > 0.001 && bh > 0.001 {
                    td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some(dim), stroke: None }), boxed(cx, cy, bw, bh));
                }
            };
            panel(0.5, y0 / 2.0, 1.0, y0, &mut td);
            panel(0.5, (y1 + 1.0) / 2.0, 1.0, 1.0 - y1, &mut td);
            panel(x0 / 2.0, (y0 + y1) / 2.0, x0, y1 - y0, &mut td);
            panel((x1 + 1.0) / 2.0, (y0 + y1) / 2.0, 1.0 - x1, y1 - y0, &mut td);
        }
        other => return Err(refuse(i, format!("\"{other}\" is not a highlight"), "use box, ring or spot")),
    }
    let tl_in = at;
    let tl_out = tl_in + snap(hold, fps);
    if tl_out > seq.total_duration() {
        return Err(refuse(i, format!("it would run to {:.2} s, past the end of the timeline at {:.2} s", secs(tl_out), secs(seq.total_duration())), "start it earlier or hold it shorter"));
    }
    let fits = |ti: usize| overlapping(seq, ti, tl_in, tl_out, None).is_none() && !seq.tracks[ti].locked;
    let existing = match track {
        Some(t) => {
            let ti = resolve_track(seq, t, i)?;
            if !matches!(seq.tracks[ti].kind, TrackKind::Text | TrackKind::Video) || !fits(ti) {
                return Err(refuse(i, format!("track {ti} is not a free text track"), "leave the track out and one with room is found or made"));
            }
            Some(ti)
        }
        None => (0..seq.tracks.len()).find(|ti| seq.tracks[*ti].kind == TrackKind::Text && fits(*ti)),
    };
    let mut clip = title_clip(td, tl_in, tl_out, fps);
    // In and out over a fifth of a second.
    let fade = 0.2f64.min(hold / 3.0);
    let mut op = Vec::new();
    klipp_core::keyframe::upsert_keyframe(&mut op, 0.0, 0.0);
    klipp_core::keyframe::upsert_keyframe(&mut op, fade, 1.0);
    klipp_core::keyframe::upsert_keyframe(&mut op, hold - fade, 1.0);
    klipp_core::keyframe::upsert_keyframe(&mut op, hold, 0.0);
    clip.opacity_keyframes = op;
    let id = clip.id;
    let what = format!("{kind} highlight at {x:.2},{y:.2} ({w:.2}\u{00d7}{h:.2}) {:.2}-{:.2} s on {}", secs(tl_in), secs(tl_out), match existing { Some(ti) => format!("track {ti}"), None => "a new text track 0".to_string() });
    let edit = EditCommand::AddTitleClip { clip, track_index: existing.unwrap_or(0), created_track: existing.is_none() };
    Ok((what, edit, vec![id]))
}

/// An animated zoom over the clip: transform keyframes for a push-in or
/// pull-out, optionally moving toward a point. The renderer bakes a
/// per-frame `Transform` from these, so it runs on the GPU.
fn zoom(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, from: f32, to: f32, x: f32, y: f32) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    require_picture(seq, pool, i, ti, clip, "zoom")?;
    for (name, v) in [("from", from), ("to", to)] {
        if !v.is_finite() || !(1.0..=4.0).contains(&v) {
            return Err(refuse(i, format!("{name} {v} is out of range"), format!("give {name} between 1 and 4")));
        }
    }
    for (name, v) in [("x", x), ("y", y)] {
        if !v.is_finite() || !(-1.0..=1.0).contains(&v) {
            return Err(refuse(i, format!("{name} {v} is out of range"), format!("give {name} between -1 and 1")));
        }
    }
    if (from - to).abs() < 1e-4 && x.abs() < 1e-4 && y.abs() < 1e-4 {
        return Err(refuse(i, "nothing to zoom", "give a different from and to, or an x/y to push toward"));
    }
    if from.max(to) < 1.0 + 1e-4 && (from - to).abs() < 1e-4 {
        return Err(refuse(i, "a push at scale 1 has nowhere to go", "give a to above 1: the frame pushes into what the scale takes past its edge"));
    }
    let dur = (clip.timeline_out.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.05);
    let old_state = crate::history::KeyframeSnapshot::from_clip(clip);
    let mut new_state = old_state.clone();
    // Keyframes in the clip's own time: start scale to end scale.
    let mut scale_kfs = Vec::new();
    klipp_core::keyframe::upsert_keyframe(&mut scale_kfs, 0.0, from);
    klipp_core::keyframe::upsert_keyframe(&mut scale_kfs, dur, to);
    new_state.transform_scale_keyframes = scale_kfs;
    // A push toward (x, y), as reframe measures it, at each end's own
    // scale: the position grows with the room the zoom makes, so the
    // picture's edge never shows on the way.
    if x.abs() > 1e-4 || y.abs() > 1e-4 {
        let (w, h) = (seq.resolution.width as f32, seq.resolution.height as f32);
        let mut px = Vec::new();
        klipp_core::keyframe::upsert_keyframe(&mut px, 0.0, -x * reach(from, w));
        klipp_core::keyframe::upsert_keyframe(&mut px, dur, -x * reach(to, w));
        new_state.transform_pos_x_keyframes = px;
        let mut py = Vec::new();
        klipp_core::keyframe::upsert_keyframe(&mut py, 0.0, -y * reach(from, h));
        klipp_core::keyframe::upsert_keyframe(&mut py, dur, -y * reach(to, h));
        new_state.transform_pos_y_keyframes = py;
    }
    let push = if x.abs() > 1e-4 || y.abs() > 1e-4 { format!(", push to {x:.2},{y:.2}") } else { String::new() };
    let what = format!("zoom {}: {from:.2}\u{2192}{to:.2}{push}", clip_name(seq, pool, clip));
    let edit = EditCommand::EditKeyframes {
        clip_id: clip.id,
        linked_clip_id: None,
        description: "Zoom",
        old_state,
        new_state,
        old_linked_state: None,
        new_linked_state: None,
    };
    Ok((what, edit, vec![clip.id]))
}

fn apply_look(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, lut_path: &str, name: Option<&str>, opacity: Option<f32>, remove: bool) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    require_picture(seq, pool, i, ti, clip, "put a look on")?;
    let label = || name.map(|n| n.to_string()).unwrap_or_else(|| std::path::Path::new(lut_path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "a look".into()));
    if remove {
        if !clip.node_graph.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Lut3D && n.params.lut_path.is_some()) {
            return Err(refuse(i, "no look to remove", "this clip has no look on it"));
        }
        let mut graph = clip.node_graph.clone();
        for node in graph.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::Lut3D) {
            node.params.lut_path = None;
            node.params.lut_opacity = None;
        }
        let what = format!("clear the look on {}", clip_name(seq, pool, clip));
        return Ok((what, EditCommand::SetNodeGraph { clip_id: clip.id, old_graph: clip.node_graph.clone(), new_graph: graph }, vec![clip.id]));
    }
    if lut_path.is_empty() {
        return Err(refuse(i, "no LUT given", "give a look to apply"));
    }
    let op = opacity.unwrap_or(1.0);
    if !op.is_finite() || !(0.0..=1.0).contains(&op) {
        return Err(refuse(i, format!("opacity {op} is out of range"), "give opacity between 0 and 1"));
    }
    let mut graph = clip.node_graph.clone();
    if graph.is_identity() {
        graph = klipp_nodes::defaults::default_graph();
    }
    let lut_id = graph.ensure_lut3d();
    let node = graph.nodes.iter_mut().find(|n| n.id == lut_id).expect("ensure_lut3d made it");
    node.params.lut_path = Some(std::path::PathBuf::from(lut_path));
    node.params.lut_opacity = Some(op);
    let what = if (op - 1.0).abs() < 1e-4 {
        format!("look on {}: {}", clip_name(seq, pool, clip), label())
    } else {
        format!("look on {}: {} at {:.0}%", clip_name(seq, pool, clip), label(), op * 100.0)
    };
    Ok((what, EditCommand::SetNodeGraph { clip_id: clip.id, old_graph: clip.node_graph.clone(), new_graph: graph }, vec![clip.id]))
}

/// One sample of a tracked window: where the thing is, and when.
///
/// `x`, `y`, `w`, `h` are fractions of the frame, the top-left corner
/// and the size, as every detector in the program reports a box. `at`
/// is seconds into the clip, so a window tracks with the picture rather
/// than with the timeline and survives the clip being moved or trimmed.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WindowKey {
    pub at: f64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

struct QualifyArgs<'a> {
    target: Option<&'a str>,
    hue: Option<f32>,
    hue_width: Option<f32>,
    sat_low: Option<f32>,
    sat_high: Option<f32>,
    lum_low: Option<f32>,
    lum_high: Option<f32>,
    invert: bool,
    exposure: Option<f32>,
    saturation: Option<f32>,
    hue_shift: Option<f32>,
    temperature: Option<f32>,
    remove: bool,
    window: Option<&'a [WindowKey]>,
    window_feather: Option<f32>,
}

/// A named colour target for the qualifier: its hue centre and width in
/// degrees, where colourists usually key each.
fn qualifier_target(name: &str) -> Option<(f32, f32)> {
    match name.trim().to_lowercase().as_str() {
        "skin" | "skin_tone" | "skintone" | "faces" => Some((28.0, 18.0)),
        "reds" | "red" => Some((0.0, 20.0)),
        "oranges" | "orange" => Some((30.0, 20.0)),
        "yellows" | "yellow" => Some((55.0, 25.0)),
        "greens" | "green" | "foliage" | "grass" => Some((120.0, 50.0)),
        "cyans" | "cyan" | "teal" => Some((190.0, 30.0)),
        "blues" | "blue" | "sky" => Some((220.0, 40.0)),
        "magentas" | "magenta" | "purple" | "purples" => Some((300.0, 30.0)),
        _ => None,
    }
}

fn qualify(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, a: QualifyArgs) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    require_picture(seq, pool, i, ti, clip, "grade a secondary on")?;
    if a.remove {
        let has = clip.node_graph.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Qualifier && n.params.qualifier.as_ref().is_some_and(|q| q.enabled));
        if !has {
            return Err(refuse(i, "no secondary to remove", "this clip has no qualifier on it"));
        }
        let mut graph = clip.node_graph.clone();
        // The keyer off, and the corrections it gated back to neutral.
        let keyed: Vec<klipp_nodes::NodeId> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == klipp_nodes::NodeKind::Qualifier)
            .filter_map(|q| graph.edges.iter().find(|e| e.from_node == q.id && e.to_port == 1).map(|e| e.to_node))
            .collect();
        for node in graph.nodes.iter_mut() {
            if node.kind == klipp_nodes::NodeKind::Qualifier {
                if let Some(q) = node.params.qualifier.as_mut() {
                    q.enabled = false;
                    q.exposure = 0.0;
                    q.saturation = 1.0;
                    q.hue_shift = 0.0;
                    q.temperature = 0.0;
                }
            }
            if keyed.contains(&node.id) {
                for (name, v) in [("hue", 0.0), ("temperature", 0.0), ("tint", 0.0), ("exposure", 0.0), ("saturation", 1.0)] {
                    node.params.apply_override(name, v);
                }
            }
        }
        let what = format!("clear the secondary on {}", clip_name(seq, pool, clip));
        return Ok((what, EditCommand::SetNodeGraph { clip_id: clip.id, old_graph: clip.node_graph.clone(), new_graph: graph }, vec![clip.id]));
    }

    // Resolve a named target to a hue centre/width, unless one was given.
    let (mut hue_center, mut hue_width) = (a.hue, a.hue_width);
    if let Some(name) = a.target {
        let (c, w) = qualifier_target(name).ok_or_else(|| refuse(i, format!("\"{name}\" is not a colour I key"), "target one of: skin, reds, oranges, yellows, greens, cyans, blues, magentas — or give a hue in degrees"))?;
        hue_center.get_or_insert(c);
        hue_width.get_or_insert(w);
    }

    let key_given = hue_center.is_some() || a.sat_low.is_some() || a.sat_high.is_some() || a.lum_low.is_some() || a.lum_high.is_some();
    let corr_given = a.exposure.is_some() || a.saturation.is_some() || a.hue_shift.is_some() || a.temperature.is_some();
    if !key_given {
        return Err(refuse(i, "nothing keyed", "say what to key: a target, a hue, or a saturation or luminance window"));
    }
    if !corr_given {
        return Err(refuse(i, "nothing to change in the keyed range", "give exposure, saturation, hue_shift or temperature for the secondary"));
    }

    // (param name, value, low, high) — each validated then applied.
    let knobs: [(&str, Option<f32>, f32, f32); 10] = [
        ("qual_hue_center", hue_center, 0.0, 360.0),
        ("qual_hue_width", hue_width, 1.0, 180.0),
        ("qual_sat_low", a.sat_low, 0.0, 1.0),
        ("qual_sat_high", a.sat_high, 0.0, 1.0),
        ("qual_lum_low", a.lum_low, 0.0, 1.0),
        ("qual_lum_high", a.lum_high, 0.0, 1.0),
        ("qual_exposure", a.exposure, -3.0, 3.0),
        ("qual_saturation", a.saturation, 0.0, 3.0),
        ("qual_hue_shift", a.hue_shift, -180.0, 180.0),
        ("qual_temperature", a.temperature, -1.0, 1.0),
    ];
    for (name, v, lo, hi) in knobs {
        if let Some(v) = v {
            if !v.is_finite() || v < lo || v > hi {
                return Err(refuse(i, format!("{} {v} is out of range", name.trim_start_matches("qual_")), format!("give it between {lo} and {hi}")));
            }
        }
    }

    let mut graph = clip.node_graph.clone();
    if graph.is_identity() {
        graph = klipp_nodes::defaults::default_graph();
    }
    // A secondary is two nodes: the keyer writes a matte, and the
    // correction it gates gets the change. A qualifier alone in the
    // chain puts its matte on screen where the picture should be.
    let turns_hue = a.hue_shift.is_some() || a.temperature.is_some();
    if turns_hue && (a.exposure.is_some() || a.saturation.is_some()) {
        return Err(refuse(
            i,
            "one secondary either turns the hue or lifts and saturates, not both",
            "send one qualify with hue_shift or temperature, and another with the same key for exposure or saturation",
        ));
    }
    let kind = if turns_hue { klipp_nodes::NodeKind::ColorBalance } else { klipp_nodes::NodeKind::Corrector };
    // A window, when one is given: the key says what, the window says
    // where, and the correction reaches only what is both.
    let window: Vec<(f64, [f32; 4])> = a
        .window
        .unwrap_or(&[])
        .iter()
        .map(|k| (k.at, [k.x, k.y, k.w, k.h]))
        .collect();
    for (n, k) in window.iter().enumerate() {
        let b = k.1;
        if !k.0.is_finite() || k.0 < 0.0 || b.iter().any(|v| !v.is_finite()) || b[2] <= 0.0 || b[3] <= 0.0 || b[0] < -0.5 || b[1] < -0.5 || b[0] + b[2] > 1.5 || b[1] + b[3] > 1.5 {
            return Err(refuse(i, format!("window key {n} is not a box in the frame"), "x, y, w and h are fractions of the frame and at is seconds into the clip"));
        }
    }
    if let Some(f) = a.window_feather {
        if !f.is_finite() || !(0.0..=1.0).contains(&f) {
            return Err(refuse(i, format!("window_feather {f} is out of range"), "give it between 0 and 1"));
        }
    }
    let (qid, cid) = if window.is_empty() {
        graph.ensure_masked_secondary(kind)
    } else {
        let (q, mask, c) = graph.ensure_windowed_secondary(kind);
        let res = seq.resolution;
        let aspect = res.height as f32 / (res.width.max(1) as f32);
        // Soft by default. A colourist softens a face window until its
        // edge cannot be found by looking, and 0.5 of the radius is
        // about where that lands.
        graph.set_window_track(mask, &window, aspect, a.window_feather.unwrap_or(0.5));
        (q, c)
    };
    {
        let node = graph.nodes.iter_mut().find(|n| n.id == qid).expect("the keyer was made");
        for (name, v, _, _) in knobs.iter().take(6) {
            if let Some(v) = v {
                if !node.params.apply_override(name, *v) {
                    return Err(refuse(i, format!("the qualifier has no \"{name}\""), "this is a bug: tell the user the secondary key went nowhere"));
                }
            }
        }
        node.params.apply_override("qual_enabled", 1.0);
        if a.invert {
            node.params.apply_override("qual_invert", 1.0);
        }
    }
    {
        // The correction, in the node's own names: a hue turn is the
        // colour balance's `hue`, the rest the corrector's knobs.
        let node = graph.nodes.iter_mut().find(|n| n.id == cid).expect("the correction was made");
        for (name, v) in [("hue", a.hue_shift), ("temperature", a.temperature), ("exposure", a.exposure), ("saturation", a.saturation)] {
            let Some(v) = v else { continue };
            if !node.params.apply_override(name, v) {
                return Err(refuse(i, format!("the secondary has no \"{name}\""), "this is a bug: tell the user the secondary correction went nowhere"));
            }
        }
    }

    let mut said = Vec::new();
    if let Some(name) = a.target { said.push(name.to_string()); }
    else if let Some(h) = hue_center { said.push(format!("hue {h:.0}°")); }
    if a.invert { said.push("inverted".into()); }
    let mut corr = Vec::new();
    for (label, v) in [("exposure", a.exposure), ("saturation", a.saturation), ("hue_shift", a.hue_shift), ("temperature", a.temperature)] {
        if let Some(v) = v { corr.push(format!("{label} {v}")); }
    }
    if !window.is_empty() {
        said.push(if window.len() > 1 { format!("inside a window tracked over {} samples", window.len()) } else { "inside a window".to_string() });
    }
    let what = format!("secondary on {}: key {}, {}", clip_name(seq, pool, clip), said.join(", "), corr.join(", "));
    Ok((what, EditCommand::SetNodeGraph { clip_id: clip.id, old_graph: clip.node_graph.clone(), new_graph: graph }, vec![clip.id]))
}

/// A rate as a fraction: the NTSC ones over 1001, the rest whole.
fn fps_from_f64(rate: f64) -> Fps {
    let ntsc = rate * 1001.0 / 1000.0;
    if (ntsc - ntsc.round()).abs() < 0.01 && (rate - rate.round()).abs() > 0.01 {
        Fps::new((ntsc.round() as u32) * 1000, 1001)
    } else {
        Fps::new(rate.round().max(1.0) as u32, 1)
    }
}

fn slip(seq: &Sequence, pool: &MediaPool, fps: Fps, i: usize, ti: usize, clip: &Clip, by: f64) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let ClipKind::Media { media_id } = &clip.kind else {
        return Err(refuse(i, format!("{} is not a media clip", clip_name(seq, pool, clip)), "slip a clip from a file"));
    };
    if seq.tracks[ti].locked {
        return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
    }
    let item = pool.get(media_id).ok_or_else(|| refuse(i, format!("no media item {}", media_id.0), "media ids are in list_media"))?;
    let shift = snap(by.abs(), fps);
    let (new_in, new_out) = if by >= 0.0 { (clip.source_in + shift, clip.source_out + shift) } else { (clip.source_in - shift, clip.source_out - shift) };
    if new_in.value < 0 {
        return Err(refuse(i, format!("{} would start {:.2} s before the file begins", clip_name(seq, pool, clip), -secs(new_in)), "slip it by less"));
    }
    if secs(new_out) > secs(item.duration) + 1e-6 {
        return Err(refuse(i, format!("{} would run {:.2} s past the end of {}", clip_name(seq, pool, clip), secs(new_out) - secs(item.duration), item.filename), "slip it by less"));
    }
    if shift.value == 0 {
        return Err(refuse(i, "a slip of no frames", "give secs of at least a frame"));
    }
    let what = format!("slip {} by {:+.2} s: it now shows {:.2}-{:.2} s of the file, where it was", clip_name(seq, pool, clip), if by >= 0.0 { secs(shift) } else { -secs(shift) }, secs(new_in), secs(new_out));
    Ok((what, EditCommand::LegacySlipClip { clip_id: clip.id, old_source_in: clip.source_in, old_source_out: clip.source_out, new_source_in: new_in, new_source_out: new_out }, vec![clip.id]))
}

fn stabilize(seq: &Sequence, pool: &MediaPool, i: usize, ti: usize, clip: &Clip, smoothing: f32) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    require_picture(seq, pool, i, ti, clip, "stabilise")?;
    if !smoothing.is_finite() || !(0.0..=1.0).contains(&smoothing) {
        return Err(refuse(i, format!("smoothing {smoothing} is out of range"), "give smoothing between 0 and 1"));
    }
    use klipp_effects::BuiltinEffect as E;
    let fx = E::VideoStabilization { smoothing, crop_ratio: 0.05 + smoothing * 0.05, zoom: true };
    let mut effects: Vec<E> = clip.builtin_effects.iter().filter(|e| !matches!(e, E::VideoStabilization { .. })).cloned().collect();
    effects.push(fx);
    let what = format!("stabilise {} at {smoothing:.2}", clip_name(seq, pool, clip));
    Ok((what, set_builtin_effects(clip, effects), vec![clip.id]))
}

fn retime(seq: &Sequence, pool: &MediaPool, fps: Fps, i: usize, ti: usize, ci: usize, clip: &Clip, speed: f64) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    if seq.tracks[ti].locked {
        return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
    }
    if !matches!(clip.kind, ClipKind::Media { .. }) {
        return Err(refuse(i, format!("{} cannot be retimed", clip_name(seq, pool, clip)), "retime a media clip"));
    }
    if !speed.is_finite() || !(0.1..=8.0).contains(&speed) {
        return Err(refuse(i, format!("a speed of {speed}"), "give speed between 0.1 and 8"));
    }
    let source_len = clip.source_out - clip.source_in;
    let new_len_secs = crate::ops::secs(source_len) / speed;
    let new_out = clip.timeline_in + snap(new_len_secs, fps);
    if new_out <= clip.timeline_in + one_frame(fps) {
        return Err(refuse(i, "the clip would be shorter than a frame", "use a smaller speed"));
    }
    // Overlap with the next clip is the caller's to close: said, not fixed.
    let runs_into = seq.tracks[ti].clips.get(ci + 1).filter(|n| new_out > n.timeline_in).map(|n| clip_name(seq, pool, n));
    let old_out = clip.timeline_out;
    let what = format!(
        "retime {} to {speed:.2}x (now {:.2}-{:.2} s){}",
        clip_name(seq, pool, clip),
        crate::ops::secs(clip.timeline_in),
        crate::ops::secs(new_out),
        runs_into.map(|n| format!("; it now overlaps {n} — move or ripple to close it")).unwrap_or_default(),
    );
    let edit = EditCommand::SetClipSpeed {
        clip_id: clip.id,
        old_speed: clip.speed,
        new_speed: speed,
        old_timeline_out: old_out,
        new_timeline_out: new_out,
    };
    Ok((what, edit, vec![clip.id]))
}

fn clean_audio(seq: &Sequence, pool: &MediaPool, i: usize, given: &Clip, wind: bool, hiss: bool, hum: bool, deess: bool) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let clip = sound_settings_clip(seq, given);
    if !clip.media_id().and_then(|id| pool.get(&id)).is_some_and(|m| m.has_audio) {
        return Err(refuse(i, format!("{} has no sound", clip_name(seq, pool, clip)), "name a clip whose media has audio"));
    }
    if !(wind || hiss || hum || deess) {
        return Err(refuse(i, "nothing to clean", "set wind, hiss, hum or deess"));
    }
    let mk = |effect_type: &str, params: Vec<(&str, f32)>| klipp_core::AudioEffectInstance {
        effect_type: effect_type.to_string(),
        enabled: true,
        wet_dry: 1.0,
        params: params.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    };
    let mut chain = clip.audio_effects.clone();
    let has = |chain: &[klipp_core::AudioEffectInstance], t: &str| chain.iter().any(|e| e.effect_type == t);
    let mut said = Vec::new();
    // Wind and hiss both go to the broadband reducer, wind stronger; one
    // of it, so two flags do not stack two reducers.
    if (wind || hiss) && !has(&chain, "noise_reduction") {
        let reduction = if wind { 0.7 } else { 0.4 };
        chain.push(mk("noise_reduction", vec![("reduction", reduction), ("sensitivity", 0.5), ("smoothing", 0.5)]));
        said.push(if wind { "wind and rumble reduced" } else { "hiss reduced" });
    }
    if hum && !has(&chain, "hum_removal") {
        chain.push(mk("hum_removal", vec![("fundamental_hz", 50.0), ("harmonics", 4.0), ("q", 30.0)]));
        said.push("mains hum removed");
    }
    if deess && !has(&chain, "deesser") {
        chain.push(mk("deesser", vec![("frequency", 6500.0), ("threshold", -24.0), ("reduction", 6.0), ("bandwidth", 1.0)]));
        said.push("sibilance tamed");
    }
    if said.is_empty() {
        return Err(refuse(i, "the sound already has these cleaners", "nothing to add"));
    }
    let mut new_state = crate::history::AudioChainSnapshot::from_clip(clip);
    new_state.audio_effects = chain;
    let on = if clip.id == given.id { String::new() } else { " (on the linked audio)".to_string() };
    let what = format!("clean {}: {}{on}", clip_name(seq, pool, given), said.join(", "));
    let edit = EditCommand::EditAudioChain {
        clip_id: clip.id,
        description: "Clean Audio",
        old_state: crate::history::AudioChainSnapshot::from_clip(clip),
        new_state,
    };
    Ok((what, edit, vec![given.id]))
}

/// A transition between `clip` and the clip after it on its track. The
/// registry names the kind; a stylised one is spelt by its key or a
/// friendly alias. Both clips need source beyond the cut to overlap by.
#[allow(clippy::too_many_arguments)]
fn transition(
    seq: &Sequence,
    pool: &MediaPool,
    fps: Fps,
    i: usize,
    ti: usize,
    ci: usize,
    clip: &Clip,
    kind: &str,
    secs: f64,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    if seq.tracks[ti].locked {
        return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
    }
    let Some(next) = seq.tracks[ti].clips.get(ci + 1) else {
        return Err(refuse(i, format!("{} has no clip after it on its track", clip_name(seq, pool, clip)), "a transition goes between two clips meeting at a cut"));
    };
    if clip.timeline_out != next.timeline_in {
        return Err(refuse(i, format!("{} and the clip after it do not meet at a cut", clip_name(seq, pool, clip)), "a transition needs two clips edge to edge"));
    }
    let spec = resolve_transition_kind(kind).ok_or_else(|| {
        refuse(i, format!("\"{kind}\" is not a transition"), "use cross_dissolve, dip_to_black, dip_to_white, or a stylised one like zoom, whip, glitch, film_burn; a straight cut needs no transition")
    })?;
    if !(0.06..=4.0).contains(&secs) {
        return Err(refuse(i, format!("a transition of {secs} s"), "give secs between 0.06 and 4"));
    }
    let dur = snap(secs, fps).max(one_frame(fps));
    // Both clips overlap the cut by half the transition: each needs that
    // much source beyond its edge, and to stay longer than the overlap.
    let half = dur.to_secs_f64() / 2.0;
    let half_tc = snap(half, fps);
    let media_dur = |c: &Clip| c.media_id().and_then(|m| pool.get(&m)).map(|m| m.duration);
    let a_room = media_dur(clip).is_none_or(|d| clip.source_out + half_tc <= d);
    let _ = media_dur;
    let b_room = next.source_in >= half_tc;
    if !a_room || !b_room {
        let who = if !a_room { clip_name(seq, pool, clip) } else { clip_name(seq, pool, next) };
        return Err(refuse(i, format!("{who} has no footage beyond the cut to make the {secs:.2} s overlap"), "shorten the transition, or trim the clip so there is a handle"));
    }
    if dur >= clip.timeline_out - clip.timeline_in || dur >= next.timeline_out - next.timeline_in {
        return Err(refuse(i, "the transition is as long as one of its clips", "shorten it, or lengthen the clips"));
    }
    let replaced = seq.tracks[ti].transitions.iter().find(|t| t.clip_a_id == clip.id && t.clip_b_id == next.id).cloned();
    let transition = klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        kind: spec.kind,
        duration: dur,
        clip_a_id: clip.id,
        clip_b_id: next.id,
        user_params: klipp_core::TransitionUserParams::default_for_kind(spec.kind_id),
        alignment: klipp_core::TransitionAlignment::default(),
    };
    let at = clip.timeline_out.to_secs_f64();
    let what = format!(
        "{} of {:.2} s between {} and {} at {at:.2} s",
        spec.display_name,
        dur.to_secs_f64(),
        clip_name(seq, pool, clip),
        clip_name(seq, pool, next),
    );
    let edit = EditCommand::AddTransition {
        track_index: ti,
        transition,
        clip_a_id: clip.id,
        clip_b_id: next.id,
        old_fade_out_a: clip.fade_out_secs,
        old_fade_in_b: next.fade_in_secs,
        replaced,
    };
    Ok((what, edit, vec![clip.id, next.id]))
}

/// A transition spec from a name: its registry key (any case), or a
/// friendly alias for the ones people ask for by feel.
fn resolve_transition_kind(name: &str) -> Option<&'static klipp_core::TransitionSpec> {
    let key = name.trim().to_lowercase().replace([' ', '-'], "_");
    let alias = match key.as_str() {
        "dissolve" | "crossfade" | "cross_fade" => "CrossDissolve",
        "dip_to_black" | "dip_black" | "fade_to_black" | "fade_black" => "DipToBlack",
        "dip_to_white" | "dip_white" | "fade_to_white" | "flash_white" => "DipToWhite",
        "zoom" | "zoom_in" | "punch_in" => "ZoomInDissolve",
        "zoom_out" => "ZoomOutDissolve",
        "whip" | "whip_pan" | "swish" | "shake" => "CrossZoom",
        "glitch" | "digital" => "GlitchDissolve",
        "film_burn" | "burn" | "light_leak" => "FilmBurn",
        "vanish" | "dip" => "DipToBlack",
        "blur" => "BlurDissolve",
        "spin" => "Spin",
        other => {
            // The registry key itself, matched without case.
            return klipp_core::TRANSITION_SPECS.iter().find(|s| s.string_key.eq_ignore_ascii_case(&other.replace('_', "")));
        }
    };
    klipp_core::spec_for_string_key(alias)
}

/// A clip's primary grade set from named knobs: what is given lands on
/// the clip's Corrector node, the rest of the grade stays, recorded as
/// one whole-graph edit so a single undo takes it back.
fn grade(
    seq: &Sequence,
    pool: &MediaPool,
    i: usize,
    ti: usize,
    clip: &Clip,
    knobs: &[(&str, &str, Option<f32>, f32, f32)],
    reset: bool,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    if seq.tracks[ti].locked {
        return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
    }
    if !matches!(clip.kind, ClipKind::Media { .. }) {
        return Err(refuse(i, format!("{} is not a picture clip", clip_name(seq, pool, clip)), "grade a media clip; a title or a generator has no grade"));
    }
    if !reset && knobs.iter().all(|(_, _, v, _, _)| v.is_none()) {
        return Err(refuse(i, "nothing to grade", "give exposure, contrast, saturation, temperature, tint, lift, gamma, gain, highlights or shadows"));
    }
    for (name, _, v, lo, hi) in knobs {
        if let Some(v) = v {
            if !v.is_finite() || v < lo || v > hi {
                return Err(refuse(i, format!("{name} {v} is out of range"), format!("give {name} between {lo} and {hi}")));
            }
        }
    }
    let mut graph = if reset { klipp_nodes::defaults::default_graph() } else { clip.node_graph.clone() };
    // A clip that has never been graded has an empty graph: it needs the
    // primary chain before there is a grade to set.
    if graph.is_identity() || !graph.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Corrector) {
        graph = klipp_nodes::defaults::default_graph();
    }
    let corrector = graph.ensure_corrector();
    let node = graph.nodes.iter_mut().find(|n| n.id == corrector).expect("ensure_corrector made it");
    let mut said = Vec::new();
    for (name, param, v, _, _) in knobs {
        if let Some(v) = v {
            if !node.params.apply_override(param, *v) {
                return Err(refuse(i, format!("the grade has no \"{param}\""), "this is a bug: tell the user the grade param went nowhere"));
            }
            said.push(format!("{name} {v}"));
        }
    }
    let what = format!(
        "grade {}: {}",
        clip_name(seq, pool, clip),
        if said.is_empty() { "back to neutral".to_string() } else { format!("{}{}", if reset { "neutral, then " } else { "" }, said.join(", ")) },
    );
    let edit = EditCommand::SetNodeGraph { clip_id: clip.id, old_graph: clip.node_graph.clone(), new_graph: graph };
    Ok((what, edit, vec![clip.id]))
}

/// `#rrggbb` or `#rrggbbaa`.
fn parse_color(c: &str) -> Option<[u8; 4]> {
    let hex = c.trim().strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |k: usize| u8::from_str_radix(&hex[k..k + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?, if hex.len() == 8 { byte(6)? } else { 255 }])
}

/// `look` on one title: its text boxes take the size, colour, face and
/// alignment, every layer moves with the first text's centre, the
/// panel behind it is as dark as asked.
fn restyle(td: &klipp_timeline::TitleData, look: &Look) -> klipp_timeline::TitleData {
    use klipp_timeline::{FontStyle, TitleBackground, TitleObjectContent};
    let mut new = td.clone();
    let first_centre = new.objects.iter().find(|o| matches!(o.content, TitleObjectContent::TextBox(_))).map(|o| (o.transform.center.x, o.transform.center.y));
    // Sizes keep their proportions: a lower third's role stays smaller than its name.
    let first_size = new.objects.iter().find_map(|o| match &o.content { TitleObjectContent::TextBox(tb) => Some(tb.font_size), _ => None }).unwrap_or(1.0).max(1.0);
    for o in new.objects.iter_mut() {
        if let (Some((cx, cy)), true) = (first_centre, look.x.is_some() || look.y.is_some()) {
            let (dx, dy) = (look.x.map(|x| x - cx).unwrap_or(0.0), look.y.map(|y| y - cy).unwrap_or(0.0));
            o.transform.center.x = (o.transform.center.x + dx).clamp(0.02, 0.98);
            o.transform.center.y = (o.transform.center.y + dy).clamp(0.02, 0.98);
        }
        let TitleObjectContent::TextBox(tb) = &mut o.content else { continue };
        if let Some(size) = look.size {
            tb.font_size = (tb.font_size / first_size * size).max(6.0);
        }
        if let Some(c) = look.color {
            tb.color = c;
        }
        if let Some(f) = &look.font {
            tb.font_name = f.clone();
        }
        if let Some(b) = look.bold {
            tb.font_style = match (tb.font_style, b) {
                (FontStyle::Italic | FontStyle::BoldItalic, true) => FontStyle::BoldItalic,
                (FontStyle::Italic | FontStyle::BoldItalic, false) => FontStyle::Italic,
                (_, true) => FontStyle::Bold,
                (_, false) => FontStyle::Regular,
            };
        }
        if let Some(a) = look.align {
            tb.alignment = a;
        }
        if let Some(w) = look.outline {
            tb.outlines.clear();
            if w > 0.0 {
                tb.outlines.push(klipp_timeline::TextOutline { color: [0, 0, 0, 200], width: w, enabled: true, width_keyframes: Vec::new() });
            }
        }
    }
    if let Some(color) = look.full_frame {
        new.background = TitleBackground::FullFrame { color };
    } else if let Some(dark) = look.backing {
        new.background = if dark <= 0.0 {
            TitleBackground::None
        } else {
            // Composited in linear light: an alpha of 0.72 only halves the
            // picture's brightness to the eye. This is the alpha that
            // looks as dark as asked.
            let alpha = 1.0 - (1.0 - dark.min(1.0)).powf(2.2);
            TitleBackground::Color { color: [0, 0, 0, (alpha * 255.0).round() as u8], padding: 12.0 }
        };
    }
    new
}

#[allow(clippy::too_many_arguments)]
fn style(
    seq: &Sequence,
    pool: &MediaPool,
    i: usize,
    one: Option<(usize, &Clip)>,
    track: Option<usize>,
    from: Option<Timecode>,
    to: Option<Timecode>,
    look: &Look,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    if *look == Look::default() {
        return Err(refuse(i, "nothing to change", "give size, color, font, bold, align, x, y, backing or outline"));
    }
    for (name, v, lo, hi) in [("size", look.size, 6.0, 400.0), ("x", look.x, 0.0, 1.0), ("y", look.y, 0.0, 1.0), ("backing", look.backing, 0.0, 1.0), ("outline", look.outline, 0.0, 40.0)] {
        if let Some(v) = v {
            if !v.is_finite() || v < lo || v > hi {
                return Err(refuse(i, format!("{name} {v} is out of range"), format!("give {name} between {lo} and {hi}")));
            }
        }
    }
    let targets: Vec<(usize, &Clip)> = match (one, track) {
        (Some(_), Some(_)) => return Err(refuse(i, "both a clip and a track were named", "name one clip, or a track for every title on it")),
        (None, None) => return Err(refuse(i, "no clip or track was named", "name a clip, or a track such as \"subtitle\" for all the captions")),
        (Some((ti, c)), None) => vec![(ti, c)],
        (None, Some(ti)) => seq.tracks[ti]
            .clips
            .iter()
            .filter(|c| c.title_data().is_some())
            .filter(|c| from.is_none_or(|f| c.timeline_out > f) && to.is_none_or(|t| c.timeline_in < t))
            .map(|c| (ti, c))
            .collect(),
    };
    if targets.is_empty() {
        return Err(refuse(i, "there is no title there to restyle", "name a text or subtitle track that has titles in that range"));
    }
    let mut edits = Vec::new();
    let mut ids = Vec::new();
    for (ti, c) in &targets {
        let Some(old) = c.title_data() else {
            return Err(refuse(i, format!("{} is not a title or a caption", clip_name(seq, pool, c)), "name a clip on a text or subtitle track"));
        };
        if seq.tracks[*ti].locked {
            return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
        }
        let new = restyle(old, look);
        if new != *old {
            edits.push(EditCommand::EditTitle { clip_id: c.id, description: "Restyle Title", old_data: old.clone(), new_data: new });
            ids.push(c.id);
        }
    }
    if edits.is_empty() {
        return Err(refuse(i, "they look like that already", "nothing to do"));
    }
    let mut said = Vec::new();
    if let Some(v) = look.size { said.push(format!("size {v:.0}")); }
    if let Some(c) = look.color { said.push(format!("colour #{:02x}{:02x}{:02x}", c[0], c[1], c[2])); }
    if let Some(f) = &look.font { said.push(format!("face {f}")); }
    if let Some(b) = look.bold { said.push(if b { "bold".into() } else { "not bold".into() }); }
    if let Some(a) = look.align { said.push(format!("{a:?}").to_lowercase()); }
    if look.x.is_some() || look.y.is_some() { said.push(format!("centre at {}, {}", look.x.map(|x| format!("{x:.2}")).unwrap_or_else(|| "its x".into()), look.y.map(|y| format!("{y:.2}")).unwrap_or_else(|| "its y".into()))); }
    if let Some(d) = look.backing { said.push(if d <= 0.0 { "no backing".into() } else { format!("backing {:.0}% dark", d * 100.0) }); }
    if let Some(w) = look.outline { said.push(if w <= 0.0 { "no outline".into() } else { format!("outline {w:.0} px") }); }
    let first = targets.first().map(|(_, c)| secs(c.timeline_in)).unwrap_or(0.0);
    let last = targets.last().map(|(_, c)| secs(c.timeline_out)).unwrap_or(0.0);
    let what = format!("restyle {} title{} from {first:.2} to {last:.2} s: {}", edits.len(), if edits.len() == 1 { "" } else { "s" }, said.join(", "));
    let touched: Vec<ClipId> = ids.first().into_iter().chain(ids.last().filter(|_| ids.len() > 1)).copied().collect();
    let edit = if edits.len() == 1 { edits.remove(0) } else { EditCommand::Batch(edits) };
    Ok((what, edit, touched))
}

/// A two-line lower third, bottom left inside title-safe: the name, and
/// the role smaller and spaced under it, both lifted off the picture by
/// a soft shadow, fading in and out.
/// Where a lower third sits: the edge its text is set against, the
/// name's line, and whether that edge is the right one.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LowerThirdPlace {
    right: bool,
    x: Option<f32>,
    y: Option<f32>,
}

/// The role sits this far under the name.
const LOWER_THIRD_ROLE_DROP: f32 = 0.053;

fn lower_third_place(i: usize, side: Option<&str>, x: Option<f32>, y: Option<f32>, with_role: bool) -> Result<LowerThirdPlace, Refusal> {
    let right = match side.map(|s| s.trim().to_lowercase()).as_deref() {
        None | Some("") | Some("left") => false,
        Some("right") => true,
        Some(other) => return Err(refuse(i, format!("\"{other}\" is not a side"), "use left or right")),
    };
    // Title-safe is the inner nine tenths of the frame.
    if let Some(x) = x {
        if !(0.05..=0.95).contains(&x) {
            return Err(refuse(i, format!("x {x} is outside title-safe"), "x is a fraction of the width, from 0.05 to 0.95"));
        }
    }
    if let Some(y) = y {
        let lowest = if with_role { 0.95 - LOWER_THIRD_ROLE_DROP - 0.02 } else { 0.93 };
        if !(0.05..=lowest).contains(&y) {
            return Err(refuse(i, format!("y {y} is outside title-safe"), format!("y is the name's line as a fraction of the height, from 0.05 to {lowest:.2}")));
        }
    }
    Ok(LowerThirdPlace { right, x, y })
}

fn lower_third_title(name: &str, role: Option<&str>, place: LowerThirdPlace) -> klipp_timeline::TitleData {
    use klipp_timeline::{default_text_transform, Easing, FontStyle, Motion, MotionKind, ObjectTransform, TextAlignment, TextBox, TitleMotion, TitleObjectContent, TitlePosition};
    let lift = klipp_timeline::TitleShadow {
        color: [0, 0, 0, 120],
        offset_x: 0.0,
        offset_y: 3.0,
        blur: 12.0,
        enabled: true,
        offset_x_keyframes: Vec::new(),
        offset_y_keyframes: Vec::new(),
        blur_keyframes: Vec::new(),
    };
    let motion = |delay: f32| {
        TitleMotion::both(
            Motion::new(MotionKind::Fade).duration(0.4).easing(Easing::OutCubic).delay(delay),
            Motion::new(MotionKind::Fade).duration(0.4).easing(Easing::InCubic),
        )
    };
    let edge = place.x.unwrap_or(if place.right { 0.92 } else { 0.08 });
    let name_y = place.y.unwrap_or(if role.is_some() { 0.815 } else { 0.85 });
    let alignment = if place.right { TextAlignment::Right } else { TextAlignment::Left };
    let at = |y: f32| ObjectTransform { center: TitlePosition { x: edge, y }, anchor_norm: TitlePosition { x: if place.right { 1.0 } else { 0.0 }, y: 0.5 }, ..default_text_transform() };
    let mut td = klipp_timeline::TitleData {
        objects: Vec::new(),
        next_object_id: 0,
        animation_type: Default::default(),
        background: klipp_timeline::TitleBackground::None,
    };
    let mut name_box = TextBox {
        text: name.to_string(),
        font_size: 46.0,
        font_name: "Inter".into(),
        font_style: FontStyle::Bold,
        color: [255, 255, 255, 255],
        alignment,
        ..Default::default()
    };
    name_box.shadows.push(lift.clone());
    name_box.motion = motion(0.0);
    td.add_object(TitleObjectContent::TextBox(name_box), at(name_y));
    if let Some(role) = role {
        let mut role_box = TextBox {
            text: role.to_string(),
            font_size: 24.0,
            font_name: "Inter".into(),
            font_style: FontStyle::Regular,
            color: [205, 205, 212, 255],
            alignment,
            ..Default::default()
        };
        role_box.case_transform = klipp_timeline::TextCase::Uppercase;
        role_box.tracking = 3.0;
        role_box.shadows.push(lift);
        role_box.motion = motion(0.1);
        td.add_object(TitleObjectContent::TextBox(role_box), at(name_y + LOWER_THIRD_ROLE_DROP));
    }
    td
}

#[allow(clippy::too_many_arguments)]
fn default_title_secs() -> f64 {
    3.0
}

struct TitleArgs<'a> {
    text: &'a str,
    secs: f64,
    track: Option<&'a TrackRef>,
    x: Option<f32>,
    y: Option<f32>,
    size: Option<f32>,
    color: Option<&'a str>,
    font: Option<String>,
    bold: Option<bool>,
    align: Option<&'a str>,
    backing: Option<f32>,
    backing_color: Option<&'a str>,
    full_frame: Option<&'a str>,
    outline: Option<f32>,
    outline_color: Option<&'a str>,
    glow: Option<f32>,
    glow_color: Option<&'a str>,
    gradient: Option<&'a [String; 2]>,
    animate: Option<&'a str>,
    animate_secs: Option<f32>,
    stagger: Option<&'a str>,
    loop_effect: Option<&'a str>,
    glitch: Option<f32>,
    chromatic: Option<f32>,
    /// Faces under the title's span: the words keep clear of them.
    keep_out: Vec<[f32; 4]>,
}

/// Build a styled, animated title from scratch and place it on a text
/// track — the general-purpose sibling of `lower_third`/`captions`.
fn title_op(seq: &Sequence, fps: Fps, i: usize, at: Timecode, a: TitleArgs) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    use klipp_core::title_motion::{Motion, MotionKind, StaggerUnit, TitleLoop, TitleMotion};
    use klipp_timeline::{FontStyle, TextAlignment, TextBox, TextGlow, TextGradient, TextOutline, TitleBackground, TitleData};
    let text = a.text.trim();
    if text.is_empty() {
        return Err(refuse(i, "a title needs some text", "give the words to show"));
    }
    if !a.secs.is_finite() || a.secs < 0.2 {
        return Err(refuse(i, format!("{} s is too short", a.secs), "hold it at least a fraction of a second"));
    }
    let col = |c: Option<&str>, what: &str| -> Result<Option<[u8; 4]>, Refusal> {
        c.map(|s| parse_color(s).ok_or_else(|| refuse(i, format!("\"{s}\" is not a colour"), format!("write {what} as #rrggbb or #rrggbbaa"))))
            .transpose()
    };

    let mut tb = TextBox { text: text.to_string(), ..Default::default() };
    if let Some(s) = a.size {
        if !s.is_finite() || !(4.0..=400.0).contains(&s) {
            return Err(refuse(i, format!("size {s} is out of range"), "give size between 4 and 400"));
        }
        tb.font_size = s;
    }
    if let Some(c) = col(a.color, "the colour")? {
        tb.color = c;
    }
    if let Some(f) = a.font {
        tb.font_name = f;
    }
    tb.font_style = match a.bold {
        Some(true) => FontStyle::Bold,
        Some(false) => FontStyle::Regular,
        None => tb.font_style,
    };
    if let Some(al) = a.align {
        tb.alignment = match al.to_lowercase().as_str() {
            "left" => TextAlignment::Left,
            "center" | "centre" => TextAlignment::Center,
            "right" => TextAlignment::Right,
            other => return Err(refuse(i, format!("\"{other}\" is not an alignment"), "use left, center or right")),
        };
    }
    if let Some(w) = a.outline {
        if !w.is_finite() || !(0.0..=40.0).contains(&w) {
            return Err(refuse(i, format!("outline {w} is out of range"), "give an outline width 0 to 40"));
        }
        let color = col(a.outline_color, "the outline")?.unwrap_or([0, 0, 0, 255]);
        tb.outlines = vec![TextOutline { enabled: true, color, width: w, width_keyframes: Vec::new() }];
    }
    if let Some(g) = a.glow {
        if !g.is_finite() || !(0.0..=1.0).contains(&g) {
            return Err(refuse(i, format!("glow {g} is out of range"), "give a glow 0 to 1"));
        }
        let color = col(a.glow_color, "the glow")?.unwrap_or(tb.color);
        tb.glows = vec![TextGlow { enabled: true, color, radius: 18.0, radius_keyframes: Vec::new(), intensity: g, intensity_keyframes: Vec::new() }];
    }
    if let Some(gr) = a.gradient {
        let top = parse_color(&gr[0]).ok_or_else(|| refuse(i, format!("\"{}\" is not a colour", gr[0]), "write the gradient colours as #rrggbb"))?;
        let bottom = parse_color(&gr[1]).ok_or_else(|| refuse(i, format!("\"{}\" is not a colour", gr[1]), "write the gradient colours as #rrggbb"))?;
        tb.gradient = Some(TextGradient { top_color: top, bottom_color: bottom, enabled: true });
    }
    if let Some(gl) = a.glitch {
        if !gl.is_finite() || !(0.0..=1.0).contains(&gl) {
            return Err(refuse(i, format!("glitch {gl} is out of range"), "give a glitch 0 to 1"));
        }
        tb.glitch_intensity = gl;
    }
    if let Some(ch) = a.chromatic {
        if !ch.is_finite() || !(0.0..=1.0).contains(&ch) {
            return Err(refuse(i, format!("chromatic {ch} is out of range"), "give a chromatic 0 to 1"));
        }
        tb.chromatic_aberration_px = ch * 12.0;
    }
    if let Some(an) = a.animate {
        let kind = match an.to_lowercase().as_str() {
            "fade" => MotionKind::Fade,
            "rise" => MotionKind::Rise,
            "drop" => MotionKind::Drop,
            "slide_left" | "slide" | "slideleft" => MotionKind::SlideLeft,
            "slide_right" | "slideright" => MotionKind::SlideRight,
            "scale_up" | "scaleup" | "zoom_in" | "pop" => MotionKind::ScaleUp,
            "scale_down" | "scaledown" => MotionKind::ScaleDown,
            "blur" | "focus" => MotionKind::Blur,
            "typewriter" | "type" => MotionKind::Typewriter,
            other => return Err(refuse(i, format!("\"{other}\" is not an entrance"), "use fade, rise, drop, slide_left, slide_right, scale_up, scale_down, blur or typewriter")),
        };
        let mut m = Motion::new(kind).duration(a.animate_secs.unwrap_or(0.6).clamp(0.05, 5.0));
        match a.stagger {
            Some(st) => {
                let unit = match st.to_lowercase().as_str() {
                    "none" => StaggerUnit::None,
                    "line" => StaggerUnit::Line,
                    "word" => StaggerUnit::Word,
                    "char" | "character" => StaggerUnit::Char,
                    other => return Err(refuse(i, format!("\"{other}\" is not a stagger"), "use none, line, word or char")),
                };
                m = m.stagger(unit, 0.05);
            }
            None if matches!(kind, MotionKind::Typewriter) => m = m.stagger(StaggerUnit::Char, 0.04),
            None => {}
        }
        tb.motion = TitleMotion::enter(m);
    }
    if let Some(lp) = a.loop_effect {
        let l = match lp.to_lowercase().as_str() {
            "pulse" | "breathe" => TitleLoop::Pulse { rate_hz: 1.0, min_opacity: 0.4 },
            "flash" | "blink" => TitleLoop::Blink { rate_hz: 2.0, duty: 0.6 },
            "neon" => TitleLoop::ColorBlend { color_a: tb.color, color_b: tb.glows.first().map(|g| g.color).unwrap_or([255, 40, 120, 255]), rate_hz: 1.5 },
            "wave" => TitleLoop::Wave { amplitude: 0.12, rate_hz: 1.0 },
            "shake" => TitleLoop::Shake { amplitude: 0.04, rate_hz: 10.0 },
            "color_cycle" | "colour_cycle" | "rainbow" => TitleLoop::ColorCycle { rate_hz: 0.4 },
            other => return Err(refuse(i, format!("\"{other}\" is not a loop"), "use pulse, flash, neon, wave, shake or color_cycle")),
        };
        tb.loops = vec![l];
    }

    let mut td = TitleData::single_text(tb);
    if a.x.is_some() || a.y.is_some() {
        if let Some(obj) = td.objects.get_mut(0) {
            // Inside the title-safe area: nothing sits on the frame's edge,
            // where a player's chrome or an overscan crop would take it.
            if let Some(x) = a.x {
                obj.transform.center.x = x.clamp(0.05, 0.95);
            }
            if let Some(y) = a.y {
                obj.transform.center.y = y.clamp(0.05, 0.95);
            }
        }
    }
    // Never over a face: the words move to the nearest clear zone and
    // the reply says so. A full-frame card has nothing behind it to keep
    // clear of.
    let mut moved: Option<&'static str> = None;
    if !a.keep_out.is_empty() && a.full_frame.is_none() {
        let font = td.font_size();
        if let Some(obj) = td.objects.get_mut(0) {
            let size = text_extent(text, font);
            let center = (obj.transform.center.x, obj.transform.center.y);
            match clear_zone(center, size, &a.keep_out) {
                Some((c, Some(zone))) => {
                    obj.transform.center.x = c.0;
                    obj.transform.center.y = c.1;
                    moved = Some(zone);
                }
                Some(_) => {}
                None => return Err(refuse(i, "a face is under every zone of the frame for the title's whole span", "hold it where the shot has no face, or make it a card")),
            }
        }
    }
    if let Some(ff) = a.full_frame {
        let color = parse_color(ff).ok_or_else(|| refuse(i, format!("\"{ff}\" is not a colour"), "write the full-frame colour as #rrggbb"))?;
        td.background = TitleBackground::FullFrame { color };
    } else if let Some(bc) = a.backing_color {
        let color = parse_color(bc).ok_or_else(|| refuse(i, format!("\"{bc}\" is not a colour"), "write the backing colour as #rrggbb"))?;
        td.background = TitleBackground::Color { color, padding: 0.3 };
    } else if let Some(b) = a.backing {
        let alpha = (b.clamp(0.0, 1.0) * 255.0) as u8;
        td.background = TitleBackground::Color { color: [0, 0, 0, alpha], padding: 0.3 };
    }

    let tl_in = at;
    let tl_out = tl_in + snap(a.secs, fps);
    if tl_out > seq.total_duration() {
        return Err(refuse(i, format!("it would run to {:.2} s, past the end of the timeline at {:.2} s", secs(tl_out), secs(seq.total_duration())), "start it earlier or hold it shorter"));
    }
    let fits = |ti: usize| overlapping(seq, ti, tl_in, tl_out, None).is_none() && !seq.tracks[ti].locked;
    let existing = match a.track {
        Some(t) => {
            let ti = resolve_track(seq, t, i)?;
            if !matches!(seq.tracks[ti].kind, TrackKind::Text | TrackKind::Video) {
                return Err(refuse(i, format!("track {ti} is not a text track"), "name a text track, or leave the track out"));
            }
            if !fits(ti) {
                return Err(refuse(i, format!("track {ti} is locked or has a clip there"), "leave the track out and one with room is found or made"));
            }
            Some(ti)
        }
        None => (0..seq.tracks.len()).find(|ti| seq.tracks[*ti].kind == TrackKind::Text && fits(*ti)),
    };
    let clip = title_clip(td, tl_in, tl_out, fps);
    let id = clip.id;
    let short: String = text.chars().take(24).collect();
    let what = format!(
        "title \"{short}\" at {:.2}-{:.2} s on {}{}",
        secs(tl_in),
        secs(tl_out),
        match existing {
            Some(ti) => format!("track {ti}"),
            None if seq.tracks.is_empty() => "a new text track 0".to_string(),
            None => "a new text track 0; every other track's index moves down by one".to_string(),
        },
        moved.map(|z| format!(", moved to the {z} to keep clear of a face")).unwrap_or_default(),
    );
    let edit = EditCommand::AddTitleClip { clip, track_index: existing.unwrap_or(0), created_track: existing.is_none() };
    Ok((what, edit, vec![id]))
}

fn lower_third(
    seq: &Sequence,
    fps: Fps,
    i: usize,
    name: &str,
    role: Option<&str>,
    at: Timecode,
    hold_secs: f64,
    track: Option<&TrackRef>,
    place: LowerThirdPlace,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let name = name.trim();
    let role = role.map(str::trim).filter(|r| !r.is_empty());
    if name.is_empty() {
        return Err(refuse(i, "a lower third needs a name", "names come from the brief or the cast list"));
    }
    // With a cast list, the name is one of the cast's, spelt as the list
    // spells it, and the role is the list's unless another is given.
    let cast = seq.brief.as_ref().map(|b| b.cast.as_slice()).unwrap_or(&[]);
    let member = seq.brief.as_ref().and_then(|b| b.cast_member(name));
    if !cast.is_empty() && member.is_none() {
        let names: Vec<&str> = cast.iter().map(|c| c.name.as_str()).collect();
        return Err(refuse(i, format!("\"{name}\" is not in the brief's cast"), format!("the cast is: {}; use a name as it is spelt there, or add the person to the brief", names.join(", "))));
    }
    let name = member.map(|m| m.name.trim()).unwrap_or(name);
    let role = role.or_else(|| member.map(|m| m.role.trim()).filter(|r| !r.is_empty()));
    if !hold_secs.is_finite() || hold_secs < 1.5 {
        return Err(refuse(i, format!("{hold_secs} s is too short to read a name"), "hold it about four seconds, never under two"));
    }
    let tl_in = at;
    let tl_out = tl_in + snap(hold_secs, fps);
    if tl_out > seq.total_duration() {
        return Err(refuse(i, format!("it would run to {:.2} s, past the end of the timeline at {:.2} s", secs(tl_out), secs(seq.total_duration())), "start it earlier or hold it shorter"));
    }
    // Never across a shot change: a clip edge on a video track while it is up.
    let cut = seq
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video && t.visible)
        .flat_map(|t| t.clips.iter())
        .flat_map(|c| [c.timeline_in, c.timeline_out])
        .filter(|e| *e > tl_in && *e < tl_out)
        .min();
    if let Some(cut) = cut {
        return Err(refuse(i, format!("the picture changes at {:.2} s while it is up ({:.2}-{:.2} s)", secs(cut), secs(tl_in), secs(tl_out)), "a lower third does not run across a shot change: start it after the cut, or end it before"));
    }
    // The track: the one named, else a text track with room, else a new one on top.
    let fits = |ti: usize| overlapping(seq, ti, tl_in, tl_out, None).is_none() && !seq.tracks[ti].locked;
    let existing = match track {
        Some(t) => {
            let ti = resolve_track(seq, t, i)?;
            if !matches!(seq.tracks[ti].kind, TrackKind::Text | TrackKind::Video) {
                return Err(refuse(i, format!("track {ti} is not a text or video track"), "name a text track, or leave the track out"));
            }
            if !fits(ti) {
                return Err(refuse(i, format!("track {ti} is locked or has a clip there"), "leave the track out and one with room is found or made"));
            }
            Some(ti)
        }
        None => (0..seq.tracks.len()).find(|ti| seq.tracks[*ti].kind == TrackKind::Text && fits(*ti)),
    };
    let clip = title_clip(lower_third_title(name, role, place), tl_in, tl_out, fps);
    let id = clip.id;
    let what = format!(
        "lower third \"{name}\"{} at {:.2}-{:.2} s on {}",
        role.map(|r| format!(", \"{r}\"")).unwrap_or_default(),
        secs(tl_in),
        secs(tl_out),
        match existing {
            Some(ti) => format!("track {ti}"),
            None if seq.tracks.is_empty() => "a new text track 0".to_string(),
            None => "a new text track 0; every other track's index moves down by one".to_string(),
        },
    );
    let edit = EditCommand::AddTitleClip { clip, track_index: existing.unwrap_or(0), created_track: existing.is_none() };
    Ok((what, edit, vec![id]))
}

/// A J-cut (`j`) on the cut into `clip` or an L-cut out of it, `frames`
/// long: the two clips' linked audio, adjacent on one audio track, get
/// their shared edge moved — earlier for a J-cut, later for an L-cut —
/// as two trims recorded in one batch.
fn split_edit(seq: &Sequence, pool: &MediaPool, fps: Fps, i: usize, clip_id: ClipId, frames: i64, j: bool) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    if frames <= 0 {
        return Err(refuse(i, "the lead has to be at least one frame", "give frames above zero"));
    }
    let (ti, ci, clip) = find_clip(seq, clip_id).ok_or_else(|| refuse(i, format!("no clip {}", clip_id.0), "clip ids are in the timeline view"))?;
    // The picture's neighbour: before it for a J-cut, after it for an L-cut.
    let track = &seq.tracks[ti];
    let neighbour = if j { ci.checked_sub(1).map(|p| &track.clips[p]) } else { track.clips.get(ci + 1) };
    // Compared as times, not as fields: a clip placed before the sequence
    // changed rate carries the old rate in its timecodes.
    let same = |a: Timecode, b: Timecode| a.cmp(&b) == std::cmp::Ordering::Equal;
    let Some(neighbour) = neighbour.filter(|n| if j { same(n.timeline_out, clip.timeline_in) } else { same(n.timeline_in, clip.timeline_out) }) else {
        return Err(refuse(i, format!("{} has no clip cut {} it on its track", clip_name(seq, pool, clip), if j { "before" } else { "after" }), "a J-cut or L-cut needs two clips meeting at a cut"));
    };
    let (outgoing, incoming) = if j { (neighbour, clip) } else { (clip, neighbour) };
    let audio_of = |c: &Clip| -> Result<(usize, Clip), Refusal> {
        let aid = c.linked_clip_id.ok_or_else(|| refuse(i, format!("{} has no linked audio", clip_name(seq, pool, c)), "attach its audio first (attach_audio)"))?;
        let (ati, _, a) = find_clip(seq, aid).ok_or_else(|| refuse(i, format!("the audio linked to {} is not on the timeline", clip_name(seq, pool, c)), "attach its audio again"))?;
        Ok((ati, a.clone()))
    };
    let (out_ti, out_a) = audio_of(outgoing)?;
    let (in_ti, in_a) = audio_of(incoming)?;
    if out_ti != in_ti {
        return Err(refuse(i, "the two clips' audio is on different tracks", "put both audio clips on one track"));
    }
    if seq.tracks[out_ti].locked {
        return Err(refuse(i, format!("audio track {out_ti} is locked"), "unlock it first"));
    }
    if !same(out_a.timeline_out, in_a.timeline_in) {
        return Err(refuse(i, format!("the audio clips do not meet: {:.2} s and {:.2} s", secs(out_a.timeline_out), secs(in_a.timeline_in)), "line the audio up with the picture first"));
    }
    let lead = frame_tc(frames, fps);
    let (new_edge, out_src_out, in_src_in) = if j {
        (out_a.timeline_out - lead, out_a.source_out - lead, in_a.source_in - lead)
    } else {
        (out_a.timeline_out + lead, out_a.source_out + lead, in_a.source_in + lead)
    };
    if new_edge <= out_a.timeline_in + one_frame(fps) {
        return Err(refuse(i, "the outgoing audio would be shorter than a frame", "use fewer frames"));
    }
    if new_edge >= in_a.timeline_out - one_frame(fps) {
        return Err(refuse(i, "the incoming audio would be shorter than a frame", "use fewer frames"));
    }
    if in_src_in.value < 0 {
        return Err(refuse(i, format!("only {:.2} s of sound lies before the incoming clip's in point", secs(in_a.source_in)), "use fewer frames"));
    }
    if let Some(d) = outgoing.media_id().and_then(|m| pool.get(&m)).map(|m| m.duration) {
        if out_src_out > d {
            return Err(refuse(i, format!("only {:.2} s of sound lies after the outgoing clip's out point", secs(d - out_a.source_out)), "use fewer frames"));
        }
    }
    let trim = |a: &Clip, src_in: Timecode, src_out: Timecode, tl_in: Timecode, tl_out: Timecode| EditCommand::LegacyTrimClip {
        clip_id: a.id,
        old_source_in: a.source_in,
        old_source_out: a.source_out,
        old_timeline_in: a.timeline_in,
        old_timeline_out: a.timeline_out,
        new_source_in: src_in,
        new_source_out: src_out,
        new_timeline_in: tl_in,
        new_timeline_out: tl_out,
    };
    let edits = vec![
        trim(&out_a, out_a.source_in, out_src_out, out_a.timeline_in, new_edge),
        trim(&in_a, in_src_in, in_a.source_out, new_edge, in_a.timeline_out),
    ];
    let what = format!(
        "{} of {} frames at the cut between {} and {}: the audio now changes at {:.2} s, the picture at {:.2} s",
        if j { "J-cut" } else { "L-cut" },
        frames,
        clip_name(seq, pool, outgoing),
        clip_name(seq, pool, incoming),
        secs(new_edge),
        secs(incoming.timeline_in)
    );
    Ok((what, EditCommand::Batch(edits), vec![out_a.id, in_a.id]))
}

#[cfg(test)]
mod tests {
    /// A secondary is a keyer gating a correction, never a keyer alone
    /// in the picture chain: that would put its matte on screen. The
    /// hue turn lands on the colour balance the qualifier gates.
    #[test]
    fn a_secondary_keys_a_correction_and_leaves_the_picture_in_the_chain() {
        use klipp_nodes::NodeKind;
        let (seq, pool, _, a, _) = fixture();
        let op = |hue_shift: Option<f32>, saturation: Option<f32>| Op::Qualify {
            clip_id: ClipRef::Id(a), target: Some("skin".into()), hue: None, hue_width: None,
            sat_low: None, sat_high: None, lum_low: None, lum_high: None, invert: false,
            exposure: None, saturation, hue_shift, temperature: None, remove: false, window: None, window_feather: None };
        let plan = resolve(&seq, &pool, &[op(Some(20.0), None)]).unwrap();
        let EditCommand::SetNodeGraph { new_graph, .. } = &plan.steps[0].edit else { panic!("a graph edit") };
        let qual = new_graph.nodes.iter().find(|n| n.kind == NodeKind::Qualifier).expect("a keyer");
        assert!(qual.params.qualifier.as_ref().is_some_and(|q| q.enabled && q.hue_center > 0.0), "{:?}", qual.params.qualifier);
        // The keyer feeds a correction's matte port, not the picture.
        let edge = new_graph.edges.iter().find(|e| e.from_node == qual.id).expect("the keyer is wired");
        assert_eq!(edge.to_port, 1, "a keyer feeds a matte port");
        let corr = new_graph.nodes.iter().find(|n| n.id == edge.to_node).expect("the correction");
        assert_eq!(corr.kind, NodeKind::ColorBalance);
        assert_eq!(corr.params.hue, Some(20.0), "the hue turn is on the correction");
        // And nothing in the picture chain is the keyer.
        assert_ne!(new_graph.input_of(new_graph.output_node_id, 0), Some(qual.id), "the matte is not the picture");
        // Saturation alone goes to a corrector instead.
        let plan = resolve(&seq, &pool, &[op(None, Some(0.8))]).unwrap();
        let EditCommand::SetNodeGraph { new_graph, .. } = &plan.steps[0].edit else { panic!() };
        let qual = new_graph.nodes.iter().find(|n| n.kind == NodeKind::Qualifier).unwrap();
        let edge = new_graph.edges.iter().find(|e| e.from_node == qual.id).unwrap();
        let corr = new_graph.nodes.iter().find(|n| n.id == edge.to_node).unwrap();
        assert_eq!(corr.kind, NodeKind::Corrector);
        assert_eq!(corr.params.saturation, Some(0.8));
        // Asking one secondary for both is refused with what to do.
        let err = resolve(&seq, &pool, &[op(Some(20.0), Some(0.8))]).unwrap_err();
        assert!(format!("{err:?}").contains("either turns the hue"), "{err:?}");
    }

    /// A title placed over a face moves to the nearest clear zone and
    /// says so; one placed clear stays; a card is never moved.
    #[test]
    fn a_title_keeps_clear_of_a_face() {
        struct Faces(MediaId);
        impl Transcripts for Faces {
            fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
                None
            }
            fn faces(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
                if item.id == self.0 { vec![(1.0, [0.4, 0.05, 0.2, 0.3]), (2.0, [0.4, 0.05, 0.2, 0.3])] } else { vec![] }
            }
        }
        let (seq, pool, media, _, _) = fixture();
        let op = |y: f32, full: Option<&str>| Op::Title { text: "Hello".into(), at: Anchor::Secs(1.0), style: None, secs: 2.0, track: None, x: Some(0.5), y: Some(y), size: Some(72.0), color: None, font: None, bold: None, align: None, backing: None, backing_color: None, full_frame: full.map(str::to_string), outline: None, outline_color: None, glow: None, glow_color: None, gradient: None, animate: None, animate_secs: None, stagger: None, loop_effect: None, glitch: None, chromatic: None };
        let plan = resolve_with(&seq, &pool, &[op(0.12, None)], &Faces(media)).unwrap();
        assert!(plan.steps[0].what.contains("keep clear of a face"), "{}", plan.steps[0].what);
        let EditCommand::AddTitleClip { clip, .. } = &plan.steps[0].edit else { panic!() };
        let ClipKind::Title(td) = &clip.kind else { panic!() };
        assert!(td.objects[0].transform.center.y > 0.4 || (td.objects[0].transform.center.x - 0.5).abs() > 0.2, "moved off the face: {:?}", td.objects[0].transform.center);
        let plan = resolve_with(&seq, &pool, &[op(0.85, None)], &Faces(media)).unwrap();
        assert!(!plan.steps[0].what.contains("face"), "{}", plan.steps[0].what);
        let plan = resolve_with(&seq, &pool, &[op(0.12, Some("#101418"))], &Faces(media)).unwrap();
        assert!(!plan.steps[0].what.contains("face"), "a card is not moved: {}", plan.steps[0].what);
        assert_eq!(clear_zone((0.5, 0.5), (0.2, 0.1), &[]).unwrap().1, None);
    }


    /// Where the transform puts a picture point on screen, as the renderer
    /// does (`cpu.rs`: scale about the middle, then offset; the camera
    /// test in `render_rules` pins that a positive offset moves right).
    fn on_screen(scale: f32, (px, py): (f32, f32), (x, y): (f32, f32)) -> (f32, f32) {
        let (w, h) = (1920.0f32, 1080.0f32);
        (w / 2.0 + scale * (x * w - w / 2.0) + px, h / 2.0 + scale * (y * h - h / 2.0) + py)
    }

    #[test]
    fn a_pan_puts_the_subject_in_the_middle_and_a_push_stops_at_the_edge() {
        let (seq, pool, _, a, _) = fixture();

        // A subject right of and above the middle, pushed in on at 2x,
        // lands in the middle of the frame.
        let keys = vec![PanKey { secs: 0.0, scale: 1.0, x: 0.5, y: 0.5 }, PanKey { secs: 2.0, scale: 2.0, x: 0.6, y: 0.4 }];
        let plan = resolve(&seq, &pool, &[Op::Pan { clip_id: ClipRef::Id(a), keys }]).unwrap();
        let EditCommand::EditKeyframes { new_state, .. } = &plan.steps[0].edit else { panic!("keyframes") };
        let p = (new_state.transform_pos_x_keyframes[1].value, new_state.transform_pos_y_keyframes[1].value);
        let (sx, sy) = on_screen(2.0, p, (0.6, 0.4));
        assert!((sx - 960.0).abs() < 0.5 && (sy - 540.0).abs() < 0.5, "the subject is in the middle: {sx}, {sy}");

        // A reframe pushed all the way right brings the picture's right
        // edge to the frame's and no further.
        let plan = resolve(&seq, &pool, &[Op::Reframe { clip_id: ClipRef::Id(a), scale: 1.5, x: 1.0, y: 0.0 }]).unwrap();
        let EditCommand::SetBuiltinEffects { new_effects, .. } = &plan.steps[0].edit else { panic!("effects edit") };
        let Some(klipp_effects::BuiltinEffect::Transform { position_x, position_y, .. }) = new_effects.first() else { panic!("a transform") };
        let (right_edge, _) = on_screen(1.5, (*position_x, *position_y), (1.0, 0.5));
        assert!((right_edge - 1920.0).abs() < 0.5, "the picture's edge meets the frame's: {right_edge}");

        // And a zoom that pushes toward a point gets there by the end,
        // starting from where it was.
        let plan = resolve(&seq, &pool, &[Op::Zoom { clip_id: ClipRef::Id(a), from: 1.0, to: 1.5, x: -1.0, y: 0.0 }]).unwrap();
        let EditCommand::EditKeyframes { new_state, .. } = &plan.steps[0].edit else { panic!("keyframes") };
        assert!(new_state.transform_pos_x_keyframes[0].value.abs() < 1e-3, "at 1x there is nowhere to be yet");
        let end = (new_state.transform_pos_x_keyframes[1].value, new_state.transform_pos_y_keyframes[1].value);
        let (left_edge, _) = on_screen(1.5, end, (0.0, 0.5));
        assert!(left_edge.abs() < 0.5, "pushed left, the picture's left edge meets the frame's: {left_edge}");
    }
    /// A pan keys scale and position from frame points, held at the
    /// picture's edge; a highlight's spot is four panels round the
    /// region on a faded title clip; a ramp's clip length follows the
    /// speed integral; a follow's keys go to each spot and back.
    #[test]
    fn pans_highlights_and_ramps_land_as_keyframes_and_shapes() {
        let (seq, pool, _, a, _) = fixture();
        let keys = vec![PanKey { secs: 0.0, scale: 1.0, x: 0.5, y: 0.5 }, PanKey { secs: 2.0, scale: 2.0, x: 1.0, y: 0.25 }];
        let plan = resolve(&seq, &pool, &[Op::Pan { clip_id: ClipRef::Id(a), keys }]).unwrap();
        let EditCommand::EditKeyframes { new_state, .. } = &plan.steps[0].edit else { panic!("keyframes") };
        assert_eq!(new_state.transform_scale_keyframes.iter().map(|k| k.value).collect::<Vec<_>>(), vec![1.0, 2.0]);
        // Centring the right edge at 2x would move the picture a whole
        // frame left; it is held where its edge meets the frame's, half a
        // frame, 960 px. A subject above the middle moves the picture down.
        assert!((new_state.transform_pos_x_keyframes[1].value + 960.0).abs() < 1e-3);
        assert!((new_state.transform_pos_y_keyframes[1].value - 540.0).abs() < 1e-3);
        assert_eq!(pan_position(1.5, 0.5, 0.5, 1920.0, 1080.0), (0.0, 0.0));
        let err = resolve(&seq, &pool, &[Op::Pan { clip_id: ClipRef::Id(a), keys: vec![PanKey { secs: 9.0, scale: 1.0, x: 0.5, y: 0.5 }, PanKey { secs: 0.0, scale: 1.0, x: 0.5, y: 0.5 }] }]).unwrap_err();
        assert!(format!("{err:?}").contains("outside the clip"));

        let plan = resolve(&seq, &pool, &[Op::Highlight { at: Anchor::Secs(1.0), secs: 2.0, x: 0.5, y: 0.5, w: 0.4, h: 0.2, kind: "spot".into(), color: None, stroke: None, track: None }]).unwrap();
        let EditCommand::AddTitleClip { clip, .. } = &plan.steps[0].edit else { panic!("a title clip") };
        let ClipKind::Title(td) = &clip.kind else { panic!() };
        assert_eq!(td.objects.len(), 4, "four dim panels round the region");
        assert_eq!(clip.opacity_keyframes.len(), 4, "faded in and out");
        let plan = resolve(&seq, &pool, &[Op::Highlight { at: Anchor::Secs(1.0), secs: 2.0, x: 0.3, y: 0.3, w: 0.2, h: 0.1, kind: "ring".into(), color: Some("#00ff00".into()), stroke: Some(4.0), track: None }]).unwrap();
        let EditCommand::AddTitleClip { clip, .. } = &plan.steps[0].edit else { panic!() };
        let ClipKind::Title(td) = &clip.kind else { panic!() };
        assert_eq!(td.objects.len(), 1);
        assert!(resolve(&seq, &pool, &[Op::Highlight { at: Anchor::Secs(1.0), secs: 2.0, x: 0.3, y: 0.3, w: 0.2, h: 0.1, kind: "arrow".into(), color: None, stroke: None, track: None }]).is_err());

        let plan = resolve(&seq, &pool, &[Op::Ramp { clip_id: ClipRef::Id(a), from: 1.0, to: 2.0, secs: None }]).unwrap();
        let EditCommand::Batch(edits) = &plan.steps[0].edit else { panic!("a batch") };
        let EditCommand::SetClipSpeed { new_timeline_out, .. } = &edits[1] else { panic!() };
        // Four seconds of source at 1x rising to 2x over its mean-speed length lands near 2.7 s.
        let len = new_timeline_out.to_secs_f64();
        assert!(len > 2.0 && len < 3.5, "{len}");

        let keys = follow_keys(&[(1.0, 0.2, 0.3), (1.5, 0.8, 0.7), (6.0, 0.5, 0.9)], 8.0, 1.6, 1.0);
        assert!((keys[0].secs, keys[0].scale) == (0.0, 1.0) && keys[1].secs < 1.0, "rest, then ease in: {keys:?}");
        assert!(keys.iter().any(|k| k.secs == 1.0 && k.x == 0.2), "to the first spot");
        assert!(keys.iter().any(|k| k.secs == 1.5 && k.x == 0.8), "straight on to the near one");
        assert!(keys.windows(2).all(|w| w[0].secs <= w[1].secs), "in order: {keys:?}");
        assert_eq!(keys.last().map(|k| k.scale), Some(1.0), "and back out at the end");
    }

    /// A title that names a brief type style takes the look's fields and
    /// keeps its own; a name the brief does not know is refused with
    /// the names it does.
    #[test]
    fn a_title_takes_the_brief_style_it_names() {
        use klipp_timeline::{Brief, TypeStyle};
        let (mut seq, pool, _, _, _) = fixture();
        seq.brief = Some(Brief {
            type_styles: vec![TypeStyle { name: "kicker".into(), size: Some(28.0), y: Some(0.12), color: Some("#ff6600".into()), animate: Some("drop".into()), ..Default::default() }],
            ..Default::default()
        });
        let op = |style: Option<&str>, size: Option<f32>| Op::Title { text: "Now".into(), at: Anchor::Secs(1.0), style: style.map(str::to_string), secs: 2.0, track: None, x: None, y: None, size, color: None, font: None, bold: None, align: None, backing: None, backing_color: None, full_frame: None, outline: None, outline_color: None, glow: None, glow_color: None, gradient: None, animate: None, animate_secs: None, stagger: None, loop_effect: None, glitch: None, chromatic: None };
        let plan = resolve(&seq, &pool, &[op(Some("kicker"), None)]).unwrap();
        let EditCommand::AddTitleClip { clip, .. } = &plan.steps[plan.steps.len() - 1].edit else { panic!("a title is a clip") };
        let ClipKind::Title(td) = &clip.kind else { panic!("a title") };
        let tb = td.primary_text().unwrap();
        assert_eq!(tb.font_size, 28.0);
        assert_eq!(tb.color, [255, 102, 0, 255]);
        assert!((td.objects[0].transform.center.y - 0.12).abs() < 1e-6);
        assert!(matches!(tb.motion.enter.map(|m| m.kind), Some(klipp_core::title_motion::MotionKind::Drop)));
        // Its own size wins over the look's.
        let plan = resolve(&seq, &pool, &[op(Some("kicker"), Some(40.0))]).unwrap();
        let EditCommand::AddTitleClip { clip, .. } = &plan.steps[plan.steps.len() - 1].edit else { panic!() };
        let ClipKind::Title(td) = &clip.kind else { panic!() };
        assert_eq!(td.primary_text().unwrap().font_size, 40.0);
        // A built-in look still answers, and an unknown one names the choices.
        assert!(resolve(&seq, &pool, &[op(Some("card"), None)]).is_ok());
        let err = resolve(&seq, &pool, &[op(Some("banner"), None)]).unwrap_err();
        assert!(format!("{err:?}").contains("kicker"), "{err:?}");
    }

    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::Track;

    fn tc(secs: f64) -> Timecode {
        RationalTime::from_secs_f64(secs, 25)
    }

    /// A 25 fps sequence with a video track holding two clips of a
    /// 10-second media item, at 0-4 s and 4-8 s, and an audio track.
    fn fixture() -> (Sequence, MediaPool, MediaId, ClipId, ClipId) {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/a.mov".into(), None);
        item.duration = tc(10.0);
        item.video_codec = "hevc".into();
        item.has_audio = true;
        let media_id = pool.add(item);
        let mut v = Track::new("V1", TrackKind::Video);
        let mut a = Clip::media(media_id, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let mut b = Clip::media(media_id, tc(4.0), tc(8.0), tc(4.0), tc(8.0));
        let (ia, ib) = (a.id, b.id);
        // Linked audio under each, meeting at the same cut.
        let mut audio = Track::new("A1", TrackKind::Audio);
        let mut aa = Clip::media(media_id, tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        let mut ab = Clip::media(media_id, tc(4.0), tc(8.0), tc(4.0), tc(8.0));
        a.linked_clip_id = Some(aa.id);
        aa.linked_clip_id = Some(ia);
        b.linked_clip_id = Some(ab.id);
        ab.linked_clip_id = Some(ib);
        audio.clips = vec![aa, ab];
        v.clips = vec![a, b];
        seq.tracks = vec![v, audio];
        seq.markers.push(klipp_timeline::Marker {
            id: klipp_core::MarkerId::new(),
            position: tc(6.0),
            label: "Intro".into(),
            color: klipp_core::MarkerColor::Blue,
            duration: None,
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
        });
        (seq, pool, media_id, ia, ib)
    }

    /// The fixture's file says a sentence in each clip and one the
    /// timeline never uses.
    struct Spoken;

    impl Transcripts for Spoken {
        fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            let say = |at: f64, text: &str| -> Vec<SpokenWord> {
                text.split(' ').enumerate().map(|(k, w)| SpokenWord { start_secs: at + k as f64 * 0.2, end_secs: at + k as f64 * 0.2 + 0.18, text: format!(" {w}") }).collect()
            };
            Some([say(0.5, "Nobody is afraid of the machine."), say(4.4, "What changed it for me was agents, and that is the long and the short of it."), say(9.0, "Never used.")].concat())
        }
    }

    #[test]
    fn a_trim_takes_the_linked_sound_with_it() {
        let (seq, pool, _, a, b) = fixture();
        // The picture's out edge a second earlier: the sound under it follows.
        let plan = resolve(&seq, &pool, &[Op::Trim { clip_id: ClipRef::Id(a), edge: Edge::Out, to: Anchor::Secs(3.0), ripple: false, linked: true }]).unwrap();
        assert!(plan.steps[0].what.contains("with it"), "{}", plan.steps[0].what);
        let after = &plan.steps[0].after;
        assert_eq!(after.len(), 2, "picture and sound: {after:?}");
        assert!(after.iter().all(|c| c.timeline_out_secs == 3.0 && c.source_out_secs == 3.0), "{after:?}");

        // Rippled, both tracks close up by the same second and stay in step.
        let plan = resolve(&seq, &pool, &[Op::Trim { clip_id: ClipRef::Id(a), edge: Edge::Out, to: Anchor::Secs(3.0), ripple: true, linked: true }]).unwrap();
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        let s2 = view.sequences[0].clone();
        for t in &s2.tracks {
            assert_eq!((t.clips[0].timeline_out, t.clips[1].timeline_in, t.clips[1].timeline_out), (tc(3.0), tc(3.0), tc(7.0)), "track {}", t.name);
        }

        // After a J-cut the sound's edge is its own: a trim moves both by the same amount.
        let ops = [
            Op::JCut { incoming_clip: ClipRef::Id(b), frames: 10 },
            Op::Trim { clip_id: ClipRef::Id(b), edge: Edge::Out, to: Anchor::Secs(7.0), ripple: false, linked: true },
        ];
        let plan = resolve(&seq, &pool, &ops).unwrap();
        let after = &plan.steps[1].after;
        assert!(after.iter().all(|c| c.timeline_out_secs == 7.0), "{after:?}");
        assert!(after.iter().any(|c| (c.timeline_in_secs - 3.6).abs() < 1e-9), "the sound keeps its lead: {after:?}");

        // Alone, when asked.
        let plan = resolve(&seq, &pool, &[Op::Trim { clip_id: ClipRef::Id(a), edge: Edge::Out, to: Anchor::Secs(3.0), ripple: false, linked: false }]).unwrap();
        assert_eq!(plan.steps[0].after.len(), 1);
    }

    #[test]
    fn a_redaction_is_a_cropped_pixelated_copy_on_the_track_above() {
        let (seq, pool, _, a, _) = fixture();
        // Clip a runs 0-4 s from 0-4 s of the file; hide a box from 1 to 3 s.
        let op = Op::Redact { clip_id: ClipRef::Id(a), from: Some(Anchor::Secs(1.0)), to: Some(Anchor::Secs(3.0)), x: 0.2, y: 0.6, w: 0.3, h: 0.05, kind: "mosaic".into(), amount: None };
        let plan = resolve(&seq, &pool, std::slice::from_ref(&op)).unwrap();
        let step = &plan.steps[0];
        assert!(step.what.contains("from 1.00 to 3.00 s") && step.what.contains("a new video track on top"), "{}", step.what);
        let EditCommand::LinkedImport { video: Some((cover, track)), created_video_track_at: Some(0), audio: None, .. } = &step.edit else { panic!("{:?}", step.edit) };
        assert_eq!(*track, 0);
        assert_eq!((secs(cover.timeline_in), secs(cover.timeline_out), secs(cover.source_in), secs(cover.source_out)), (1.0, 3.0, 1.0, 3.0));
        assert_eq!(cover.volume, 0.0);
        assert!(matches!(cover.builtin_effects.as_slice(), [klipp_effects::BuiltinEffect::Mosaic { .. }, klipp_effects::BuiltinEffect::Crop { left, top, .. }] if (*left - 0.194).abs() < 1e-3 && (*top - 0.594).abs() < 1e-3), "{:?}", cover.builtin_effects);
        assert_eq!((step.after[0].timeline_in_secs, step.after[0].timeline_out_secs), (1.0, 3.0));

        // With a video track above that has room, it goes there and no track is made.
        let mut seq2 = seq.clone();
        seq2.tracks.insert(0, Track::new("V2", TrackKind::Video));
        let plan = resolve(&seq2, &pool, std::slice::from_ref(&op)).unwrap();
        assert!(matches!(&plan.steps[0].edit, EditCommand::LinkedImport { video: Some((_, 0)), created_video_track_at: None, .. }), "{:?}", plan.steps[0].edit);
        assert!(plan.steps[0].what.contains("on track 0"));

        // Outside the clip, a bad box, an unknown kind: refused with why.
        let mk = |from: f64, to: f64, w: f32, kind: &str| Op::Redact { clip_id: ClipRef::Id(a), from: Some(Anchor::Secs(from)), to: Some(Anchor::Secs(to)), x: 0.2, y: 0.6, w, h: 0.05, kind: kind.into(), amount: None };
        assert!(resolve(&seq, &pool, &[mk(5.0, 9.0, 0.3, "mosaic")]).unwrap_err().why.contains("not a stretch inside"));
        assert!(resolve(&seq, &pool, &[mk(1.0, 3.0, 0.0, "mosaic")]).unwrap_err().why.contains("no size"));
        assert!(resolve(&seq, &pool, &[mk(1.0, 3.0, 0.3, "paint")]).unwrap_err().hint.contains("mosaic or blur"));
    }

    #[test]
    fn a_long_plan_folds_its_runs_and_says_long_names_once_shortly() {
        let (seq, pool, media, _, _) = fixture();
        let ops: Vec<Op> = (0..9)
            .map(|k| Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::TrackEnd(TrackRef::Index(0)), source_in: Some(k as f64 * 0.5), source_out: Some(k as f64 * 0.5 + 0.4), insert: false, with_audio: true })
            .collect();
        let plan = resolve(&seq, &pool, &ops).unwrap();
        let folded = plan.text_folded();
        let lines: Vec<&str> = folded.lines().collect();
        assert_eq!(lines.len(), 5, "two, the fold, the last, the length:\n{folded}");
        assert!(lines[0].starts_with("1. ") && lines[1].starts_with("2. ") && lines[2].contains("6 more like these") && lines[3].starts_with("9. "), "{folded}");
        assert_eq!(plan.text().lines().count(), 10, "in full when asked");
        assert_eq!(short_name("Omarchy Is Not Afraid Of AI Code [CLFeu9AcfKk].webm"), "Omarchy Is Not Afraid…webm".replace("…webm", "….webm"));
        assert_eq!(short_name("a.mov"), "a.mov");
    }

    #[test]
    fn an_assembly_names_its_own_clips_and_the_end_of_a_track() {
        let (seq, pool, media, _, _) = fixture();
        let plan = resolve(&seq, &pool, &[
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::TrackEnd(TrackRef::Kind("video".into())), source_in: Some(1.0), source_out: Some(2.0), insert: false, with_audio: true },
            // The clip that place made, by where it is: the next goes at its end.
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::ClipEnd(ClipRef::At { track: 0, secs: 8.5 }), source_in: Some(2.0), source_out: Some(3.0), insert: false, with_audio: true },
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::ClipStart(ClipRef::At { track: 0, secs: 9.5 }), source_in: Some(2.0), source_out: Some(3.0), insert: false, with_audio: true },
        ]);
        // The first two land end to end; the third names the second's start, where the second is.
        let err = plan.unwrap_err();
        assert_eq!(err.step, 2);
        assert!(err.why.contains("overlap"), "{err}");
        let plan = resolve(&seq, &pool, &[
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::TrackEnd(TrackRef::Index(0)), source_in: Some(1.0), source_out: Some(2.0), insert: false, with_audio: true },
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::ClipEnd(ClipRef::At { track: 0, secs: 8.5 }), source_in: Some(2.0), source_out: Some(3.0), insert: false, with_audio: true },
        ]).unwrap();
        assert_eq!((plan.steps[0].after[0].timeline_in_secs, plan.steps[0].after[0].timeline_out_secs), (8.0, 9.0));
        assert_eq!((plan.steps[1].after[0].timeline_in_secs, plan.steps[1].after[0].timeline_out_secs), (9.0, 10.0));
    }

    #[test]
    fn captions_follow_the_sound_and_break_at_cuts_and_sentences() {
        let (seq, pool, _, _, _) = fixture();
        // Without transcripts there is nothing to caption, and it says what would help.
        let err = resolve(&seq, &pool, &[Op::Captions { track: None, from: None, to: None, max_chars: 42, max_lines: 2, replace: false }]).unwrap_err();
        assert!(err.hint.contains("analyze"), "{err}");

        let plan = resolve_with(&seq, &pool, &[Op::Captions { track: None, from: None, to: None, max_chars: 30, max_lines: 2, replace: false }], &Spoken).unwrap();
        assert!(plan.steps[0].what.starts_with("captions: 3 cues"), "{}", plan.steps[0].what);
        assert_eq!(plan.duration_secs, 8.0, "captions do not lengthen the piece");
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        let after = view.sequences[0].clone();
        let track = after.tracks.last().unwrap();
        assert_eq!(track.kind, TrackKind::Subtitle, "a subtitle track at the bottom");
        let texts: Vec<String> = track.clips.iter().map(|c| c.title_data().unwrap().text()).collect();
        // Thirty-two characters on lines of thirty: two even lines.
        assert_eq!(texts[0], "Nobody is afraid of\nthe machine.");
        // The second sentence is over sixty characters: two cues, each on two lines at most.
        assert!(texts[1].starts_with("What changed it for me") && texts[1].contains('\n'), "{texts:?}");
        assert!(texts.iter().all(|t| t.lines().count() <= 2 && !t.contains("Never")), "{texts:?}");
        // On the timeline where the clip puts the words, in order, never overlapping.
        assert!((secs(track.clips[0].timeline_in) - 0.52).abs() < 0.021, "{}", secs(track.clips[0].timeline_in));
        assert!((secs(track.clips[1].timeline_in) - 4.4).abs() < 0.021);
        assert!(track.clips.windows(2).all(|w| w[0].timeline_out <= w[1].timeline_in));
        // Undone, the track goes with them.
        view.apply_inverse(&plan.steps[0].edit);
        assert_eq!(view.sequences[0].tracks.len(), 2);

        // A second time onto the same track is refused where cues are already.
        let twice = [Op::Captions { track: None, from: None, to: None, max_chars: 30, max_lines: 2, replace: false }, Op::Captions { track: None, from: None, to: None, max_chars: 30, max_lines: 2, replace: false }];
        let err = resolve_with(&seq, &pool, &twice, &Spoken).unwrap_err();
        assert_eq!(err.step, 1);
        assert!(err.why.contains("overlap") && err.hint.contains("replace true"), "{err}");
        // Made again in place of the first ones, when asked: the same three, not six.
        let again = [twice[0].clone(), Op::Captions { track: None, from: None, to: None, max_chars: 30, max_lines: 2, replace: true }];
        let plan = resolve_with(&seq, &pool, &again, &Spoken).unwrap();
        assert!(plan.steps[1].what.contains("in place of the 3 that were there"), "{}", plan.steps[1].what);
        // And every cue is there to be read.
        assert!(plan.steps[1].what.contains("Nobody is afraid of / the machine."), "{}", plan.steps[1].what);
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for st in &plan.steps {
            view.apply_forward(&st.edit);
        }
        assert_eq!(view.sequences[0].tracks.last().unwrap().clips.len(), 3);
    }

    #[test]
    fn sound_settings_land_where_they_are_read_and_a_duck_follows_the_speech() {
        let (mut seq, pool, media, a, _) = fixture();
        // A music bed under the whole piece, on an audio track of its own.
        let mut music_track = Track::new("A2", TrackKind::Audio);
        let music = Clip::media(media, tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        let music_id = music.id;
        music_track.clips = vec![music];
        seq.tracks.push(music_track);

        // Named by its audio clip, the speech's settings go on the video
        // clip it is linked to: that is where playback and the render read them.
        let plan = resolve(&seq, &pool, &[Op::Audio { clip_id: ClipRef::At { track: 1, secs: 1.0 }, volume_db: Some(-6.0), gain_db: None, pan: Some(-0.5), fade_in: Some(0.5), fade_out: None }]).unwrap();
        let EditCommand::EditClipAudio { clip_id, new_volume, new_pan, new_fade_in, new_fade_out, new_gain, .. } = &plan.steps[0].edit else { panic!("an audio edit") };
        assert_eq!(*clip_id, a, "{}", plan.steps[0].what);
        assert!((new_volume - 0.5012).abs() < 1e-3 && *new_pan == -0.5 && *new_fade_in == 0.5 && *new_fade_out == 0.0 && *new_gain == 1.0);
        assert!(plan.steps[0].what.contains("linked"), "{}", plan.steps[0].what);
        // Fades that do not fit, a level out of range, nothing to change: refused.
        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        assert!(bad(Op::Audio { clip_id: ClipRef::Id(a), volume_db: None, gain_db: None, pan: None, fade_in: Some(3.0), fade_out: Some(2.0) }).why.contains("do not fit"));
        assert!(bad(Op::Audio { clip_id: ClipRef::Id(a), volume_db: Some(40.0), gain_db: None, pan: None, fade_in: None, fade_out: None }).why.contains("out of range"));
        assert!(bad(Op::Audio { clip_id: ClipRef::Id(a), volume_db: None, gain_db: None, pan: None, fade_in: None, fade_out: None }).why.contains("nothing"));

        // The duck: speech runs 0 to 8 s in two clips that meet, one stretch;
        // the music comes back up over half a second after it.
        let plan = resolve(&seq, &pool, &[
            Op::Audio { clip_id: ClipRef::Id(music_id), volume_db: Some(-6.0), gain_db: None, pan: None, fade_in: None, fade_out: Some(1.0) },
            Op::Duck { clip_id: ClipRef::Id(music_id), by_db: 12.0, fade_secs: 0.5 },
        ]).unwrap();
        assert!(plan.steps[1].what.contains("1 stretch") && plan.steps[1].what.contains("0.00-8.00"), "{}", plan.steps[1].what);
        let EditCommand::EditKeyframes { clip_id, new_state, .. } = &plan.steps[1].edit else { panic!("keyframes") };
        assert_eq!(*clip_id, music_id);
        let at = |t: f64| klipp_core::keyframe::evaluate(&new_state.volume_keyframes, t, 1.0);
        let (base, low) = (0.5012f32, 0.5012 * 0.2512);
        assert!((at(4.0) - low).abs() < 1e-3, "under the speech: {}", at(4.0));
        assert!((at(9.5) - base).abs() < 1e-3, "after it: {}", at(9.5));
        assert!(at(8.25) > low && at(8.25) < base, "on the way up: {}", at(8.25));
        // A duck of nothing, a step, a clip that is not heard: refused.
        let alone = { let mut s2 = seq.clone(); s2.tracks = vec![seq.tracks[2].clone()]; s2 };
        assert!(resolve(&alone, &pool, &[Op::Duck { clip_id: ClipRef::Id(music_id), by_db: 9.0, fade_secs: 0.5 }]).unwrap_err().why.contains("nothing is heard"));
        assert!(bad(Op::Duck { clip_id: ClipRef::Id(music_id), by_db: 9.0, fade_secs: 0.0 }).hint.contains("never a step"));
        // As the tools send them.
        let _: Vec<Op> = serde_json::from_str(r#"[{"op":"audio","clip_id":{"track":2,"secs":1.0},"volume_db":-8,"fade_in":2},{"op":"duck","clip_id":{"track":2,"secs":1.0}}]"#).unwrap();
    }

    #[test]
    fn a_lower_third_goes_where_the_speaker_is() {
        let (seq, pool, _, _, _) = fixture();
        let ops: Vec<Op> = serde_json::from_str(r#"[{"op":"lower_third","name":"DHH","role":"Creator of Omarchy","at":{"secs":4.4},"secs":3,"side":"right","x":0.9,"y":0.7}]"#).unwrap();
        let plan = resolve(&seq, &pool, &ops).unwrap();
        let EditCommand::AddTitleClip { clip, .. } = &plan.steps[0].edit else { panic!("a title clip") };
        let td = clip.title_data().unwrap();
        let (name, role) = (&td.objects[0].transform, &td.objects[1].transform);
        assert_eq!((name.center.x, name.center.y, name.anchor_norm.x), (0.9, 0.7, 1.0));
        assert!((role.center.y - 0.753).abs() < 1e-4 && role.center.x == 0.9);
        // Left alone it is where it always was.
        let plain: Vec<Op> = serde_json::from_str(r#"[{"op":"lower_third","name":"DHH","role":"x","at":{"secs":4.4},"secs":3}]"#).unwrap();
        let plan = resolve(&seq, &pool, &plain).unwrap();
        let EditCommand::AddTitleClip { clip, .. } = &plan.steps[0].edit else { panic!("a title clip") };
        let t = &clip.title_data().unwrap().objects[0].transform;
        assert_eq!((t.center.x, t.center.y, t.anchor_norm.x), (0.08, 0.815, 0.0));
        // Outside title-safe, or a side that is not one: refused.
        for bad in [r#""x":0.99"#, r#""y":0.93"#, r#""side":"middle""#] {
            let ops: Vec<Op> = serde_json::from_str(&format!(r#"[{{"op":"lower_third","name":"DHH","role":"x","at":{{"secs":4.4}},"secs":3,{bad}}}]"#)).unwrap();
            assert!(resolve(&seq, &pool, &ops).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_title_starts_earlier_when_its_head_is_trimmed_back() {
        let (seq, pool, _, _, _) = fixture();
        let ops: Vec<Op> = serde_json::from_str(r#"[
            {"op":"lower_third","name":"Brodie","at":{"secs":1.0},"secs":2},
            {"op":"trim","clip_id":{"track":0,"secs":1.5},"edge":"in","to":{"secs":0.6}}
        ]"#).unwrap();
        let plan = resolve(&seq, &pool, &ops).unwrap();
        let after = &plan.steps[1].after[0];
        assert_eq!((after.timeline_in_secs, after.timeline_out_secs, after.source_in_secs, after.source_out_secs), (0.6, 3.0, 0.0, 2.4));
    }

    #[test]
    fn a_caption_is_put_right() {
        let (seq, pool, _, _, _) = fixture();
        let ops: Vec<Op> = serde_json::from_str(r#"[
            {"op":"lower_third","name":"Brodie","at":{"secs":0.5},"secs":3},
            {"op":"set_text","clip_id":{"track":0,"secs":1.0},"text":"Brodie Robertson"}
        ]"#).unwrap();
        let plan = resolve(&seq, &pool, &ops).unwrap();
        assert!(plan.steps[1].what.contains("\"Brodie Robertson\" (was \"Brodie\")"), "{}", plan.steps[1].what);
        let EditCommand::EditTitle { old_data, new_data, .. } = &plan.steps[1].edit else { panic!("a title edit") };
        assert_eq!((old_data.text(), new_data.text()), ("Brodie".to_string(), "Brodie Robertson".to_string()));
        assert_eq!(old_data.objects[0].transform, new_data.objects[0].transform, "only the words change");
        // A picture has no words to set, and a title is not emptied this way.
        let err = resolve(&seq, &pool, &[ops[0].clone(), Op::SetText { clip_id: ClipRef::At { track: 1, secs: 1.0 }, text: "x".into() }]).unwrap_err();
        assert!(err.why.contains("not a title"), "{err}");
        assert!(resolve(&seq, &pool, &[ops[0].clone(), Op::SetText { clip_id: ClipRef::At { track: 0, secs: 1.0 }, text: " ".into() }]).is_err());
    }

    #[test]
    fn a_transition_goes_between_two_clips_and_undo_removes_it() {
        let (seq, pool, _, a, b) = fixture();
        // The two clips meet at 4 s and each is 4 s long with source beyond.
        let plan = resolve(&seq, &pool, &[Op::Transition { clip_id: ClipRef::Id(a), kind: "dissolve".into(), secs: 0.4 }]).unwrap();
        assert!(plan.steps[0].what.starts_with("Cross Dissolve of 0.40 s") && plan.steps[0].what.contains("at 4.00 s"), "{}", plan.steps[0].what);
        let EditCommand::AddTransition { transition, .. } = &plan.steps[0].edit else { panic!("a transition edit") };
        assert_eq!(transition.clip_a_id, a);
        assert_eq!(transition.clip_b_id, b);
        assert_eq!(transition.kind, klipp_core::TransitionKind::CrossDissolve);

        // Replay and undo: the transition and the crossfades come and go.
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        assert_eq!(view.sequences[0].tracks[0].transitions.len(), 1);
        assert!((view.sequences[0].tracks[0].clips[0].fade_out_secs - 0.4).abs() < 1e-6);
        view.apply_inverse(&plan.steps[0].edit);
        assert_eq!(view.sequences[0].tracks[0].transitions.len(), 0);
        assert_eq!(view.sequences[0].tracks[0].clips[0].fade_out_secs, 0.0);

        // A stylised name by its alias.
        let plan = resolve(&seq, &pool, &[Op::Transition { clip_id: ClipRef::Id(a), kind: "whip".into(), secs: 0.4 }]).unwrap();
        let EditCommand::AddTransition { transition, .. } = &plan.steps[0].edit else { panic!() };
        assert_eq!(transition.kind, klipp_core::TransitionKind::CrossZoom);

        // Refusals: the last clip has no next; an unknown kind; too long.
        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        assert!(bad(Op::Transition { clip_id: ClipRef::Id(b), kind: "dissolve".into(), secs: 0.5 }).why.contains("no clip after"));
        assert!(bad(Op::Transition { clip_id: ClipRef::Id(a), kind: "sparkle".into(), secs: 0.5 }).why.contains("not a transition"));
        assert!(bad(Op::Transition { clip_id: ClipRef::Id(a), kind: "dissolve".into(), secs: 4.0 }).why.contains("as long as one of its clips"));

        // As the tools send it, kind and secs optional.
        let ops: Vec<Op> = serde_json::from_str(r#"[{"op":"transition","clip_id":{"track":0,"secs":2.0}}]"#).unwrap();
        assert!(matches!(&ops[0], Op::Transition { kind, secs, .. } if kind == "cross_dissolve" && (*secs - 0.5).abs() < 1e-9));
    }

    #[test]
    fn a_crop_goes_first_in_the_chain_and_comes_off_again() {
        let (seq, pool, _media, a, _) = fixture();
        use klipp_effects::BuiltinEffect as E;
        // A window in a desktop recording: 320 px in from the left of 3840.
        let op = Op::Crop { clip_id: ClipRef::Id(a), left: 320.0 / 3840.0, right: 320.0 / 3840.0, top: 106.0 / 2160.0, bottom: 54.0 / 2160.0 };
        let plan = resolve(&seq, &pool, &[op]).unwrap();
        let EditCommand::SetBuiltinEffects { new_effects, .. } = &plan.steps[0].edit else { panic!("effects edit") };
        assert!(matches!(new_effects.first(), Some(E::Crop { left, .. }) if (*left - 0.0833).abs() < 1e-3));

        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        let cropped = view.sequences[0].clone();
        let off = resolve(&cropped, &pool, &[Op::Crop { clip_id: ClipRef::Id(a), left: 0.0, right: 0.0, top: 0.0, bottom: 0.0 }]).unwrap();
        let EditCommand::SetBuiltinEffects { new_effects, .. } = &off.steps[0].edit else { panic!("effects edit") };
        assert!(new_effects.is_empty(), "four zeros take it off");

        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        assert!(bad(Op::Crop { clip_id: ClipRef::Id(a), left: 0.6, right: 0.5, top: 0.0, bottom: 0.0 }).why.contains("leaves nothing"));
        assert!(bad(Op::Crop { clip_id: ClipRef::Id(a), left: 0.0, right: 0.0, top: 0.0, bottom: 0.0 }).why.contains("no crop"));
    }

    #[test]
    fn a_camera_moves_over_the_whole_sequence_and_can_be_taken_off_again() {
        let (seq, pool, _media, _a, _) = fixture();

        // A push in from the middle to the left of the scene.
        let keys = vec![
            CameraKey { secs: 0.0, ..Default::default() },
            CameraKey { secs: 4.0, x: 0.3, zoom: 1.4, roll_deg: -3.0, ..Default::default() },
        ];
        let plan = resolve(&seq, &pool, &[Op::Camera { keys }]).unwrap();
        assert!(plan.steps[0].what.contains("1.40x at 0.30,0.50"), "{}", plan.steps[0].what);
        let EditCommand::SetSequenceCamera { new, old } = &plan.steps[0].edit else { panic!("a camera edit") };
        assert!(old.is_still(), "there was no camera before");
        assert_eq!(new.x_keyframes.len(), 2);
        assert!((new.zoom_keyframes[1].value - 1.4).abs() < 1e-6);
        assert!((new.roll_keyframes[1].value + 3.0).abs() < 1e-6);

        // It applies and undoes as one step.
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        assert!(!view.sequences[0].camera.is_still());
        view.apply_inverse(&plan.steps[0].edit);
        assert!(view.sequences[0].camera.is_still());

        // No keys takes it off again — and says so rather than doing
        // nothing when there was none.
        let mut moved = seq.clone();
        moved.camera.zoom_keyframes = vec![klipp_core::Keyframe { time_secs: 0.0, value: 1.2, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) }];
        let plan = resolve(&moved, &pool, &[Op::Camera { keys: vec![] }]).unwrap();
        let EditCommand::SetSequenceCamera { new, .. } = &plan.steps[0].edit else { panic!("a camera edit") };
        assert!(new.is_still());
        assert!(resolve(&seq, &pool, &[Op::Camera { keys: vec![] }]).unwrap_err().why.contains("no camera"));

        // Refusals: a key past the end of the timeline, an absurd zoom.
        let bad = |k: CameraKey| resolve(&seq, &pool, &[Op::Camera { keys: vec![k] }]).unwrap_err();
        assert!(bad(CameraKey { secs: 99.0, ..Default::default() }).why.contains("off the timeline"));
        assert!(bad(CameraKey { zoom: 99.0, ..Default::default() }).why.contains("zoom"));

        // As the tools send it: a key gives what it changes.
        let ops: Vec<Op> = serde_json::from_str(r#"[{"op":"camera","keys":[{"secs":1.0,"zoom":1.5}]}]"#).unwrap();
        let Op::Camera { keys } = &ops[0] else { panic!("a camera") };
        assert_eq!(keys[0], CameraKey { secs: 1.0, zoom: 1.5, ..Default::default() });
    }

    #[test]
    fn a_compose_places_a_picture_smaller_than_the_frame_and_animates_between_poses() {
        let (seq, pool, _media, a, _) = fixture();

        // One pose: a half-size picture left of centre and low in a
        // 1920x1080 frame, turned, half seen. Position is pixels from the
        // middle of the frame, which is where the anchor sits.
        let pose = ComposePose { x: 0.25, y: 0.75, width: 0.5, height: 0.5, rotation_deg: 15.0, opacity: 0.5, ..Default::default() };
        let plan = resolve(&seq, &pool, &[Op::Compose { clip_id: ClipRef::Id(a), poses: vec![pose] }]).unwrap();
        assert!(plan.steps[0].what.contains("0.50 of the frame at 0.25,0.75"), "{}", plan.steps[0].what);
        let EditCommand::EditKeyframes { new_state, .. } = &plan.steps[0].edit else { panic!("keyframe edit") };
        assert_eq!(new_state.transform_pos_x_keyframes.len(), 1);
        assert!((new_state.transform_pos_x_keyframes[0].value + 480.0).abs() < 1e-3);
        assert!((new_state.transform_pos_y_keyframes[0].value - 270.0).abs() < 1e-3);
        assert!((new_state.transform_scale_keyframes[0].value - 0.5).abs() < 1e-6);
        assert!((new_state.transform_rotation_keyframes[0].value - 15.0).abs() < 1e-6);
        assert!((new_state.opacity_keyframes[0].value - 0.5).abs() < 1e-6);

        // Two poses animate between them, in time order whichever order
        // they arrive in: this one slides in from off the right.
        let landed = ComposePose { secs: 2.0, x: 0.5, width: 0.4, height: 0.4, ..Default::default() };
        let offscreen = ComposePose { secs: 0.0, x: 1.5, width: 0.4, height: 0.4, ..Default::default() };
        let plan = resolve(&seq, &pool, &[Op::Compose { clip_id: ClipRef::Id(a), poses: vec![landed, offscreen] }]).unwrap();
        let EditCommand::EditKeyframes { new_state, .. } = &plan.steps[0].edit else { panic!("keyframe edit") };
        let times: Vec<f64> = new_state.transform_pos_x_keyframes.iter().map(|k| k.time_secs).collect();
        assert_eq!(times, vec![0.0, 2.0]);
        assert!((new_state.transform_pos_x_keyframes[0].value - 1920.0).abs() < 1e-3);
        assert!(new_state.transform_pos_x_keyframes[1].value.abs() < 1e-3);

        // Undo puts the clip's keyframes back as they were.
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        assert_eq!(view.sequences[0].tracks[0].clips[0].transform_pos_x_keyframes.len(), 2);
        view.apply_inverse(&plan.steps[0].edit);
        assert!(view.sequences[0].tracks[0].clips[0].transform_pos_x_keyframes.is_empty());

        // Refusals: no pose at all, a stretch the engine cannot make, a
        // pose after the clip has ended, a fade outside 0 to 1.
        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        let just = |pose: ComposePose| Op::Compose { clip_id: ClipRef::Id(a), poses: vec![pose] };
        assert!(bad(Op::Compose { clip_id: ClipRef::Id(a), poses: vec![] }).why.contains("needs a pose"));
        assert!(bad(just(ComposePose { width: 0.5, height: 0.25, ..Default::default() })).why.contains("differ"));
        assert!(bad(just(ComposePose { secs: 9.0, ..Default::default() })).why.contains("outside the clip"));
        assert!(bad(just(ComposePose { opacity: 2.0, ..Default::default() })).why.contains("opacity"));

        // As the tools send it: a pose gives what it changes, and what it
        // leaves out is the frame as shot.
        let ops: Vec<Op> = serde_json::from_str(r#"[{"op":"compose","clip_id":{"track":0,"secs":2.0},"poses":[{"width":0.5,"height":0.5}]}]"#).unwrap();
        let Op::Compose { poses, .. } = &ops[0] else { panic!("a compose") };
        assert_eq!(poses[0], ComposePose { width: 0.5, height: 0.5, ..Default::default() });
    }

    #[test]
    fn finishing_ops_add_effects_stabilise_retime_and_clean_audio() {
        let (seq, pool, media, a, _) = fixture();
        use klipp_effects::BuiltinEffect as E;

        // An effect appends a builtin effect; the same kind replaces it.
        let plan = resolve(&seq, &pool, &[Op::Effect { clip_id: ClipRef::Id(a), kind: "vignette".into(), amount: Some(0.6) }]).unwrap();
        assert!(plan.steps[0].what.starts_with("vignette 0.60"), "{}", plan.steps[0].what);
        let EditCommand::SetBuiltinEffects { new_effects, .. } = &plan.steps[0].edit else { panic!("effects edit") };
        assert!(matches!(new_effects.last(), Some(E::Vignette { .. })));

        // The layer treatment reaches the same op by name.
        let plan = resolve(&seq, &pool, &[Op::Effect { clip_id: ClipRef::Id(a), kind: "shadow".into(), amount: Some(0.5) }]).unwrap();
        let EditCommand::SetBuiltinEffects { new_effects, .. } = &plan.steps[0].edit else { panic!("effects edit") };
        assert!(matches!(new_effects.last(), Some(E::DropShadow { color, .. }) if *color == [0, 0, 0]));
        let plan = resolve(&seq, &pool, &[Op::Effect { clip_id: ClipRef::Id(a), kind: "corners".into(), amount: Some(0.2) }]).unwrap();
        let EditCommand::SetBuiltinEffects { new_effects, .. } = &plan.steps[0].edit else { panic!("effects edit") };
        assert!(matches!(new_effects.last(), Some(E::RoundedCorners { .. })));

        // Stabilise appends VideoStabilization; replay and undo.
        let plan = resolve(&seq, &pool, &[Op::Stabilize { clip_id: ClipRef::Id(a), smoothing: 0.5 }]).unwrap();
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        assert!(view.sequences[0].tracks[0].clips[0].builtin_effects.iter().any(|e| matches!(e, E::VideoStabilization { .. })));
        view.apply_inverse(&plan.steps[0].edit);
        assert!(view.sequences[0].tracks[0].clips[0].builtin_effects.is_empty());

        // Retime to 2x halves the clip and says nothing runs into the next
        // (clip a is 0-4, clip b starts at 4; 2x makes a 0-2, a gap opens).
        let plan = resolve(&seq, &pool, &[Op::Retime { clip_id: ClipRef::Id(a), speed: 2.0 }]).unwrap();
        assert!(plan.steps[0].what.contains("2.00x") && plan.steps[0].what.contains("0.00-2.00 s"), "{}", plan.steps[0].what);
        // Half speed makes a run into b.
        let plan = resolve(&seq, &pool, &[Op::Retime { clip_id: ClipRef::Id(a), speed: 0.5 }]).unwrap();
        assert!(plan.steps[0].what.contains("overlaps"), "{}", plan.steps[0].what);

        // Clean audio adds the DSP for each flag, on the clip's own sound.
        let plan = resolve(&seq, &pool, &[Op::CleanAudio { clip_id: ClipRef::Id(a), wind: true, hiss: false, hum: true, deess: true }]).unwrap();
        assert!(plan.steps[0].what.contains("wind and rumble") && plan.steps[0].what.contains("hum") && plan.steps[0].what.contains("sibilance"), "{}", plan.steps[0].what);
        let EditCommand::EditAudioChain { new_state, .. } = &plan.steps[0].edit else { panic!("audio edit") };
        let types: Vec<&str> = new_state.audio_effects.iter().map(|e| e.effect_type.as_str()).collect();
        assert!(types.contains(&"noise_reduction") && types.contains(&"hum_removal") && types.contains(&"deesser"), "{types:?}");

        // Refusals and the JSON shape.
        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        assert!(bad(Op::Effect { clip_id: ClipRef::Id(a), kind: "sparkle".into(), amount: None }).why.contains("not an effect"));
        assert!(bad(Op::CleanAudio { clip_id: ClipRef::Id(a), wind: false, hiss: false, hum: false, deess: false }).why.contains("nothing to clean"));
        let _: Vec<Op> = serde_json::from_str(r#"[{"op":"effect","clip_id":{"track":0,"secs":1.0},"kind":"glow"},{"op":"retime","clip_id":{"track":0,"secs":1.0},"speed":0.5},{"op":"clean_audio","clip_id":{"track":0,"secs":1.0},"wind":true}]"#).unwrap();
        let _ = media;
    }

    #[test]
    fn reframe_punches_in_with_a_transform_first_and_undoes() {
        let (seq, pool, _media, a, _) = fixture();
        use klipp_effects::BuiltinEffect as E;

        // A punch-in with a pan inserts a Transform at the front of the chain.
        let plan = resolve(&seq, &pool, &[Op::Reframe { clip_id: ClipRef::Id(a), scale: 1.5, x: 0.2, y: -0.1 }]).unwrap();
        assert!(plan.steps[0].what.contains("scale 1.50") && plan.steps[0].what.contains("pan 0.20, -0.10"), "{}", plan.steps[0].what);
        let EditCommand::SetBuiltinEffects { new_effects, .. } = &plan.steps[0].edit else { panic!("effects edit") };
        let Some(E::Transform { scale_x, position_x, position_y, .. }) = new_effects.first() else { panic!("transform first") };
        // At 1.5x the picture reaches a quarter of the 1920 frame past
        // each side, 480 px. Panning the frame a fifth of that toward the
        // right moves the picture 96 px left; a tenth up moves it 27 px down.
        assert!((scale_x - 1.5).abs() < 1e-4, "{scale_x}");
        assert!((position_x + 96.0).abs() < 1e-3, "{position_x}");
        assert!((position_y - 27.0).abs() < 1e-3, "{position_y}");

        // Re-reframing replaces the earlier Transform, never stacks a second.
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        {
            let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
            view.apply_forward(&plan.steps[0].edit);
        }
        let plan2 = resolve(&seqs[0], &p, &[Op::Reframe { clip_id: ClipRef::Id(a), scale: 2.0, x: 0.0, y: 0.0 }]).unwrap();
        let EditCommand::SetBuiltinEffects { new_effects: e2, .. } = &plan2.steps[0].edit else { panic!("effects edit") };
        assert_eq!(e2.iter().filter(|e| matches!(e, E::Transform { .. })).count(), 1, "one transform, not two");
        {
            let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
            view.apply_inverse(&plan.steps[0].edit);
        }
        assert!(seqs[0].tracks[0].clips[0].builtin_effects.is_empty(), "undo clears the reframe");

        // Out of range and a no-op reframe are refused.
        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        assert!(bad(Op::Reframe { clip_id: ClipRef::Id(a), scale: 9.0, x: 0.0, y: 0.0 }).why.contains("scale"));
        assert!(bad(Op::Reframe { clip_id: ClipRef::Id(a), scale: 1.0, x: 0.0, y: 0.0 }).why.contains("nothing to reframe"));
        // At scale 1 there is nothing past the frame to pan into.
        assert!(bad(Op::Reframe { clip_id: ClipRef::Id(a), scale: 1.0, x: 0.5, y: 0.0 }).why.contains("nowhere to go"));
        let _: Vec<Op> = serde_json::from_str(r#"[{"op":"reframe","clip_id":{"track":0,"secs":1.0},"scale":1.3}]"#).unwrap();
    }

    #[test]
    fn zoom_writes_scale_keyframes() {
        let (seq, pool, _media, a, _) = fixture();
        let plan = resolve(&seq, &pool, &[Op::Zoom { clip_id: ClipRef::Id(a), from: 1.0, to: 1.3, x: 0.0, y: 0.0 }]).unwrap();
        assert!(plan.steps[0].what.contains("1.00\u{2192}1.30"), "{}", plan.steps[0].what);
        let EditCommand::EditKeyframes { new_state, .. } = &plan.steps[0].edit else { panic!("keyframes edit") };
        assert_eq!(new_state.transform_scale_keyframes.len(), 2, "start and end keyframes");
        assert!((new_state.transform_scale_keyframes[0].value - 1.0).abs() < 1e-4);
        assert!((new_state.transform_scale_keyframes[1].value - 1.3).abs() < 1e-4);
        // Out of range and no-op are refused.
        assert!(resolve(&seq, &pool, &[Op::Zoom { clip_id: ClipRef::Id(a), from: 1.0, to: 9.0, x: 0.0, y: 0.0 }]).is_err());
        assert!(resolve(&seq, &pool, &[Op::Zoom { clip_id: ClipRef::Id(a), from: 1.0, to: 1.0, x: 0.0, y: 0.0 }]).unwrap_err().why.contains("nothing to zoom"));
        let _: Vec<Op> = serde_json::from_str(r#"[{"op":"zoom","clip_id":{"track":0,"secs":1.0},"to":1.2}]"#).unwrap();
    }

    #[test]
    fn title_builds_a_styled_animated_clip() {
        let (seq, pool, _media, _a, _) = fixture();
        let ops: Vec<Op> = serde_json::from_str(r##"[{"op":"title","text":"STRATA","at":{"secs":0.5},"secs":2.0,"size":120,"color":"#ffffff","glow":0.8,"glow_color":"#ff2a78","animate":"rise","stagger":"char","loop":"pulse","backing":0.4}]"##).unwrap();
        let plan = resolve(&seq, &pool, &ops).unwrap();
        assert!(plan.steps[0].what.contains("title \"STRATA\""), "{}", plan.steps[0].what);
        let EditCommand::AddTitleClip { clip, created_track, .. } = &plan.steps[0].edit else { panic!("a title clip") };
        assert!(created_track, "a text track is made when there is none");
        let td = clip.title_data().unwrap();
        assert_eq!(td.text(), "STRATA");
        let klipp_timeline::TitleObjectContent::TextBox(tb) = &td.objects[0].content else { panic!("text box") };
        assert!(!tb.glows.is_empty(), "the neon glow landed");
        assert!(tb.motion.enter.is_some(), "the entrance landed");
        assert!(!tb.loops.is_empty(), "the pulse loop landed");
        assert!(matches!(td.background, klipp_timeline::TitleBackground::Color { .. }), "the backing panel landed");
        // Empty text is refused.
        assert!(resolve(&seq, &pool, &[Op::Title { text: "  ".into(), at: Anchor::Secs(0.5), secs: 2.0, track: None, x: None, y: None, size: None, color: None, font: None, bold: None, align: None, backing: None, backing_color: None, full_frame: None, outline: None, outline_color: None, glow: None, glow_color: None, gradient: None, animate: None, animate_secs: None, stagger: None, loop_effect: None, glitch: None, chromatic: None, style: None }]).is_err());
    }

    #[test]
    fn a_look_adds_one_lut_node_after_the_grade_and_clears() {
        let (seq, pool, _media, a, _) = fixture();

        // A look adds a single Lut3D node with the path and opacity.
        let plan = resolve(&seq, &pool, &[Op::Look { clip_id: ClipRef::Id(a), lut_path: "/luts/kodak.cube".into(), name: Some("Kodak 2383".into()), opacity: Some(0.8), remove: false }]).unwrap();
        assert!(plan.steps[0].what.contains("Kodak 2383") && plan.steps[0].what.contains("80%"), "{}", plan.steps[0].what);
        let EditCommand::SetNodeGraph { new_graph, .. } = &plan.steps[0].edit else { panic!("node-graph edit") };
        let luts: Vec<_> = new_graph.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::Lut3D).collect();
        assert_eq!(luts.len(), 1, "one LUT node");
        assert_eq!(luts[0].params.lut_path.as_ref().unwrap().to_str().unwrap(), "/luts/kodak.cube");
        assert_eq!(luts[0].params.lut_opacity, Some(0.8));

        // Applying a second look reuses the node, never stacks a second.
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        {
            let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
            view.apply_forward(&plan.steps[0].edit);
        }
        let plan2 = resolve(&seqs[0], &p, &[Op::Look { clip_id: ClipRef::Id(a), lut_path: "/luts/fuji.cube".into(), name: None, opacity: None, remove: false }]).unwrap();
        let EditCommand::SetNodeGraph { new_graph: g2, .. } = &plan2.steps[0].edit else { panic!("node-graph edit") };
        assert_eq!(g2.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::Lut3D).count(), 1, "still one LUT");
        assert_eq!(g2.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Lut3D).unwrap().params.lut_path.as_ref().unwrap().to_str().unwrap(), "/luts/fuji.cube");

        // Removing clears the LUT path.
        let plan3 = resolve(&seqs[0], &p, &[Op::Look { clip_id: ClipRef::Id(a), lut_path: String::new(), name: None, opacity: None, remove: true }]).unwrap();
        let EditCommand::SetNodeGraph { new_graph: g3, .. } = &plan3.steps[0].edit else { panic!("node-graph edit") };
        assert!(g3.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::Lut3D).all(|n| n.params.lut_path.is_none()), "look cleared");

        // Refusals: a look with no LUT, a remove with nothing on, out of range.
        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        assert!(bad(Op::Look { clip_id: ClipRef::Id(a), lut_path: String::new(), name: None, opacity: None, remove: false }).why.contains("no LUT"));
        assert!(bad(Op::Look { clip_id: ClipRef::Id(a), lut_path: String::new(), name: None, opacity: None, remove: true }).why.contains("no look to remove"));
        assert!(bad(Op::Look { clip_id: ClipRef::Id(a), lut_path: "/x.cube".into(), name: None, opacity: Some(9.0), remove: false }).why.contains("out of range"));
    }

    #[test]
    fn a_secondary_keys_a_colour_and_corrects_only_it() {
        let (seq, pool, _media, a, _) = fixture();
        let q = |target: Option<&str>, hue: Option<f32>, exposure: Option<f32>, saturation: Option<f32>, remove: bool| Op::Qualify {
            clip_id: ClipRef::Id(a), target: target.map(|s| s.to_string()), hue, hue_width: None,
            sat_low: None, sat_high: None, lum_low: None, lum_high: None, invert: false,
            exposure, saturation, hue_shift: None, temperature: None, remove,
            window: None, window_feather: None,
        };

        // Key the sky and cool it down.
        let plan = resolve(&seq, &pool, &[q(Some("sky"), None, None, Some(0.6), false)]).unwrap();
        assert!(plan.steps[0].what.contains("sky") && plan.steps[0].what.contains("saturation 0.6"), "{}", plan.steps[0].what);
        let EditCommand::SetNodeGraph { new_graph, .. } = &plan.steps[0].edit else { panic!("node-graph edit") };
        let node = new_graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Qualifier).expect("a qualifier");
        let qs = node.params.qualifier.as_ref().expect("qualifier settings");
        assert!(qs.enabled && (qs.hue_center - 220.0).abs() < 1.0, "{qs:?}");
        // The change is on the correction the key gates, never on the
        // keyer: a qualifier writes a matte, not a picture.
        let gated = new_graph.edges.iter().find(|e| e.from_node == node.id && e.to_port == 1).map(|e| e.to_node).expect("the key gates a correction");
        let corr = new_graph.nodes.iter().find(|n| n.id == gated).expect("the correction");
        assert_eq!(corr.params.saturation, Some(0.6));

        // Reuse, not stack: a second qualify edits the same node, and undo clears it.
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        {
            let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
            view.apply_forward(&plan.steps[0].edit);
        }
        let plan2 = resolve(&seqs[0], &p, &[q(None, Some(28.0), Some(0.3), None, false)]).unwrap();
        let EditCommand::SetNodeGraph { new_graph: g2, .. } = &plan2.steps[0].edit else { panic!("edit") };
        assert_eq!(g2.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::Qualifier).count(), 1, "one qualifier");
        let plan3 = resolve(&seqs[0], &p, &[q(None, None, None, None, true)]).unwrap();
        let EditCommand::SetNodeGraph { new_graph: g3, .. } = &plan3.steps[0].edit else { panic!("edit") };
        assert!(g3.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::Qualifier).all(|n| n.params.qualifier.as_ref().map(|q| !q.enabled).unwrap_or(true)), "secondary cleared");

        // Refusals: no key, no correction, unknown target, out of range.
        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        assert!(bad(q(None, None, Some(0.5), None, false)).why.contains("nothing keyed"));
        assert!(bad(q(Some("skin"), None, None, None, false)).why.contains("nothing to change"));
        assert!(bad(q(Some("teapot"), None, Some(0.5), None, false)).why.contains("not a colour"));
        assert!(bad(q(None, Some(999.0), Some(0.5), None, false)).why.contains("out of range"));
        assert!(bad(q(None, None, None, None, true)).why.contains("no secondary to remove"));
    }

    #[test]
    fn a_skin_secondary_can_be_held_inside_a_tracked_window() {
        // The fault this exists to fix: a hue key on skin also keys a
        // red jumper, a brick wall and a wooden table, so pulling the
        // face warmer turns all of them. The window says where.
        let (seq, pool, _media, a, _) = fixture();
        let face = |at: f64, x: f32| WindowKey { at, x, y: 0.28, w: 0.12, h: 0.22 };
        let op = Op::Qualify {
            clip_id: ClipRef::Id(a),
            target: Some("skin".into()),
            hue: None, hue_width: None, sat_low: None, sat_high: None, lum_low: None, lum_high: None,
            invert: false, exposure: None, saturation: None, hue_shift: Some(6.0), temperature: None, remove: false,
            window: Some(vec![face(0.0, 0.20), face(1.0, 0.44), face(2.0, 0.62)]),
            window_feather: None,
        };
        let plan = resolve(&seq, &pool, &[op]).unwrap();
        assert!(plan.steps[0].what.contains("window tracked over 3 samples"), "{}", plan.steps[0].what);
        let EditCommand::SetNodeGraph { new_graph, .. } = &plan.steps[0].edit else { panic!("node-graph edit") };

        let qual = new_graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Qualifier).expect("a key");
        let mask = new_graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Mask).expect("a window");
        let combine = new_graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::CombineMattes).expect("the two together");
        // Both mattes meet at the combiner, and only its output gates
        // the correction: "the right colour AND in the right place".
        assert!(new_graph.edges.iter().any(|e| e.from_node == qual.id && e.to_node == combine.id));
        assert!(new_graph.edges.iter().any(|e| e.from_node == mask.id && e.to_node == combine.id));
        assert!(!new_graph.edges.iter().any(|e| e.from_node == qual.id && new_graph.nodes.iter().any(|n| n.id == e.to_node && n.kind == klipp_nodes::NodeKind::ColorBalance)));
        // The window follows the face rather than sitting still.
        let masks = mask.params.masks.as_ref().expect("a mask stack");
        assert_eq!(masks.masks.len(), 1);
        assert!(masks.has_keyframes(), "a moving subject keyframes the window");
        assert!(masks.masks[0].feather > 0.0, "a face window is soft, or it reads as a sticker");
        // And the chain still reads in a colourist's order.
        assert!(new_graph.chain_out_of_order().is_empty(), "{:?}", new_graph.chain_out_of_order());

        // A window given as nonsense is refused rather than drawn wrong.
        let bad = |w: Vec<WindowKey>| {
            resolve(&seq, &pool, &[Op::Qualify {
                clip_id: ClipRef::Id(a), target: Some("skin".into()),
                hue: None, hue_width: None, sat_low: None, sat_high: None, lum_low: None, lum_high: None,
                invert: false, exposure: None, saturation: None, hue_shift: Some(6.0), temperature: None, remove: false,
                window: Some(w), window_feather: None,
            }]).unwrap_err()
        };
        assert!(bad(vec![WindowKey { at: 0.0, x: 0.2, y: 0.2, w: 0.0, h: 0.2 }]).why.contains("not a box"));
        assert!(bad(vec![WindowKey { at: -1.0, x: 0.2, y: 0.2, w: 0.2, h: 0.2 }]).why.contains("not a box"));
    }

    #[test]
    fn a_grade_sets_the_primary_knobs_and_undo_takes_it_back() {
        let (seq, pool, media, a, _) = fixture();
        let plan = resolve(&seq, &pool, &[Op::Grade { clip_id: ClipRef::Id(a), exposure: Some(0.5), contrast: Some(1.2), saturation: Some(1.3), temperature: Some(0.1), tint: None, lift: Some(0.05), gamma: None, gain: Some(-0.1), highlights: None, shadows: Some(0.2), vibrance: Some(0.3), reset: false }]).unwrap();
        assert!(plan.steps[0].what.contains("exposure 0.5") && plan.steps[0].what.contains("gain -0.1"), "{}", plan.steps[0].what);
        let EditCommand::SetNodeGraph { new_graph, .. } = &plan.steps[0].edit else { panic!("a node-graph edit") };
        let node = new_graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).expect("a corrector");
        assert_eq!(node.params.exposure, Some(0.5));
        assert_eq!(node.params.saturation, Some(1.3));
        assert_eq!(node.params.gain_master, Some(-0.1));
        assert_eq!(node.params.shadows, Some(0.2));

        // Replay on a document, then undo.
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        assert!(!view.sequences[0].tracks[0].clips[0].node_graph.is_identity());
        view.apply_inverse(&plan.steps[0].edit);
        assert!(view.sequences[0].tracks[0].clips[0].node_graph.is_identity(), "undo restores the ungraded clip");

        // Out of range, nothing to change, a title: refused.
        let bad = |op: Op| resolve(&seq, &pool, &[op]).unwrap_err();
        assert!(bad(Op::Grade { clip_id: ClipRef::Id(a), exposure: Some(99.0), contrast: None, saturation: None, temperature: None, tint: None, lift: None, gamma: None, gain: None, highlights: None, shadows: None, vibrance: None, reset: false }).why.contains("out of range"));
        assert!(bad(Op::Grade { clip_id: ClipRef::Id(a), exposure: None, contrast: None, saturation: None, temperature: None, tint: None, lift: None, gamma: None, gain: None, highlights: None, shadows: None, vibrance: None, reset: false }).why.contains("nothing to grade"));

        // As the tools send it.
        let _: Vec<Op> = serde_json::from_str(r#"[{"op":"grade","clip_id":{"track":0,"secs":1.0},"exposure":0.3,"saturation":1.1,"reset":true}]"#).unwrap();
        let _ = media;
    }

    #[test]
    fn titles_are_restyled_one_at_a_time_or_a_track_at_once() {
        let (seq, pool, _, _, _) = fixture();
        // Captions, then every one of them made larger, yellow and raised while a lower third is up.
        let ops: Vec<Op> = serde_json::from_str(r##"[
            {"op":"captions","max_chars":30},
            {"op":"style","track":"subtitle","size":48,"color":"#ffe14d","backing":0.75,"outline":0},
            {"op":"style","track":"subtitle","from":{"secs":4.0},"to":{"secs":6.0},"y":0.7}
        ]"##).unwrap();
        let plan = resolve_with(&seq, &pool, &ops, &Spoken).unwrap();
        assert!(plan.steps[1].what.starts_with("restyle 3 titles") && plan.steps[1].what.contains("backing 75% dark"), "{}", plan.steps[1].what);
        assert!(plan.steps[2].what.starts_with("restyle 1 title") && plan.steps[2].what.contains("its x, 0.70"), "{}", plan.steps[2].what);
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for st in &plan.steps {
            view.apply_forward(&st.edit);
        }
        let after = view.sequences[0].clone();
        let caps = &after.tracks.last().unwrap().clips;
        let look = |c: &Clip| {
            let td = c.title_data().unwrap();
            let klipp_timeline::TitleObjectContent::TextBox(tb) = &td.objects[0].content else { panic!("text") };
            (tb.font_size, tb.color, tb.outlines.len(), td.objects[0].transform.center.y, td.background.clone())
        };
        let (size, color, outlines, y0, backing) = look(&caps[0]);
        assert_eq!((size, color, outlines), (48.0, [0xff, 0xe1, 0x4d, 255], 0));
        assert!((y0 - 0.88).abs() < 1e-6, "outside the range it stays low");
        assert!((look(&caps[1]).3 - 0.7).abs() < 1e-6, "the cue from 4.4 s is raised");
        // Three-quarters dark to the eye is an alpha of 0.95 in linear light.
        assert!(matches!(backing, klipp_timeline::TitleBackground::Color { color: [0, 0, 0, a], .. } if (240..=245).contains(&a)), "{backing:?}");
        // Undone, they look as they did.
        view.apply_inverse(&plan.steps[2].edit);
        view.apply_inverse(&plan.steps[1].edit);
        let again = view.sequences[0].tracks.last().unwrap().clips[0].title_data().unwrap().clone();
        assert_eq!(again, plan_caption(&plan, 0), "the caption as it was made");

        // Refusals: nothing named, nothing asked, a colour that is not one, a track with no titles.
        let bad = |json: &str| resolve_with(&seq, &pool, &serde_json::from_str::<Vec<Op>>(json).unwrap(), &Spoken).unwrap_err();
        assert!(bad(r##"[{"op":"style","size":40}]"##).why.contains("no clip or track"));
        assert!(bad(r##"[{"op":"style","track":"video"}]"##).why.contains("nothing to change"));
        assert!(bad(r##"[{"op":"style","track":"video","color":"yellow"}]"##).why.contains("not a colour"));
        assert!(bad(r##"[{"op":"style","track":"video","size":40}]"##).why.contains("no title there"));
    }

    /// The `k`th caption as the plan's captions step made it.
    fn plan_caption(plan: &Plan, k: usize) -> klipp_timeline::TitleData {
        let EditCommand::Batch(edits) = &plan.steps[0].edit else { panic!("a batch") };
        edits.iter().filter_map(|e| match e { EditCommand::LegacyAppendClip { clip, .. } => clip.title_data().cloned(), _ => None }).nth(k).expect("a caption")
    }

    #[test]
    fn a_lower_third_sits_inside_a_shot() {
        let (seq, pool, _, _, _) = fixture();
        let lt = |at: f64, secs: f64| Op::LowerThird { name: "David Heinemeier Hansson".into(), role: Some("Creator of Omarchy".into()), at: Anchor::Secs(at), secs, track: None, side: None, x: None, y: None };
        let plan = resolve(&seq, &pool, &[lt(4.4, 3.0)]).unwrap();
        assert!(plan.steps[0].what.contains("a new text track 0"), "{}", plan.steps[0].what);
        assert_eq!((plan.steps[0].after[0].track, plan.steps[0].after[0].timeline_in_secs, plan.steps[0].after[0].timeline_out_secs), (0, 4.4, 7.4));
        let EditCommand::AddTitleClip { clip, .. } = &plan.steps[0].edit else { panic!("a title clip") };
        let td = clip.title_data().unwrap();
        assert_eq!(td.objects.len(), 2, "the name and the role");
        assert_eq!(td.text(), "David Heinemeier Hansson");

        // Across the cut at 4 s it is refused, with the cut named.
        let err = resolve(&seq, &pool, &[lt(2.0, 4.0)]).unwrap_err();
        assert!(err.why.contains("4.00 s") && err.hint.contains("shot change"), "{err}");
        // Too short to read, no name, past the end: refused.
        assert!(resolve(&seq, &pool, &[lt(4.5, 1.0)]).is_err());
        assert!(resolve(&seq, &pool, &[lt(6.0, 3.0)]).unwrap_err().why.contains("past the end"));
        let nameless = Op::LowerThird { name: " ".into(), role: None, at: Anchor::Secs(4.5), secs: 3.0, track: None, side: None, x: None, y: None };
        assert!(resolve(&seq, &pool, &[nameless]).unwrap_err().hint.contains("cast list"));

        // With a cast list in the brief, the name is the list's and so is the role.
        let mut briefed = seq.clone();
        briefed.brief = Some(klipp_timeline::Brief {
            cast: vec![klipp_timeline::CastMember { name: "David Heinemeier Hansson".into(), role: "Creator of Omarchy".into(), described: "on the right".into() }],
            ..Default::default()
        });
        let from_cast = Op::LowerThird { name: "david heinemeier hansson".into(), role: None, at: Anchor::Secs(4.4), secs: 3.0, track: None, side: None, x: None, y: None };
        let plan = resolve(&briefed, &pool, &[from_cast]).unwrap();
        assert!(plan.steps[0].what.contains("\"David Heinemeier Hansson\", \"Creator of Omarchy\""), "{}", plan.steps[0].what);
        let stranger = Op::LowerThird { name: "Brodie".into(), role: None, at: Anchor::Secs(4.4), secs: 3.0, track: None, side: None, x: None, y: None };
        let err = resolve(&briefed, &pool, &[stranger]).unwrap_err();
        assert!(err.why.contains("not in the brief's cast") && err.hint.contains("David Heinemeier Hansson"), "{err}");

        // A second one finds the text track the first made; the ops read as JSON the way the tools send them.
        let ops: Vec<Op> = serde_json::from_str(r#"[
            {"op":"lower_third","name":"Brodie","at":{"secs":0.5},"secs":3},
            {"op":"lower_third","name":"DHH","role":"Omarchy","at":{"clip_start":{"track":1,"secs":5.0}}},
            {"op":"captions"},
            {"op":"place","media_id":"00000000-0000-0000-0000-000000000000","track":"video","at":{"track_end":"video"}}
        ]"#).unwrap();
        let plan = resolve_with(&seq, &pool, &ops[..3], &Spoken).unwrap();
        assert!(plan.steps[1].what.contains("on track 0") && plan.steps[1].what.contains("4.00-8.00"), "{}", plan.steps[1].what);
    }

    #[test]
    fn a_plan_runs_in_order_and_reads_as_text() {
        let (seq, pool, media, a, b) = fixture();
        let ops = vec![
            Op::Split { clip_id: ClipRef::Id(b), at: Anchor::Marker("intro".into()) },
            Op::Place { media_id: media, track: TrackRef::Kind("video".into()), at: Anchor::ClipEnd(ClipRef::Id(b)), source_in: Some(8.0), source_out: Some(9.0), insert: false, with_audio: false },
            Op::Trim { clip_id: ClipRef::Id(a), edge: Edge::Out, to: Anchor::Secs(3.0), ripple: false, linked: false },
        ];
        // The split replaces b, so its end no longer exists as `b`:
        // the plan must refuse the place with a reason naming that.
        let err = resolve(&seq, &pool, &ops).unwrap_err();
        assert_eq!(err.step, 1);
        assert!(err.why.contains("no clip"), "{err}");

        let ops = vec![
            Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::ClipEnd(ClipRef::Id(b)), source_in: Some(8.0), source_out: Some(9.0), insert: false, with_audio: false },
            Op::Split { clip_id: ClipRef::Id(b), at: Anchor::Marker("intro".into()) },
            Op::Trim { clip_id: ClipRef::Id(a), edge: Edge::Out, to: Anchor::Secs(3.0), ripple: false, linked: false },
        ];
        let plan = resolve(&seq, &pool, &ops).unwrap();
        assert_eq!(plan.steps.len(), 3);
        assert!((plan.duration_secs - 9.0).abs() < 1e-9, "{}", plan.text());
        assert_eq!(plan.steps[1].after.len(), 4, "a split reports both halves of the picture and of its linked sound");
        assert!((plan.steps[1].after[0].timeline_out_secs - 6.0).abs() < 1e-9);
        assert!((plan.steps[2].after[0].timeline_out_secs - 3.0).abs() < 1e-9);
        assert!((plan.steps[2].after[0].source_out_secs - 3.0).abs() < 1e-9, "the source moves with the edge");
        let text = plan.text();
        assert!(text.contains("1. place a.mov on track 0 at 8.00 s"), "{text}");
        assert!(text.contains("2. split"), "{text}");
        // The given sequence is untouched.
        assert_eq!(seq.tracks[0].clips.len(), 2);
    }

    #[test]
    fn refusals_say_why_and_what_would_help() {
        let (seq, pool, media, a, b) = fixture();
        let over = resolve(&seq, &pool, &[Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::Secs(2.0), source_in: None, source_out: Some(1.0), insert: false, with_audio: false }]).unwrap_err();
        assert!(over.why.contains("overlap"), "{over}");
        assert!(over.hint.contains("clip_end"), "{over}");
        let past = resolve(&seq, &pool, &[Op::Trim { clip_id: ClipRef::Id(b), edge: Edge::Out, to: Anchor::Secs(11.0), ripple: false, linked: false }]).unwrap_err();
        assert!(past.why.contains("after this clip's out point"), "{past}");
        let into = resolve(&seq, &pool, &[Op::Move { clip_id: ClipRef::Id(a), to: Anchor::Secs(1.0) }]).unwrap_err();
        assert!(into.why.contains("overlap"), "{into}");
        let outside = resolve(&seq, &pool, &[Op::Split { clip_id: ClipRef::Id(a), at: Anchor::Secs(4.0) }]).unwrap_err();
        assert!(outside.why.contains("not inside"), "{outside}");
        let no_marker = resolve(&seq, &pool, &[Op::Move { clip_id: ClipRef::Id(a), to: Anchor::Marker("outro".into()) }]).unwrap_err();
        assert!(no_marker.why.contains("no marker"), "{no_marker}");
    }

    #[test]
    fn a_ripple_remove_closes_the_gap_and_the_commands_replay_on_the_document() {
        let (seq, pool, _, a, b) = fixture();
        let plan = resolve(&seq, &pool, &[Op::Remove { clip_id: ClipRef::Id(a), ripple: true }]).unwrap();
        assert_eq!(plan.steps[0].after[0].clip_id, b);
        assert!((plan.steps[0].after[0].timeline_in_secs).abs() < 1e-9, "b moved to the start");
        // Applying the plan's commands to the real document gives the
        // state the plan reported; the inverse takes it back.
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for s in &plan.steps {
            view.apply_forward(&s.edit);
        }
        assert_eq!(seqs[0].tracks[0].clips.len(), 1);
        assert_eq!(seqs[0].tracks[0].clips[0].timeline_in, tc(0.0));
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for s in plan.steps.iter().rev() {
            view.apply_inverse(&s.edit);
        }
        assert_eq!(seqs[0].tracks[0].clips.len(), 2);
        assert_eq!(seqs[0].tracks[0].clips[1].timeline_in, tc(4.0));
    }

    #[test]
    fn times_snap_to_frames_including_ntsc() {
        let fps = Fps::new(30000, 1001);
        let t = snap(1.0, fps);
        assert_eq!(t.value, 30 * 1001, "thirty frames of 1001 ticks");
        assert!((t.to_secs_f64() - 1.001).abs() < 1e-9);
        let ops_json = r#"[{"op":"move","clip_id":"00000000-0000-0000-0000-000000000000","to":{"offset":{"from":{"clip_end":"00000000-0000-0000-0000-000000000001"},"secs":-0.5}}}]"#;
        let ops: Vec<Op> = serde_json::from_str(ops_json).unwrap();
        assert!(matches!(&ops[0], Op::Move { to: Anchor::Offset { .. }, .. }));
    }

    #[test]
    fn an_insert_makes_room_and_a_ripple_trim_closes_up() {
        let (seq, pool, media, a, b) = fixture();
        // Insert one second of the file at 2 s: a is split there and
        // everything after moves a second later, on every track.
        let plan = resolve(&seq, &pool, &[Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::Secs(2.0), source_in: Some(9.0), source_out: Some(10.0), insert: true, with_audio: false }]).unwrap();
        assert!(plan.steps[0].what.contains("2 clip(s) split"), "the picture and its audio: {}", plan.steps[0].what);
        assert!((plan.duration_secs - 9.0).abs() < 1e-9, "{}", plan.text());
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        let v = &seqs[0].tracks[0].clips;
        assert_eq!(v.len(), 4, "a's two halves, the insert, b");
        assert_eq!(v[1].timeline_in, tc(2.0));
        assert_eq!(v[3].timeline_in, tc(5.0), "b moved a second later");
        assert_eq!(seqs[0].tracks[1].clips[2].timeline_in, tc(5.0), "and its audio with it (its own audio was split too)");

        // A ripple out trim of a by a second: b closes up.
        let plan = resolve(&seq, &pool, &[Op::Trim { clip_id: ClipRef::Id(a), edge: Edge::Out, to: Anchor::Secs(3.0), ripple: true, linked: false }]).unwrap();
        let after = &plan.steps[0].after;
        assert_eq!(after.len(), 2);
        assert!((after[1].timeline_in_secs - 3.0).abs() < 1e-9, "{after:?}");
        // A ripple head trim keeps the start and shortens from the front.
        let plan = resolve(&seq, &pool, &[Op::Trim { clip_id: ClipRef::Id(b), edge: Edge::In, to: Anchor::Secs(5.0), ripple: true, linked: false }]).unwrap();
        let after = &plan.steps[0].after;
        assert!((after[0].timeline_in_secs - 4.0).abs() < 1e-9 && (after[0].timeline_out_secs - 7.0).abs() < 1e-9, "{after:?}");
        assert!((after[0].source_in_secs - 5.0).abs() < 1e-9, "{after:?}");
    }

    /// A clip placed before the sequence changed rate keeps the old rate
    /// in its timecodes; the cut between it and a clip at the new rate is
    /// still a cut.
    #[test]
    fn a_j_cut_meets_a_neighbour_at_another_rate() {
        let (mut seq, pool, _, _, b) = fixture();
        for t in seq.tracks.iter_mut() {
            let c = &mut t.clips[0];
            c.timeline_in = c.timeline_in.rescale_to(50);
            c.timeline_out = c.timeline_out.rescale_to(50);
            assert_ne!(c.timeline_out, tc(4.0), "the fields differ");
        }
        let plan = resolve(&seq, &pool, &[Op::JCut { incoming_clip: ClipRef::Id(b), frames: 10 }]).unwrap();
        assert!(plan.steps[0].what.starts_with("J-cut of 10 frames"), "{}", plan.steps[0].what);
    }

    #[test]
    fn j_and_l_cuts_move_the_audio_edge_and_leave_the_picture() {
        let (seq, pool, _, a, b) = fixture();
        let plan = resolve(&seq, &pool, &[Op::JCut { incoming_clip: ClipRef::Id(b), frames: 10 }]).unwrap();
        assert!(plan.steps[0].what.starts_with("J-cut of 10 frames"), "{}", plan.steps[0].what);
        let after = &plan.steps[0].after;
        assert!((after[0].timeline_out_secs - 3.6).abs() < 1e-9, "outgoing audio ends 10 frames early: {after:?}");
        assert!((after[1].timeline_in_secs - 3.6).abs() < 1e-9 && (after[1].source_in_secs - 3.6).abs() < 1e-9, "{after:?}");
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        assert_eq!(seqs[0].tracks[0].clips[1].timeline_in, tc(4.0), "the picture cut has not moved");
        assert_eq!(seqs[0].tracks[1].clips[1].timeline_in, tc(3.6));

        let plan = resolve(&seq, &pool, &[Op::LCut { outgoing_clip: ClipRef::Id(a), frames: 5 }]).unwrap();
        let after = &plan.steps[0].after;
        assert!((after[0].timeline_out_secs - 4.2).abs() < 1e-9 && (after[0].source_out_secs - 4.2).abs() < 1e-9, "{after:?}");
        assert!((after[1].timeline_in_secs - 4.2).abs() < 1e-9, "{after:?}");

        let too_far = resolve(&seq, &pool, &[Op::JCut { incoming_clip: ClipRef::Id(b), frames: 120 }]).unwrap_err();
        assert!(too_far.why.contains("shorter than a frame"), "{too_far}");
        let no_cut = resolve(&seq, &pool, &[Op::JCut { incoming_clip: ClipRef::Id(a), frames: 5 }]).unwrap_err();
        assert!(no_cut.why.contains("no clip cut before"), "{no_cut}");
    }

    #[test]
    fn a_place_brings_linked_sound_sets_an_empty_sequence_and_a_split_half_is_named_by_place() {
        let (seq, pool, media, _, _) = fixture();
        // An empty sequence at 25 fps with a 24 fps item to place.
        let mut empty = Sequence::new_default();
        empty.fps = Fps::new(25, 1);
        empty.tracks = vec![Track::new("V1", TrackKind::Video)];
        let mut pool2 = pool.clone();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/b.mov".into(), None);
        item.duration = RationalTime::from_secs_f64(10.0, 24);
        item.fps = Fps::new(24, 1);
        item.resolution = klipp_core::Resolution { width: 1280, height: 720 };
        item.video_codec = "hevc".into();
        item.has_audio = true;
        let media2 = pool2.add(item);
        let plan = resolve(&empty, &pool2, &[
            Op::Place { media_id: media2, track: TrackRef::Kind("video".into()), at: Anchor::Secs(0.0), source_in: Some(1.0), source_out: Some(3.0), insert: false, with_audio: true },
            Op::Split { clip_id: ClipRef::At { track: 0, secs: 0.5 }, at: Anchor::Secs(1.0) },
            Op::JCut { incoming_clip: ClipRef::At { track: 0, secs: 1.5 }, frames: 6 },
            // Frames count at the rate the sequence has by now, 24, not
            // the 25 it started with: 48 frames is 2.0 s, the first clip's
            // end, and the source range was snapped at 24 too.
            Op::Place { media_id: media2, track: TrackRef::Index(0), at: Anchor::Frame(48), source_in: Some(4.0), source_out: Some(5.0), insert: false, with_audio: true },
        ]).unwrap();
        let w = &plan.steps[0].what;
        assert!(w.contains("new audio track 1") && w.contains("1280x720 at 24.000 fps"), "{w}");
        assert_eq!((plan.steps[0].after[0].timeline_in_secs, plan.steps[0].after[0].timeline_out_secs), (0.0, 2.0));
        assert_eq!((plan.steps[3].after[0].timeline_in_secs, plan.steps[3].after[0].timeline_out_secs), (2.0, 3.0));
        assert_eq!(plan.steps[0].after.len(), 2, "picture and sound");
        assert_eq!(plan.steps[0].after[1].track, 1);
        assert!(plan.steps[2].what.starts_with("J-cut of 6 frames"), "{}", plan.steps[2].what);
        // Replay on a document: the audio track exists, the clips are linked, the rate is the item's.
        let mut seqs = vec![empty.clone()];
        let mut p = pool2.clone();
        let mut fps = empty.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for st in &plan.steps {
            view.apply_forward(&st.edit);
        }
        assert_eq!(seqs[0].tracks.len(), 2);
        assert_eq!(seqs[0].fps, Fps::new(24, 1));
        assert_eq!(seqs[0].tracks[0].clips.len(), 3, "two halves and the second place");
        let v0 = &seqs[0].tracks[0].clips[0];
        assert!(v0.linked_clip_id.is_some());

        // Without its sound, a video clip is muted: the engine would
        // otherwise play the sound in its file.
        let plan = resolve(&seq, &pool, &[Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::Secs(20.0), source_in: Some(1.0), source_out: Some(2.0), insert: false, with_audio: false }]).unwrap();
        assert!(plan.steps[0].what.ends_with("its sound muted"), "{}", plan.steps[0].what);
        assert_eq!(plan.steps[0].after.len(), 1);
        match &plan.steps[0].edit {
            EditCommand::LegacyAppendClip { clip, .. } => assert_eq!(clip.volume, 0.0),
            other => panic!("expected an append, got {other:?}"),
        }

        // An add_track on top shifts the indices, and says so.
        let plan = resolve(&seq, &pool, &[Op::AddTrack { kind: "video".into(), name: None }]).unwrap();
        assert!(plan.steps[0].what.contains("as track 0") && plan.steps[0].what.contains("moves down"), "{}", plan.steps[0].what);
        let _ = media;
    }
}

#[cfg(test)]
mod generator_tests {
    use super::*;

    #[test]
    fn a_generator_uses_native_parameters_and_undo_removes_its_new_track() {
        let seq = Sequence::new_default();
        let mut pool = MediaPool::new();
        let data = klipp_core::GeneratorData::Gradient { top: [16, 32, 48, 255], bottom: [64, 80, 96, 255], angle_deg: 45.0, four_point: None };
        let op = Op::Generator { data: data.clone(), at: Anchor::Secs(1.0), secs: Some(2.0), end: None, track: None };
        let plan = resolve(&seq, &pool, &[op]).unwrap();
        let before = serde_json::to_value(&seq).unwrap();
        let count = seq.tracks.len();
        let mut fps = seq.fps;
        let mut sequences = vec![seq];
        let mut view = crate::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        let clip = &view.sequences[0].tracks[count].clips[0];
        assert_eq!(clip.kind, ClipKind::Generator(data));
        assert!((secs(clip.timeline_in) - 1.0).abs() < 1e-6);
        assert!((secs(clip.timeline_out) - 3.0).abs() < 1e-6);
        view.apply_inverse(&plan.steps[0].edit);
        assert_eq!(serde_json::to_value(&view.sequences[0]).unwrap(), before);
    }

    #[test]
    fn invalid_generator_ranges_and_overlaps_are_refused() {
        let seq = Sequence::new_default();
        let pool = MediaPool::new();
        let op = Op::Generator { data: klipp_core::GeneratorData::Countdown, at: Anchor::Secs(0.0), secs: Some(2.0), end: None, track: Some(TrackRef::Kind("video".into())) };
        assert!(resolve(&seq, &pool, &[op.clone(), op.clone()]).is_err());
        let mut bad = op;
        if let Op::Generator { secs, .. } = &mut bad { *secs = Some(-1.0); }
        assert!(resolve(&seq, &pool, &[bad]).is_err());
    }
}

#[cfg(test)]
mod still_placement_tests {
    use super::*;

    #[test]
    fn a_five_second_still_can_be_placed_and_trimmed_beyond_its_default() {
        let seq = Sequence::new_default();
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/plate.png".into(), None);
        item.is_still = true;
        item.duration = snap(5.0, seq.fps);
        let media = pool.add(item);
        let place = |from, to| Op::Place { media_id: media, track: TrackRef::Index(0), at: Anchor::Secs(2.0), source_in: Some(from), source_out: to, insert: false, with_audio: false };
        let plan = resolve(&seq, &pool, &[place(0.0, Some(20.0)), Op::Trim { clip_id: ClipRef::At { track: 0, secs: 3.0 }, edge: Edge::Out, to: Anchor::Secs(32.0), ripple: false, linked: false }]).unwrap();
        assert_eq!(plan.steps[0].after[0].timeline_out_secs, 22.0);
        assert_eq!(plan.steps[1].after[0].timeline_out_secs, 32.0);
        assert_eq!(resolve(&seq, &pool, &[place(0.0, None)]).unwrap().steps[0].after[0].timeline_out_secs, 7.0);
        assert!(resolve(&seq, &pool, &[place(10.0, Some(30.0))]).is_ok());
        for (from, to) in [(-1.0, 20.0), (0.0, f64::NAN), (0.0, f64::INFINITY), (0.0, 1e300), (20.0, 10.0)] {
            assert!(resolve(&seq, &pool, &[place(from, Some(to))]).is_err());
        }
        pool.get_mut(&media).unwrap().is_still = false;
        assert!(resolve(&seq, &pool, &[place(0.0, Some(20.0))]).is_err(), "moving footage still has a source end");
    }
}
