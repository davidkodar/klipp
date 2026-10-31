use crate::clip_kind::ClipKind;
use crate::marker::Marker;
use klipp_core::{AudioEffectInstance, ClipId, Fps, GradeGroupId, GroupId, MarkerColor, MediaId, RationalTime, Resolution, SequenceId, Timecode, TrackId, TransitionId};
use serde::{Deserialize, Serialize};
use crate::picture_store::ClipPictureFields;
use std::collections::HashMap;

pub use klipp_core::types::{ChannelConfig, FadeCurve};

/// One frame at `fps`, as a time at the sequence's own rate. `rate` is
/// the fps numerator, so a frame is `den` ticks: one at a whole rate,
/// 1001 at an NTSC one. Taking the frame as a single tick — which is
/// the same thing only at a whole rate — makes any rule written in
/// frames a thousand times looser at 23.976, 29.97 or 59.94.
pub fn one_frame(fps: Fps) -> Timecode {
    Timecode { value: fps.den.max(1) as i64, rate: fps.rate() }
}

// ═══════════════════════════════════════════════════════════════════
// Magnetic Timeline Types
// ═══════════════════════════════════════════════════════════════════

/// Magnetic role of a track — determines how clips are positioned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TrackRole {
    /// Traditional free placement. No magnetic behaviour. (default)
    #[default]
    Free,
    /// The primary storyline. Always gapless. Exactly one per Sequence.
    Primary,
    /// Connected lane. Individual clips may have magnetic_anchor set.
    Connected,
    /// Connected Storyline. The whole track is anchored as one unit.
    ConnectedStoryline,
}

pub use klipp_core::ClipRole;

/// Anchor alignment mode for connected clips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnchorAlignment {
    /// clip.timeline_in = anchor.timeline_in + offset (most common)
    StartAligned,
    /// clip.timeline_out = anchor.timeline_in + offset
    EndAligned,
}

/// Connection from a clip (or storyline) to an anchor point on the primary storyline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MagneticAnchor {
    /// Which primary clip this is anchored to.
    pub anchor_clip_id: ClipId,
    /// Time offset from the anchor clip's timeline_in.
    pub offset: RationalTime,
    /// Whether the offset is measured from the start or end of the connected clip.
    pub alignment: AnchorAlignment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub fps: Fps,
    pub resolution: Resolution,
    /// Which settings may be inferred from the first video clip.
    /// Persisted so explicit project settings survive undo and reopening.
    #[serde(default)]
    pub first_clip_autoset: FirstClipAutoSet,
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub in_point: Option<Timecode>,
    #[serde(default)]
    pub out_point: Option<Timecode>,
    #[serde(default)]
    pub markers: Vec<Marker>,
    /// Remembered playhead position for multi-sequence tab switching.
    #[serde(default)]
    pub last_playhead: Option<RationalTime>,
    /// What the piece is for; see [`crate::Brief`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<crate::Brief>,
    /// The paper edit, proposed and approved before it is built; see
    /// [`crate::EditPlan`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_plan: Option<crate::EditPlan>,
    /// What was decided about the check's findings: kept on purpose or
    /// to be fixed, with why (#1932). Empty for a sequence never reviewed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<crate::FindingReview>,
    /// A version at another aspect still to be reframed to its frame once
    /// its files' framing analysis is done (#2211, #2231): kept with the
    /// project so a version made just before quitting is reframed when
    /// it is opened again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub awaiting_refit: bool,
    /// The sequence this one is a version of at another aspect, and that
    /// master's cut when it was copied (#2724): a master cut since makes
    /// the version older than it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_of: Option<VersionOf>,
    /// A move over the whole sequence; see [`Camera`]. Empty means none,
    /// which is what every sequence made before this had.
    #[serde(default, skip_serializing_if = "Camera::is_default")]
    pub camera: Camera,
    /// A grade over the whole sequence, run on every frame after the
    /// clips are graded and composited, in viewer and export alike: where
    /// the overall look of a piece goes, rather than into every clip.
    /// `None` is no timeline grade, as every sequence made before this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_graph: Option<crate::Grade>,
    /// Grade groups (#237): clips that share a pre-clip grade, run before
    /// each member's own, and a post-clip grade, run after it. Change a
    /// group's grade once and every member follows. Empty for every
    /// sequence made before this.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grade_groups: Vec<GradeGroup>,
    /// Whether positions on this sequence are counted drop-frame.
    ///
    /// Only 29.97 and 59.94 have drop-frame at all — it skips frame
    /// *numbers*, never frames, so the count keeps up with the wall
    /// clock the NTSC rates otherwise fall behind by 3.6 s an hour.
    ///
    /// It is a choice rather than a property of the rate: 29.97
    /// non-drop is a real delivery, and a sequence has to be able to say
    /// so. A sequence made at an NTSC rate starts drop-frame because
    /// that is the overwhelming convention, and a project saved before
    /// this field existed takes the same default on load
    /// ([`Sequence::default_drop_frame`]).
    #[serde(default)]
    pub drop_frame: bool,
    /// How bright a title's white is in an HDR delivery, cd/m² (#1723).
    /// An SDR picture has no say in it; under PQ or HLG a title's white
    /// would otherwise sit at SDR reference white, 203 nits, as BT.2408
    /// places graphics. The render holds it under the delivery's peak.
    #[serde(default = "default_title_nits", skip_serializing_if = "is_default_title_nits")]
    pub title_nits: f32,
    /// Colour labels on the nodes of its graphs, by path (#1753); see
    /// [`crate::NodeLabel`]. Empty for every sequence made before them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_labels: Vec<crate::NodeLabel>,
    /// The aux buses its tracks send to (#2861); see [`crate::routing`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buses: Vec<crate::AuxBus>,
}

/// [`Sequence::title_nits`] when nobody has chosen one: SDR reference
/// white, where BT.2408 puts graphics in an HDR programme.
pub const DEFAULT_TITLE_NITS: f32 = 203.0;

/// The title brightnesses a sequence may ask for, cd/m²: from a dim
/// caption up to the brightest PQ mastering display.
pub const TITLE_NITS_RANGE: std::ops::RangeInclusive<f32> = 50.0..=4000.0;

fn default_title_nits() -> f32 { DEFAULT_TITLE_NITS }

fn is_default_title_nits(n: &f32) -> bool { *n == DEFAULT_TITLE_NITS }

/// Which of a sequence's settings the first video clip placed on it may
/// set: an "Automatic" size or rate follows the footage (as in Final Cut),
/// while one the person or an agent chose is kept (#496).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstClipAutoSet {
    #[serde(default = "default_true")]
    pub resolution: bool,
    #[serde(default = "default_true")]
    pub fps: bool,
    /// The person answered whether a first clip of another rate changes
    /// the sequence's (#2965): the sequence is not asked again, whatever
    /// clip comes next. Its rate stays changeable in Sequence Settings.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rate_answered: bool,
}

impl Default for FirstClipAutoSet {
    fn default() -> Self { Self { resolution: true, fps: true, rate_answered: false } }
}


/// A move over the whole sequence, rather than over one clip.
///
/// `zoom`, `pan` and `follow` animate a single clip, so a drift across a
/// scene that several layers live in has to be written into each of them
/// and kept in step by hand. A camera is written once and every layer
/// inherits it, including layers that come and go during the move.
///
/// It reads as a camera, not as a transform of the picture: `x` and `y`
/// are the point in the scene the camera looks at, as fractions of the
/// frame, so moving the camera right moves the scene left. `zoom` is how
/// far in it is, 1 being the scene as composed. `roll` turns the camera
/// clockwise in degrees, so the scene turns the other way.
///
/// Every list empty is no camera at all, which costs nothing to resolve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// Where it looks, across: 0.5 is the middle of the scene.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub x_keyframes: Vec<klipp_core::Keyframe>,
    /// Where it looks, down.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub y_keyframes: Vec<klipp_core::Keyframe>,
    /// How far in, 1 being the scene as composed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zoom_keyframes: Vec<klipp_core::Keyframe>,
    /// Its roll, clockwise in degrees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roll_keyframes: Vec<klipp_core::Keyframe>,
    /// How long the shutter is open, as a share of the frame in degrees:
    /// 180 is half a frame, the film look; 0, the default, is no motion
    /// blur. Every layer that moves while it is open is smeared along
    /// its path — the card's own move and the camera's alike.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub shutter_deg: f32,
    /// How many instants across the open shutter are blended. More is
    /// smoother and slower.
    #[serde(default = "default_shutter_samples", skip_serializing_if = "is_default_shutter_samples")]
    pub shutter_samples: u8,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

fn default_shutter_samples() -> u8 {
    8
}

fn is_default_shutter_samples(n: &u8) -> bool {
    *n == default_shutter_samples()
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            x_keyframes: Vec::new(),
            y_keyframes: Vec::new(),
            zoom_keyframes: Vec::new(),
            roll_keyframes: Vec::new(),
            shutter_deg: 0.0,
            shutter_samples: default_shutter_samples(),
        }
    }
}

impl Camera {
    /// Whether there is nothing to save: no move and no shutter.
    pub fn is_default(&self) -> bool {
        self.is_still() && self.shutter_deg == 0.0 && self.shutter_samples == default_shutter_samples()
    }

    /// Whether the camera does not move: nothing to resolve.
    pub fn is_still(&self) -> bool {
        self.x_keyframes.is_empty()
            && self.y_keyframes.is_empty()
            && self.zoom_keyframes.is_empty()
            && self.roll_keyframes.is_empty()
    }
}

/// Which of a grade group's two grades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeStage {
    /// Before each member's own grade: a normalisation every member
    /// shares, on the footage as it is decoded.
    Pre,
    /// After each member's own grade: the scene's look.
    Post,
}

/// A grade group (#237), as a colourist's group in Resolve: the clips of
/// a scene or a camera share a pre-clip grade, run on the decoded
/// footage before each member's own grade, and a post-clip grade, run
/// after it. Membership is [`Clip::grade_group`]; a clip is in one group
/// at most. The clips keep their own grades.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GradeGroup {
    pub id: GradeGroupId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre: Option<crate::Grade>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<crate::Grade>,
    /// The look this group's shots are graded and judged to instead of
    /// the brief's: a film warm in the south and cool in the north is two
    /// looks, and one target pulled every shot toward its own (#793).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look: Option<klipp_color::measure::LookTarget>,
    /// The group leaves the look on purpose: its shots are judged for
    /// faults (clipping, cast, black point, faces) and not for taste.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub departs: bool,
}

impl GradeGroup {
    pub fn new(name: impl Into<String>) -> Self {
        Self { id: GradeGroupId::new(), name: name.into(), pre: None, post: None, look: None, departs: false }
    }

    pub fn stage(&self, stage: GradeStage) -> Option<&klipp_nodes::NodeGraph> {
        match stage {
            GradeStage::Pre => self.pre.as_ref().map(|g| &g.0),
            GradeStage::Post => self.post.as_ref().map(|g| &g.0),
        }
    }

    /// Only for [`GradeGroup::stage_slot`]: callers change a stage through
    /// the slot, which stage B's storage can keep.
    pub(crate) fn stage_mut(&mut self, stage: GradeStage) -> &mut Option<crate::Grade> {
        match stage {
            GradeStage::Pre => &mut self.pre,
            GradeStage::Post => &mut self.post,
        }
    }

    /// The id a stage of this group is edited under as a clip, as the
    /// timeline grade is (see [`Sequence::timeline_grade_clip_id`]):
    /// fixed for the group, and never a real clip's.
    pub fn stage_clip_id(&self, stage: GradeStage) -> ClipId {
        const PRE: u128 = 0x6772_6f75_702d_7072_652d_6772_6164_0001;
        const POST: u128 = 0x6772_6f75_702d_706f_7374_2d67_7261_0002;
        ClipId(uuid::Uuid::from_u128(self.id.0.as_u128() ^ if stage == GradeStage::Pre { PRE } else { POST }))
    }

    /// `clip`'s grade inside this group: the pre-clip grade, then the
    /// clip's own, then the post-clip grade, every node keeping its id.
    /// A stage that does nothing is left out; a stage pinned to another
    /// working space than the clip's grade cannot run in it and is left
    /// out too rather than shifting the colour (the splice refuses it and
    /// changes nothing).
    pub fn wrap(&self, clip_graph: &klipp_nodes::NodeGraph) -> klipp_nodes::NodeGraph {
        self.wrap_mapped(clip_graph).0
    }

    /// [`GradeGroup::wrap`], and where each stage's nodes are in the
    /// wrapped graph, by stage and their own id. A stage node keeps its id
    /// unless the graph it joins already holds it (a grade copied into a
    /// stage): then it takes [`klipp_nodes::graph::spliced_id`]'s, by the
    /// stage's clip id (M3 step 16a), so two nodes of one id never run.
    pub fn wrap_mapped(&self, clip_graph: &klipp_nodes::NodeGraph) -> (klipp_nodes::NodeGraph, HashMap<(GradeStage, klipp_nodes::NodeId), klipp_nodes::NodeId>) {
        let mut g = clip_graph.clone();
        let mut placed = HashMap::new();
        for stage in [GradeStage::Pre, GradeStage::Post] {
            let Some(graph) = self.stage(stage).filter(|p| !p.is_identity()) else { continue };
            let held: std::collections::HashSet<_> = g.nodes.iter().map(|n| n.id).collect();
            let (graph, renamed) = graph.with_unique_ids(self.stage_clip_id(stage).0, |id| held.contains(&id));
            let joined = if stage == GradeStage::Pre { g.prepend_keeping_ids(&graph) } else { g.append_keeping_ids(&graph) };
            if joined.is_ok() {
                let own = |id: klipp_nodes::NodeId| renamed.iter().find(|(_, now)| **now == id).map_or(id, |(was, _)| *was);
                for n in graph.nodes.iter().filter(|n| !held.contains(&n.id) && g.nodes.iter().any(|m| m.id == n.id)) {
                    placed.insert((stage, own(n.id)), n.id);
                }
            }
        }
        (g, placed)
    }

    /// Whether this group grades anything.
    pub fn grades(&self) -> bool {
        self.stage(GradeStage::Pre).is_some_and(|g| !g.is_identity()) || self.stage(GradeStage::Post).is_some_and(|g| !g.is_identity())
    }
}

/// Where a version at another aspect came from (#2724): its master, and
/// the master's [`Sequence::cut_stamp`] when it was copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionOf {
    pub master: SequenceId,
    pub cut: u64,
}

impl Sequence {
    /// A stamp of the sequence's cut (#2724): every clip's place on the
    /// timeline, its source range, its media and speed, on every track. A
    /// trim, a shot added, removed or moved changes it; a grade or an
    /// effect does not. FNV-1a, the same in every build.
    pub fn cut_stamp(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |bytes: &[u8]| {
            for b in bytes {
                h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
            }
        };
        for (ti, track) in self.tracks.iter().enumerate() {
            eat(&(ti as u64).to_le_bytes());
            for c in &track.clips {
                eat(c.id.0.as_bytes());
                for t in [c.timeline_in, c.timeline_out, c.source_in, c.source_out] {
                    eat(&t.to_secs_f64().to_bits().to_le_bytes());
                }
                eat(&c.speed.to_bits().to_le_bytes());
                eat(&[u8::from(c.enabled), u8::from(c.hold.is_some())]);
                if let Some(m) = c.media_id() {
                    eat(m.0.as_bytes());
                }
            }
        }
        h
    }

    /// Whether this version is older than its master's cut, among `all`:
    /// `None` when it is no version, or its master is gone.
    pub fn version_is_stale(&self, all: &[Sequence]) -> Option<bool> {
        let v = self.version_of?;
        let master = all.iter().find(|s| s.id == v.master)?;
        Some(master.cut_stamp() != v.cut)
    }

    /// The size and rate the sequence would take from a first video clip
    /// of `resolution` and `fps`, where [`Self::first_clip_autoset`] allows
    /// it, without taking them: the settings before and after. `None` when
    /// the sequence already has a video clip or neither setting follows the
    /// footage. The one rule for every way a first clip is placed, the
    /// app's appends and the document's `place` alike (#496, #534).
    #[allow(clippy::type_complexity)]
    pub fn first_clip_format(
        &self,
        resolution: Resolution,
        fps: Fps,
    ) -> Option<((Resolution, Fps), (Resolution, Fps))> {
        let auto = self.first_clip_autoset;
        if self.has_video_clip() || !(auto.resolution || auto.fps) || resolution.width == 0 || resolution.height == 0 {
            return None;
        }
        let prev = (self.resolution, self.fps);
        let new = (if auto.resolution { resolution } else { self.resolution }, if auto.fps { fps } else { self.fps });
        Some((prev, new))
    }

    /// Take the size and rate of the first video clip placed, where
    /// [`Self::first_clip_autoset`] allows it: [`Self::first_clip_format`],
    /// applied. `None` when nothing may change; otherwise the settings
    /// before and after, for the undo history.
    #[allow(clippy::type_complexity)]
    pub fn adopt_first_clip(
        &mut self,
        resolution: Resolution,
        fps: Fps,
    ) -> Option<((Resolution, Fps), (Resolution, Fps))> {
        let change = self.first_clip_format(resolution, fps)?;
        (self.resolution, self.fps) = change.1;
        Some(change)
    }

    /// The rate to ask about before a video clip of `fps` is placed: the
    /// sequence's own, when this would be its first video clip, the
    /// sequence keeps a rate of its own and `fps` is another. `None`
    /// otherwise: a sequence that follows its first clip takes the rate
    /// without asking, once a video clip is down every later clip is
    /// conformed to the rate there is (#2550), and once the question was
    /// answered it is not asked again (#2965).
    pub fn first_clip_rate_question(&self, fps: Fps) -> Option<Fps> {
        let asked = self.first_clip_autoset.rate_answered;
        (!asked && !self.has_video_clip() && !self.first_clip_autoset.fps && fps != self.fps).then_some(self.fps)
    }

    fn has_video_clip(&self) -> bool {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Video).any(|t| !t.clips.is_empty())
    }

    /// Whether nothing is on the timeline: no enabled clip but gaps on any
    /// track. An export of it is refused (#655): it rendered a 261-byte
    /// file and called that a success.
    pub fn has_nothing_to_export(&self) -> bool {
        !self.tracks.iter().flat_map(|t| t.clips.iter()).any(|c| c.enabled && !matches!(c.kind, ClipKind::Gap))
    }

    /// The grade group `id`, if this sequence has it.
    pub fn grade_group(&self, id: GradeGroupId) -> Option<&GradeGroup> {
        self.grade_groups.iter().find(|g| g.id == id)
    }

    /// The group and stage `clip_id` stands for, when it is a group
    /// stage's editing id (see [`GradeGroup::stage_clip_id`]).
    pub fn grade_group_stage_of(&self, clip_id: ClipId) -> Option<(GradeGroupId, GradeStage)> {
        self.grade_groups.iter().find_map(|g| {
            [GradeStage::Pre, GradeStage::Post].into_iter().find(|s| g.stage_clip_id(*s) == clip_id).map(|s| (g.id, s))
        })
    }

    /// Every member clip's grade with its group's around it (#237): what
    /// the renderer runs, so the viewer and export alike grade a member
    /// pre-clip, own, post-clip. Done on the render copy (see
    /// [`crate::flatten::flatten_sequence`]), never on the document: the
    /// clip keeps its own grade and the group keeps its own.
    pub fn bake_grade_groups(&mut self) {
        if !self.grade_groups.iter().any(GradeGroup::grades) {
            return;
        }
        let groups: HashMap<GradeGroupId, GradeGroup> = self.grade_groups.iter().filter(|g| g.grades()).map(|g| (g.id, g.clone())).collect();
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                // Already baked (its own grade kept aside): once is all.
                if clip.picture.view().own_node_graph.is_some() {
                    continue;
                }
                if let Some(group) = clip.grade_group.and_then(|id| groups.get(&id)) {
                    let wrapped = group.wrap(&clip.picture.view().node_graph.0);
                    let own = std::mem::replace(&mut clip.view_mut().node_graph.0, wrapped);
                    clip.view_mut().own_node_graph = Some(Box::new(own));
                }
            }
        }
    }

    /// Whether a render copy of this sequence differs from it: a grade
    /// group that grades, or a track with effects of its own.
    pub fn bakes_for_render(&self) -> bool {
        self.grade_groups.iter().any(GradeGroup::grades) || self.tracks.iter().any(|t| t.effects.touches_picture())
    }

    /// This sequence as it renders: its grade groups' grades wrapped
    /// round their members' (see [`Sequence::bake_grade_groups`]), then
    /// its tracks' effects put on their clips (see
    /// [`Sequence::bake_track_effects`]), in that order wherever a render
    /// copy is made, so the viewer and the export make the same one. A
    /// copy only when there is something to bake.
    pub fn baked_for_render(&self) -> std::borrow::Cow<'_, Sequence> {
        if !self.bakes_for_render() {
            return std::borrow::Cow::Borrowed(self);
        }
        let mut s = self.clone();
        s.bake_grade_groups();
        s.bake_track_effects();
        std::borrow::Cow::Owned(s)
    }

    /// Each track's effects onto every clip of it (#846), for a render
    /// copy: a picture's effects followed by its track's, its grade
    /// (inside its group's, when that is baked first) followed by its
    /// track's grade. A track's sound effects stay on the track, for the
    /// mixer to run on its summed clips (#2560). The clips
    /// keep what was their own (see [`Clip::own_effects`]), and a clip
    /// that already carries its track's is left, so baking twice is
    /// baking once. Gaps and adjustment layers take nothing: neither is a
    /// picture of the track, and an adjustment layer would put the
    /// track's effects on everything under it.
    pub fn bake_track_effects(&mut self) {
        for track in &mut self.tracks {
            if track.effects.is_empty() {
                continue;
            }
            let fx = &track.effects;
            let picture = fx.touches_picture() && matches!(track.kind, TrackKind::Video | TrackKind::Text);
            for clip in &mut track.clips {
                if clip.baked_own.is_some() || matches!(clip.kind, ClipKind::Gap | ClipKind::AdjustmentLayer) {
                    continue;
                }
                let own = listed_effects(clip);
                if picture {
                    let track_fx = fx.video_effects();
                    if !track_fx.is_empty() {
                        // The track's after the clip's own, through its
                        // picture (M4 step 12f), each with its track's flag.
                        let flags = fx.video_effects_enabled();
                        let mut edits: Vec<crate::EffectEdit> = track_fx.iter().map(crate::EffectEdit::Add).collect();
                        edits.extend((0..track_fx.len()).filter(|k| !flags.get(*k).copied().unwrap_or(true)).map(|k| crate::EffectEdit::SetEnabled(own + k, false)));
                        if let Some(Err(why)) = clip.apply_effect_edits(&edits) {
                            tracing::warn!("clip {:?}: its track's effects: {why}", clip.id);
                        }
                    }
                    if let Some(grade) = fx.grade_onto(track.kind, clip) {
                        if clip.picture.view().own_node_graph.is_none() {
                            clip.view_mut().own_node_graph = Some(Box::new(clip.picture.view().node_graph.0.clone()));
                        }
                        append_track_grade(&mut clip.view_mut().node_graph.0, track.id, grade);
                    }
                }
                // A track's sound effects are not the clips': they run on
                // the track's summed clips, before its fader, in the mixer
                // and the export alike (#2560).
                clip.baked_own = Some(BakedOwn { effects: own, audio: clip.audio_effects.len() });
            }
        }
    }

    /// The node of `clip_id`'s grade, as a render copy runs it (see
    /// [`Sequence::render_grade`]), that `path` names (M3 step 16a): what
    /// an inspection (a thumbnail, a tap, a solo) reads. A node of the
    /// clip's own grade group keeps its id; a group stage's or the track
    /// grade's has the id its bake gave it. Works on the sequence and on
    /// its render copy alike. `Err` says why the path names nothing to
    /// inspect, never a stand-in: a node inside a compound (no inspection
    /// there yet, #1475), the timeline grade (graded after the clips), a
    /// stage that does nothing here, or a node that isn't there.
    pub fn grade_node_at(&self, clip_id: ClipId, path: &klipp_graph_format::path::NodePath) -> Result<klipp_nodes::NodeId, String> {
        use klipp_graph_format::lifted::Stage;
        use klipp_graph_format::path::{Root, TrackPart};
        let (track, clip) = self
            .tracks
            .iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id).map(|c| (t, c)))
            .ok_or_else(|| format!("no clip {} on this sequence's tracks", clip_id.0))?;
        let own = clip.own_grade();
        let held = |g: &klipp_nodes::NodeGraph, id: klipp_nodes::NodeId| g.nodes.iter().any(|n| n.id == id);
        let group = clip.grade_group.and_then(|g| self.grade_group(g)).filter(|g| g.grades());
        let one = |nodes: &[klipp_nodes::NodeId]| match nodes {
            [inner] => Ok(*inner),
            [] => Err("an empty path names no node".to_string()),
            _ => Err("a node inside a compound is inspected only in a clip's own grade so far (#1475)".to_string()),
        };
        match path.root {
            Root::Clip { clip: c } if c == clip_id.0 => {
                let grade = klipp_graph_lift::ids::entry_id(klipp_graph_lift::ids::RootKind::Clip, c, "grade", 0);
                match path.nodes.split_first() {
                    // Through each compound it names to the node inside
                    // (#1475): the executor runs a compound's nodes under
                    // their own ids.
                    Some((first, rest)) if *first == grade => {
                        let (inner, through) = rest.split_last().ok_or("an empty path names no node")?;
                        let mut level = own;
                        for id in through {
                            level = level
                                .nodes
                                .iter()
                                .find(|n| n.id == *id && n.kind == klipp_nodes::NodeKind::Compound)
                                .and_then(|n| n.params.compound_subgraph.as_ref())
                                .ok_or_else(|| format!("{id} isn't a compound of the clip's grade"))?;
                        }
                        if held(level, *inner) { Ok(*inner) } else { Err(format!("{inner} isn't in the clip's grade")) }
                    }
                    Some((first, _)) => Err(format!("{first} isn't the clip's grade group; only grade nodes are inspected")),
                    None => Err("an empty path names no node".into()),
                }
            }
            Root::GroupStage { group: g, stage } if clip.grade_group.map(|id| id.0) == Some(g) => {
                let inner = one(&path.nodes)?;
                let group = group.ok_or("the clip's grade group grades nothing")?;
                let stage = if stage == Stage::Pre { GradeStage::Pre } else { GradeStage::Post };
                group.wrap_mapped(own).1.get(&(stage, inner)).copied().ok_or_else(|| format!("{inner} doesn't run in that stage for this clip"))
            }
            Root::Track { track: t, part: TrackPart::Grade } if t == track.id.0 => {
                let inner = one(&path.nodes)?;
                let grade = track.effects.grade_onto(track.kind, clip).ok_or("the track's grade doesn't run on this clip")?;
                let mut base = group.map_or_else(|| own.clone(), |g| g.wrap(own));
                // The track grade's Source is what it receives: the clip's
                // grade before it, ended by the node feeding its Output
                // (#1655).
                if grade.nodes.iter().any(|n| n.id == inner && n.kind == klipp_nodes::NodeKind::Source) {
                    return base.input_of(base.output_node_id, 0).ok_or_else(|| "the clip's grade gives the track's grade no picture".into());
                }
                append_track_grade(&mut base, track.id, grade).get(&inner).copied().ok_or_else(|| format!("{inner} doesn't run in the track's grade"))
            }
            Root::TimelineGrade { .. } => Err("the timeline grade runs after the clips, not in a clip's grade".into()),
            _ => Err("that path is in another clip's picture".into()),
        }
    }

    /// `clip_id`'s grade as a render copy runs it (see
    /// [`Sequence::baked_for_render`]): inside its grade group's when the
    /// group grades, then followed by its track's grade. Built for the one
    /// clip, for a preview that grades a single picture (the scopes), so
    /// it cannot drift from the bakes: each step is theirs (#1060). `None`
    /// when no top-level track holds the clip.
    pub fn render_grade(&self, clip_id: ClipId) -> Option<std::borrow::Cow<'_, klipp_nodes::NodeGraph>> {
        let (track, clip) = self.tracks.iter().find_map(|t| t.clips.iter().find(|c| c.id == clip_id).map(|c| (t, c)))?;
        let mut graph = match clip.grade_group.and_then(|g| self.grade_group(g)).filter(|g| g.grades()) {
            Some(group) => std::borrow::Cow::Owned(group.wrap(&clip.picture.view().node_graph.0)),
            None => std::borrow::Cow::Borrowed(&clip.picture.view().node_graph.0),
        };
        if let Some(grade) = track.effects.grade_onto(track.kind, clip) {
            append_track_grade(graph.to_mut(), track.id, grade);
        }
        Some(graph)
    }

    /// This sequence as the document has it, from a render copy: each
    /// clip's own grade in place of its grade inside its group's (see
    /// [`Sequence::bake_grade_groups`]), and its own effects and audio
    /// chain in place of those with its track's (see
    /// [`Sequence::bake_track_effects`]), each effect under the node id
    /// the document has for it. A copy only when there is something to
    /// take back out.
    pub fn with_own_grades(&self) -> std::borrow::Cow<'_, Sequence> {
        if !self.all_clips().any(|c| c.picture.view().own_node_graph.is_some() || c.baked_own.is_some()) {
            return std::borrow::Cow::Borrowed(self);
        }
        let mut seq = self.clone();
        for clip in seq.all_clips_mut() {
            let own = clip.view_mut().own_node_graph.take();
            if let Some(own) = own {
                clip.view_mut().node_graph = crate::Grade(*own);
            }
            // The track's effects taken back off the end, through the
            // clip's picture: its own keep their node ids.
            if let Some(own) = clip.baked_own.take() {
                let edits: Vec<crate::EffectEdit> = (own.effects..listed_effects(clip)).rev().map(crate::EffectEdit::Remove).collect();
                if let Some(Err(why)) = clip.apply_effect_edits(&edits) {
                    tracing::warn!("clip {:?}: its track's effects back off: {why}", clip.id);
                }
                clip.audio_effects.truncate(own.audio);
            }
        }
        std::borrow::Cow::Owned(seq)
    }

    /// The id the timeline grade runs under as a clip (see
    /// [`Clip::timeline_grade_of`]): fixed for the sequence, and never a
    /// real clip's.
    /// The clip a track's grade is edited through in the Colour workspace
    /// (#1408): fixed for the track, as a stage's is for its group.
    pub fn track_grade_clip_id(track: TrackId) -> ClipId {
        const TRACK: u128 = 0x7472_6163_6b2d_6772_6164_652d_7374_0001;
        ClipId(uuid::Uuid::from_u128(track.0.as_u128() ^ TRACK))
    }

    /// The video track whose grade `clip_id` stands in for, if it does.
    pub fn track_grade_of(&self, clip_id: ClipId) -> Option<TrackId> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Video).map(|t| t.id).find(|t| Self::track_grade_clip_id(*t) == clip_id)
    }

    pub fn timeline_grade_clip_id(&self) -> ClipId {
        const TIMELINE: u128 = 0x7469_6d65_6c69_6e65_6772_6164_6521_0001;
        ClipId(uuid::Uuid::from_u128(self.id.0.as_u128() ^ TIMELINE))
    }

    /// Whether a sequence at `fps` counts drop-frame unless told
    /// otherwise: yes at 29.97 and 59.94, no everywhere else.
    ///
    /// The NTSC rates run 1000/1001 of their nominal speed, so a
    /// non-drop count falls behind the wall clock by about 3.6 s an
    /// hour — over a feature that is more than seven seconds, and a
    /// broadcast slot is exact. Drop-frame is the convention for those
    /// rates for that reason, so it is where a sequence starts; a
    /// delivery that wants 29.97 non-drop turns it off.
    pub fn default_drop_frame(fps: Fps) -> bool {
        klipp_core::sync::is_drop_frame_rate(fps)
    }

    /// How positions on this sequence are shown and read back.
    pub fn timecode_display(&self) -> klipp_core::TimecodeDisplay {
        klipp_core::TimecodeDisplay { fps: self.fps, drop: self.drop_frame }
    }

    pub fn new_default() -> Self {
        Self {
            id: SequenceId::new(),
            camera: Camera::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            drop_frame: Self::default_drop_frame(Fps::film_24()),
            title_nits: DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            buses: Vec::new(),
            name: "Main Sequence".to_string(),
            fps: Fps::film_24(),
            resolution: Resolution::hd_1080(),
            first_clip_autoset: FirstClipAutoSet::default(),
            tracks: {
                let mut tracks = vec![
                    Track::new("Video 1", TrackKind::Video),
                    Track::new("Audio 1", TrackKind::Audio),
                ];
                tracks[0].magnetic_role = TrackRole::Primary;
                tracks[1].magnetic_role = TrackRole::Connected;
                tracks
            },
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
            awaiting_refit: false,
            version_of: None,
        }
    }

    pub fn total_duration(&self) -> Timecode {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.last())
            .map(|c| c.timeline_out)
            .max()
            .unwrap_or(Timecode::zero())
    }

    /// Renumber video and audio tracks based on visual position.
    /// Video tracks are numbered bottom-up (last video in array = "Video 1").
    /// Audio tracks are numbered top-down (first audio = "Audio 1").
    /// Text/Subtitle tracks are numbered top-down.
    /// Only renames tracks that have default-pattern names ("Video N", "Audio N", etc.).
    /// User-customized names (e.g., "B-Roll", "Interview") are left unchanged.
    /// Number the tracks' default names in order again ("Video 1",
    /// "Audio 2"), leaving every name the user gave as it is.
    pub fn renumber_tracks(&mut self) {
        // Video: bottom-up (reverse order among video tracks)
        let video_indices: Vec<usize> = self.tracks.iter().enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Video)
            .map(|(i, _)| i)
            .collect();
        for (num_from_bottom, &idx) in video_indices.iter().rev().enumerate() {
            let track = &mut self.tracks[idx];
            // Only rename if it has a default-pattern name
            if is_default_track_name(&track.name, "Video", 'V') {
                track.name = renumbered(&track.name, "Video", 'V', num_from_bottom as u32 + 1);
            }
        }
        // Audio: top-down
        let mut audio_num = 1u32;
        for track in &mut self.tracks {
            if track.kind == TrackKind::Audio {
                if is_default_track_name(&track.name, "Audio", 'A') {
                    track.name = renumbered(&track.name, "Audio", 'A', audio_num);
                }
                audio_num += 1;
            }
        }
        // Text: top-down
        let mut text_num = 1u32;
        for track in &mut self.tracks {
            if track.kind == TrackKind::Text {
                if is_default_track_name(&track.name, "Text", 'T') {
                    track.name = renumbered(&track.name, "Text", 'T', text_num);
                }
                text_num += 1;
            }
        }
    }

    /// Returns all overlay video/text/subtitle tracks — every track that composites
    /// over the primary video track. The primary is identified by `primary_track_idx`
    /// (the engine's `current_track_index`). All other unmuted video/text/subtitle
    /// tracks are returned as overlays.
    pub fn overlay_video_tracks(&self, primary_track_idx: usize) -> Vec<(usize, &Track)> {
        self.tracks.iter().enumerate()
            .filter(|(i, t)| {
                !t.muted && match t.kind {
                    TrackKind::Text | TrackKind::Subtitle => true,
                    TrackKind::Video => *i != primary_track_idx,
                    _ => false,
                }
            })
            .collect()
    }

    pub fn audio_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == TrackKind::Audio)
    }

    /// Add a new video track at the top of the video track group (above all existing
    /// video tracks). This makes it the highest-numbered overlay. V1 stays at the bottom.
    /// Renumbers all video tracks after insertion.
    pub fn add_video_track(&mut self, name: String) -> TrackId {
        let track = Track::new(&name, TrackKind::Video);
        let id = track.id;
        // Insert at index 0 — above everything, new track becomes highest V number
        self.tracks.insert(0, track);
        self.renumber_tracks();
        id
    }

    /// Add a new audio track at the end.
    pub fn add_audio_track(&mut self, name: String) -> TrackId {
        let track = Track::new(&name, TrackKind::Audio);
        let id = track.id;
        self.tracks.push(track);
        id
    }

    /// Add a new text track. Inserts before the first Audio track (same region as video).
    pub fn add_text_track(&mut self, name: String) -> TrackId {
        let track = Track::new(&name, TrackKind::Text);
        let id = track.id;
        let insert_pos = self.tracks.iter()
            .position(|t| t.kind == TrackKind::Audio)
            .unwrap_or(self.tracks.len());
        self.tracks.insert(insert_pos, track);
        id
    }

    /// Add a new subtitle track at the end (after audio tracks).
    pub fn add_subtitle_track(&mut self, name: String) -> TrackId {
        let track = Track::new(&name, TrackKind::Subtitle);
        let id = track.id;
        self.tracks.push(track);
        id
    }

    /// Remove a track. Returns Err if track has clips.
    pub fn remove_track(&mut self, track_id: TrackId) -> Result<(), &'static str> {
        let pos = self.tracks.iter().position(|t| t.id == track_id)
            .ok_or("track not found")?;
        if !self.tracks[pos].clips.is_empty() {
            return Err("track has clips");
        }
        self.tracks.remove(pos);
        Ok(())
    }

    /// Remove a track including all its clips.
    /// Also clears linked_clip_id on clips in other tracks that pointed to removed clips.
    pub fn remove_track_with_clips(&mut self, track_id: TrackId) -> Result<(), &'static str> {
        let pos = self.tracks.iter().position(|t| t.id == track_id)
            .ok_or("track not found")?;
        // Collect IDs of clips being removed so we can unlink them
        let removed_ids: Vec<ClipId> = self.tracks[pos].clips.iter().map(|c| c.id).collect();
        self.tracks.remove(pos);
        self.purge_dangling_linked_clip_ids(&removed_ids);
        Ok(())
    }

    /// Clear `linked_clip_id` on every clip whose link target is in
    /// `removed`. Use this from any code path that removes clips
    /// permanently (track removal, ripple_delete) so other clips on
    /// surviving tracks don't keep dangling references.
    ///
    /// The asymmetric-link case (third-party
    /// clip C points at deleted clip A while A's own linked_clip_id
    /// points at unrelated clip B) used to leak dangling refs from
    /// ripple_delete. Centralising the sweep here means every clip
    /// removal path sees the same cleanup.
    pub fn purge_dangling_linked_clip_ids(&mut self, removed: &[ClipId]) {
        if removed.is_empty() {
            return;
        }
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                if let Some(linked) = clip.linked_clip_id {
                    if removed.contains(&linked) {
                        clip.linked_clip_id = None;
                    }
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // Magnetic Timeline Helpers
    // ═══════════════════════════════════════════════════════════════════

    /// The track a source edit lays `kind` on (#915): the one patched for
    /// it, else the first track of the kind.
    pub fn source_track(&self, kind: TrackKind) -> Option<usize> {
        self.tracks
            .iter()
            .position(|t| t.kind == kind && t.source_patch)
            .or_else(|| self.tracks.iter().position(|t| t.kind == kind))
    }

    /// Patch the source to `track_index`, taking the patch off every
    /// other track of its kind.
    pub fn patch_source_to(&mut self, track_index: usize) {
        let Some(kind) = self.tracks.get(track_index).map(|t| t.kind) else { return };
        for (i, t) in self.tracks.iter_mut().enumerate() {
            if t.kind == kind {
                t.source_patch = i == track_index;
            }
        }
    }

    /// The audio track a sound-only source is laid on from `from` to `to`
    /// (#2940): the one the source is patched to, chosen on purpose; else
    /// the first unlocked audio track free over the span, of the targeted
    /// ones when only some are. `None` when none is free: a new track
    /// takes it, and no sound already there is written over.
    pub fn sound_source_track(&self, from: Timecode, to: Timecode) -> Option<usize> {
        let audio = |t: &Track| t.kind == TrackKind::Audio;
        if let Some(i) = self.tracks.iter().position(|t| audio(t) && t.source_patch) {
            return Some(i);
        }
        let narrowed = self.tracks.iter().filter(|t| audio(t)).any(|t| t.targeted) && self.tracks.iter().filter(|t| audio(t)).any(|t| !t.targeted);
        let free = |t: &Track| !t.clips.iter().any(|c| !c.kind.is_gap() && c.timeline_in < to && from < c.timeline_out);
        self.tracks.iter().position(|t| audio(t) && !t.locked && (!narrowed || t.targeted) && free(t))
    }

    /// [`Self::sound_source_track`], or a new audio track at the bottom
    /// when none is free: where a sound-only source edit lays its clip.
    pub fn sound_source_track_or_new(&mut self, from: Timecode, to: Timecode) -> usize {
        if let Some(i) = self.sound_source_track(from, to) {
            return i;
        }
        let n = self.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count();
        self.add_audio_track(format!("Audio {}", n + 1));
        self.tracks.len() - 1
    }

    /// A click on `track_index`'s badge (#2940): it becomes where source
    /// edits of its kind go. While every track of the kind is targeted
    /// (none chosen), it is targeted alone of them; otherwise it is
    /// targeted, or, targeted already, untargeted. A picture or sound
    /// track targeted this way has the source patched to it, and loses
    /// the patch when untargeted.
    pub fn target_from_badge(&mut self, track_index: usize) {
        let Some(kind) = self.tracks.get(track_index).map(|t| t.kind) else { return };
        let of_kind = self.tracks.iter().filter(|t| t.kind == kind).count();
        let all = self.tracks.iter().filter(|t| t.kind == kind).all(|t| t.targeted);
        let patches = matches!(kind, TrackKind::Video | TrackKind::Audio);
        if self.tracks[track_index].targeted && (!all || of_kind == 1) {
            self.tracks[track_index].targeted = false;
            self.tracks[track_index].source_patch = false;
            return;
        }
        for (i, t) in self.tracks.iter_mut().enumerate() {
            if t.kind == kind && (all || i == track_index) {
                t.targeted = i == track_index;
            }
        }
        if patches {
            self.patch_source_to(track_index);
        }
    }

    /// Whether track targeting lets edits act on `track_index` (#915):
    /// it is targeted, or no track is.
    pub fn is_targeted(&self, track_index: usize) -> bool {
        self.tracks.get(track_index).is_some_and(|t| t.targeted) || !self.tracks.iter().any(|t| t.targeted)
    }

    /// Find the index of the Primary track. Returns None if no primary track set.
    pub fn primary_track_index(&self) -> Option<usize> {
        self.tracks.iter().position(|t| t.magnetic_role == TrackRole::Primary)
    }

    /// Find the single Primary track.
    pub fn primary_track(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.magnetic_role == TrackRole::Primary)
    }

    /// Find the single Primary track mutably.
    pub fn primary_track_mut(&mut self) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.magnetic_role == TrackRole::Primary)
    }

    /// Find a clip by id across all tracks. Returns (track_index, clip_index).
    pub fn find_clip(&self, id: ClipId) -> Option<(usize, usize)> {
        for (ti, track) in self.tracks.iter().enumerate() {
            for (ci, clip) in track.clips.iter().enumerate() {
                if clip.id == id {
                    return Some((ti, ci));
                }
            }
        }
        None
    }

    /// Set a clip's absolute position directly (used by undo/redo restore).
    pub fn set_clip_position(&mut self, id: ClipId, new_in: RationalTime, new_out: RationalTime) {
        if let Some((ti, ci)) = self.find_clip(id) {
            self.tracks[ti].clips[ci].timeline_in = new_in;
            self.tracks[ti].clips[ci].timeline_out = new_out;
        }
    }

    /// Attempt to swap two adjacent clips on the same track. Returns
    /// `Ok(SwapResult)` with the captured pre-swap state when the
    /// swap succeeds, or `Err(reason)` listing the validation
    /// failure. The caller is responsible for toast display, history
    /// push, and engine re-sync.
    ///
    /// Validation rules:
    /// 1. Both clips exist; aren't the same id.
    /// 2. Both clips are on the same track; the track isn't locked.
    /// 3. The clips are adjacent within `1/fps_rate` seconds.
    /// 4. Neither shares a group with the other.
    /// 5. Linked-partner symmetry: either both have a linked partner
    ///    or neither does. Asymmetric → reject (would orphan the lone
    ///    partner).
    /// 6. When both have partners, those partners are themselves
    ///    adjacent on the same track in matching left/right order.
    ///
    /// On success, both primaries' positions exchange (preserve-span
    /// semantics: right takes left's start, left starts at
    /// `left.start + right.duration`), and the linked-partner pair
    /// is swapped on its track too. Returns the pre-swap positions
    /// for history capture, including partner positions when those
    /// were also swapped.
    pub fn try_swap_adjacent_clips(
        &mut self,
        clip_a: ClipId,
        clip_b: ClipId,
    ) -> Result<SwapResult, &'static str> {
        if clip_a == clip_b {
            return Err("Swap: clips must be distinct");
        }

        let fps_rate = self.fps.rate();
        let epsilon_secs = 1.0 / fps_rate as f64;

        // Snapshot the inputs.
        let mut a_track = None;
        let mut b_track = None;
        let mut a_in = RationalTime { value: 0, rate: 1 };
        let mut a_out = a_in;
        let mut b_in = a_in;
        let mut b_out = a_in;
        let mut a_group = None;
        let mut b_group = None;
        let mut a_linked = None;
        let mut b_linked = None;

        for (idx, track) in self.tracks.iter().enumerate() {
            for clip in &track.clips {
                if clip.id == clip_a {
                    a_track = Some(idx);
                    a_in = clip.timeline_in;
                    a_out = clip.timeline_out;
                    a_group = clip.group_id;
                    a_linked = clip.linked_clip_id;
                } else if clip.id == clip_b {
                    b_track = Some(idx);
                    b_in = clip.timeline_in;
                    b_out = clip.timeline_out;
                    b_group = clip.group_id;
                    b_linked = clip.linked_clip_id;
                }
            }
        }

        let (a_idx, b_idx) = match (a_track, b_track) {
            (Some(a), Some(b)) => (a, b),
            _ => return Err("Swap: one or both clips not found"),
        };
        if a_idx != b_idx {
            return Err("Swap: both clips must be on the same track");
        }
        if self.tracks[a_idx].locked {
            return Err("Swap: track is locked");
        }

        let (left_id, left_in, left_out, right_id, right_in, right_out) =
            if a_in.to_secs_f64() <= b_in.to_secs_f64() {
                (clip_a, a_in, a_out, clip_b, b_in, b_out)
            } else {
                (clip_b, b_in, b_out, clip_a, a_in, a_out)
            };
        let gap = right_in.to_secs_f64() - left_out.to_secs_f64();
        if gap.abs() > epsilon_secs {
            return Err("Swap: clips must be adjacent (no gap, no overlap)");
        }

        if let (Some(ga), Some(gb)) = (a_group, b_group) {
            if ga == gb {
                return Err("Swap: clips share a group — ungroup them first");
            }
        }

        if a_linked.is_some() != b_linked.is_some() {
            return Err("Swap: linked clips must come in matched pairs");
        }

        // Validate + capture partner state when both have partners.
        let mut partners_info: Option<(ClipId, RationalTime, RationalTime, ClipId, RationalTime, RationalTime, usize)> = None;
        if let (Some(la_id), Some(lb_id)) = (a_linked, b_linked) {
            let mut la = None;
            let mut lb = None;
            let mut la_track = None;
            let mut lb_track = None;
            for (idx, track) in self.tracks.iter().enumerate() {
                for clip in &track.clips {
                    if clip.id == la_id {
                        la = Some((clip.timeline_in, clip.timeline_out));
                        la_track = Some(idx);
                    } else if clip.id == lb_id {
                        lb = Some((clip.timeline_in, clip.timeline_out));
                        lb_track = Some(idx);
                    }
                }
            }
            match (la, lb, la_track, lb_track) {
                (Some((la_in, la_out)), Some((lb_in, lb_out)), Some(lat), Some(lbt)) if lat == lbt => {
                    if self.tracks[lat].locked {
                        return Err("Swap: linked-partner track is locked");
                    }
                    let (l_left_in, l_left_out, l_right_in, _l_right_out) =
                        if la_in.to_secs_f64() <= lb_in.to_secs_f64() {
                            (la_in, la_out, lb_in, lb_out)
                        } else {
                            (lb_in, lb_out, la_in, la_out)
                        };
                    let _ = l_left_in;
                    let l_gap = l_right_in.to_secs_f64() - l_left_out.to_secs_f64();
                    if l_gap.abs() > epsilon_secs {
                        return Err("Swap: linked partners must also be adjacent");
                    }
                    let left_primary_partner = if left_id == clip_a { la_id } else { lb_id };
                    let l_left_id = if la_in.to_secs_f64() <= lb_in.to_secs_f64() { la_id } else { lb_id };
                    if left_primary_partner != l_left_id {
                        return Err("Swap: linked partners must be in the same order as the primaries");
                    }
                    partners_info = Some((la_id, la_in, la_out, lb_id, lb_in, lb_out, lat));
                }
                _ => {
                    return Err("Swap: linked partners must both exist on the same track");
                }
            }
        }

        let _ = right_out;
        // Apply primary swap.
        let track = &mut self.tracks[a_idx];
        let mut a_orig_in = RationalTime { value: 0, rate: 1 };
        let mut a_orig_out = a_orig_in;
        let mut b_orig_in = a_orig_in;
        let mut b_orig_out = a_orig_in;
        let mut left_dur = a_orig_in;
        let mut right_dur = a_orig_in;
        for clip in &track.clips {
            if clip.id == left_id {
                left_dur = clip.timeline_out - clip.timeline_in;
            } else if clip.id == right_id {
                right_dur = clip.timeline_out - clip.timeline_in;
            }
        }
        let new_right_in = left_in;
        let new_right_out = left_in + right_dur;
        let new_left_in = new_right_out;
        let new_left_out = new_left_in + left_dur;
        for clip in track.clips.iter_mut() {
            if clip.id == left_id {
                a_orig_in = clip.timeline_in;
                a_orig_out = clip.timeline_out;
                clip.timeline_in = new_left_in;
                clip.timeline_out = new_left_out;
            } else if clip.id == right_id {
                b_orig_in = clip.timeline_in;
                b_orig_out = clip.timeline_out;
                clip.timeline_in = new_right_in;
                clip.timeline_out = new_right_out;
            }
        }
        track.sort_clips();

        // Apply linked-partner swap.
        let linked_partners = if let Some((la_id, la_in_old, la_out_old, lb_id, lb_in_old, lb_out_old, lat)) = partners_info {
            let (l_left_id, l_left_in, l_left_out, l_right_id, _l_right_in, l_right_out) =
                if la_in_old.to_secs_f64() <= lb_in_old.to_secs_f64() {
                    (la_id, la_in_old, la_out_old, lb_id, lb_in_old, lb_out_old)
                } else {
                    (lb_id, lb_in_old, lb_out_old, la_id, la_in_old, la_out_old)
                };
            let l_left_dur = l_left_out - l_left_in;
            let l_right_dur = l_right_out - l_left_out;
            let new_l_right_in = l_left_in;
            let new_l_right_out = l_left_in + l_right_dur;
            let new_l_left_in = new_l_right_out;
            let new_l_left_out = new_l_left_in + l_left_dur;
            let partner_track = &mut self.tracks[lat];
            for clip in partner_track.clips.iter_mut() {
                if clip.id == l_left_id {
                    clip.timeline_in = new_l_left_in;
                    clip.timeline_out = new_l_left_out;
                } else if clip.id == l_right_id {
                    clip.timeline_in = new_l_right_in;
                    clip.timeline_out = new_l_right_out;
                }
            }
            partner_track.sort_clips();
            Some(SwappedPartners {
                a_partner: (la_id, la_in_old, la_out_old),
                b_partner: (lb_id, lb_in_old, lb_out_old),
            })
        } else {
            None
        };

        // Map captured originals back to the user's clip_a / clip_b
        // identity, regardless of which one ended up "left".
        let (a_old_in_final, a_old_out_final, b_old_in_final, b_old_out_final) =
            if left_id == clip_a {
                (a_orig_in, a_orig_out, b_orig_in, b_orig_out)
            } else {
                (b_orig_in, b_orig_out, a_orig_in, a_orig_out)
            };

        Ok(SwapResult {
            a_old_in: a_old_in_final,
            a_old_out: a_old_out_final,
            b_old_in: b_old_in_final,
            b_old_out: b_old_out_final,
            partners: linked_partners,
        })
    }
}

/// Pre-swap positions captured for history. Returned by
/// [`Sequence::try_swap_adjacent_clips`] so the caller can push a
/// history entry that exactly restores the pre-swap state on undo.
#[derive(Debug, Clone)]
pub struct SwapResult {
    pub a_old_in: RationalTime,
    pub a_old_out: RationalTime,
    pub b_old_in: RationalTime,
    pub b_old_out: RationalTime,
    pub partners: Option<SwappedPartners>,
}

#[derive(Debug, Clone)]
pub struct SwappedPartners {
    pub a_partner: (ClipId, RationalTime, RationalTime),
    pub b_partner: (ClipId, RationalTime, RationalTime),
}

impl Sequence {

    /// Iterator over all clips in all tracks.
    pub fn all_clips(&self) -> impl Iterator<Item = &Clip> {
        self.tracks.iter().flat_map(|t| t.clips.iter())
    }

    /// Mutable iterator over all clips in all tracks.
    pub fn all_clips_mut(&mut self) -> impl Iterator<Item = &mut Clip> {
        self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut())
    }

    /// The gap clips the primary track needs to hold no holes (#3060): one
    /// for each stretch between its start and its last clip that no clip
    /// covers, with the primary's index. The primary is gapless by rule;
    /// an edit that leaves a hole there (a place, a move, a removal or a
    /// trim that doesn't ripple) fills it with a gap clip, so no later
    /// magnetic edit packs the track to close it.
    pub fn primary_holes(&self) -> Option<(usize, Vec<Clip>)> {
        let p = self.primary_track_index()?;
        let mut spans: Vec<(RationalTime, RationalTime)> = self.tracks[p].clips.iter().map(|c| (c.timeline_in, c.timeline_out)).collect();
        spans.sort_by_key(|s| s.0);
        let mut reached = RationalTime::ZERO;
        let mut holes = Vec::new();
        for (a, b) in spans {
            if a.cmp(&reached).is_gt() {
                // In the clip's rate: from a zero of rate 1 the gap's end
                // would round to a whole second.
                holes.push(Clip::gap(reached.rescale_to(a.rate), a));
            }
            if b.cmp(&reached).is_gt() {
                reached = b;
            }
        }
        (!holes.is_empty()).then_some((p, holes))
    }

    /// Clips that run over the next one on their track, ended before it
    /// (#3230): an append once ended a clip at its file's length, off the
    /// sequence's frames and over the next clip's first frame. The end goes
    /// back to the last whole frame at or before it, or to the next clip's
    /// start if that is earlier, and a clip linked to it that ended with
    /// it goes too. The source is cut by as much as the clip plays of it.
    /// How many clips were mended.
    pub fn mend_overlaps(&mut self) -> usize {
        let fps = self.fps;
        let mut ends: Vec<(ClipId, Timecode, Timecode)> = Vec::new();
        for t in &self.tracks {
            let mut order: Vec<&Clip> = t.clips.iter().collect();
            order.sort_by_key(|c| c.timeline_in);
            for w in order.windows(2) {
                let (a, b) = (w[0], w[1]);
                if a.timeline_out <= b.timeline_in || b.timeline_in <= a.timeline_in {
                    continue;
                }
                let on_frame = klipp_core::whole_frames_within(a.timeline_out.to_secs_f64(), fps);
                let end = on_frame.min(b.timeline_in);
                if end > a.timeline_in {
                    ends.push((a.id, a.timeline_out, end));
                }
            }
        }
        let partners: Vec<(ClipId, Timecode, Timecode)> = ends
            .iter()
            .filter_map(|(id, old, new)| {
                let (ti, ci) = self.find_clip(*id)?;
                let lid = self.tracks[ti].clips[ci].linked_clip_id?;
                let (pt, pc) = self.find_clip(lid)?;
                let p = &self.tracks[pt].clips[pc];
                (p.timeline_out.cmp(old).is_eq() && *new > p.timeline_in && !ends.iter().any(|(e, _, _)| *e == lid)).then_some((lid, *old, *new))
            })
            .collect();
        let n = ends.len() + partners.len();
        for (id, old, new) in ends.into_iter().chain(partners) {
            let Some((ti, ci)) = self.find_clip(id) else { continue };
            let c = &mut self.tracks[ti].clips[ci];
            let cut = RationalTime::from_secs_f64((old - new).to_secs_f64() * c.speed.abs(), c.source_out.rate.max(1));
            c.timeline_out = new;
            if c.speed < 0.0 {
                c.source_in += cut;
            } else {
                c.source_out -= cut;
            }
        }
        n
    }

    /// [`Self::primary_holes`] filled with gap clips; how many.
    pub fn fill_primary_holes(&mut self) -> usize {
        let Some((p, gaps)) = self.primary_holes() else { return 0 };
        let n = gaps.len();
        self.tracks[p].clips.extend(gaps);
        self.tracks[p].sort_clips();
        n
    }

    /// Enforce primary invariant: primary track has no gaps.
    /// Call this after every magnetic edit as a debug assertion.
    #[cfg(debug_assertions)]
    pub fn assert_primary_gapless(&self) {
        if let Some(primary) = self.primary_track() {
            // Compared exactly: rescaled to the rate of a zero start, a
            // gap under a second went unseen.
            let mut expected = RationalTime::ZERO;
            for clip in &primary.clips {
                assert!(
                    clip.timeline_in.cmp(&expected).is_eq(),
                    "Primary storyline gap detected before clip {:?} (expected {:?}, got {:?})",
                    clip.id, expected, clip.timeline_in
                );
                expected = clip.timeline_out;
            }
        }
    }

    /// Generate the next auto-increment name for a track kind (e.g. "Video 3", "Audio 2").
    pub fn next_track_name(&self, kind: TrackKind) -> String {
        let prefix = match kind {
            TrackKind::Video => "Video",
            TrackKind::Audio => "Audio",
            TrackKind::Text => "Text",
            TrackKind::Subtitle => "Subtitle",
        };
        // Find the highest existing number for this kind to avoid name collisions
        // after track deletion (e.g., deleting "Video 2" from [1,2,3] then adding → "Video 4").
        let highest = self.tracks.iter()
            .filter(|t| t.kind == kind)
            .filter_map(|t| t.name.strip_prefix(prefix).and_then(|s| s.trim().parse::<u32>().ok()))
            .max()
            .unwrap_or(0);
        format!("{} {}", prefix, highest + 1)
    }
}

/// A subtitle track's caption look (#70): what every caption on it is
/// styled to. The default is the look captions are made with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptionStyle {
    /// The face; empty for the default.
    #[serde(default)]
    pub font: String,
    /// In pixels of a 1080-line frame.
    pub size: f32,
    pub color: [u8; 4],
    #[serde(default)]
    pub bold: bool,
    /// Outline width in pixels, 0 for none.
    pub outline: f32,
    pub outline_color: [u8; 4],
    /// How dark the panel behind the text is, 0 (none) to 1 (black).
    pub backing: f32,
    /// A soft drop shadow behind the text.
    #[serde(default)]
    pub shadow: bool,
    /// The text's centre, down the frame, 0 (top) to 1 (bottom).
    pub y: f32,
}

impl Default for CaptionStyle {
    fn default() -> Self {
        Self { font: String::new(), size: 40.0, color: [255, 255, 255, 255], bold: false, outline: 2.0, outline_color: [0, 0, 0, 200], backing: 0.92, shadow: false, y: 0.88 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
    pub transitions: Vec<Transition>,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub locked: bool,
    /// Frozen (#3207): played from frames drawn ahead into the project's
    /// cache, where they are current, instead of drawn live.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub frozen: bool,
    /// Sync lock (#915): a ripple on another track moves this one's clips
    /// by the same amount, so what is above or below the edit stays in
    /// sync with it.
    #[serde(default)]
    pub sync_lock: bool,
    /// Track targeting (#915): whether edit-point navigation, paste,
    /// split at playhead and match frame act on this track. On for every
    /// track by default; with none on, they act on all of them.
    #[serde(default = "default_true")]
    pub targeted: bool,
    /// Source patching (#915): a source edit (insert, overwrite, append)
    /// lays its picture, or its sound, on this track. At most one track
    /// of each kind has it; with none, the first track of the kind does.
    #[serde(default)]
    pub source_patch: bool,
    /// The look of the captions on this subtitle track (#70): new
    /// captions made on it take it, and it can be put on every caption
    /// already there. `None` keeps the default caption look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_style: Option<CaptionStyle>,
    /// The language of a subtitle track's captions (#2223), a BCP 47 tag
    /// such as `de`: what an export tags the file with. `None` when not
    /// known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Magnetic role of this track. Defaults to Free — backward compatible.
    #[serde(default)]
    pub magnetic_role: TrackRole,
    /// If this track is a Connected Storyline, its anchor on the primary.
    #[serde(default)]
    pub storyline_anchor: Option<MagneticAnchor>,
    /// Custom track height in pixels. None = use default for track kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_height: Option<f32>,
    /// The track's colour, on the media pool's colour label scale; 0 is
    /// none, and the header's colour is then guessed from its name (#922).
    #[serde(default)]
    pub colour_label: u8,
    /// An audio track draws each clip's channels apart, left above right
    /// (#922).
    #[serde(default)]
    pub channel_waveforms: bool,
    /// Stored height before collapse, so double-click can restore it.
    #[serde(skip)]
    pub pre_collapse_height: Option<f32>,
    /// A runtime track carrying a precomposition's sound, made by
    /// flattening and never saved: the scene's audio-bearing clips on the
    /// precomposition's window, with the scene track's and the containing
    /// track's controls composed into this one's. The exporter takes a
    /// precomposition's sound from these and nowhere else.
    #[serde(skip)]
    pub precomposition_sound: bool,
    /// Source file (for subtitle tracks imported from SRT/VTT/ASS/SSA/TTML/SCC/STL).
    /// Set when a track is created via subtitle file import; displayed in
    /// the subtitle editor as a "Source: foo.srt" label.
    #[serde(default)]
    pub source_file: Option<std::path::PathBuf>,
    /// True when this audio track is "record-armed" — the Record button in
    /// the transport bar will capture to this track on stop.
    /// Only one track is armed at a time in the MVP (see #60 tier 2b).
    #[serde(default)]
    pub record_armed: bool,
    /// Per-track expandable automation lane state. `None` = collapsed (the
    /// inline rubber band on each clip is the only surface). `Some(param)`
    /// = the lane is visible below the clip area, showing the named
    /// param's curve at full lane height. Survives project reload via
    /// serde; defaults to `None` for back-compat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_lane_visible: Option<klipp_core::AudioParam>,
    /// The open lane shows the track's own keys of its param (#2306), its
    /// [`TrackAutomation`](crate::TrackAutomation), not the clips'.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub automation_lane_track: bool,
    /// User-overridden lane height in pixels. `None` falls back to the
    /// `AUTOMATION_LANE_HEIGHT` default (120 px) so projects saved
    /// before drag-resize landed still render at the original height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_lane_height: Option<f32>,
    /// What the track does to everything on it (#846): effects and a grade
    /// on every picture, an audio chain on every sound, after each clip's
    /// own. Set once, they hold for clips added later too.
    #[serde(default, skip_serializing_if = "TrackEffects::is_empty")]
    pub effects: TrackEffects,
    /// The track's own automation (#2306): its mode and its keys for its
    /// volume and pan, in sequence seconds.
    #[serde(default, skip_serializing_if = "crate::track_automation::TrackAutomation::is_default")]
    pub automation: crate::track_automation::TrackAutomation,
    /// Its sends to aux buses (#2861); see [`crate::routing`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<crate::AuxSend>,
}

/// A track's own effects (#846), applied to every clip on it after the
/// clip's own: each clip of a render copy carries them (see
/// [`Sequence::bake_track_effects`]), so the viewer, playback and export
/// apply them the one way they apply a clip's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackEffects {
    /// Effects on every picture of the track, after each clip's own.
    #[serde(flatten)]
    pub video_effect_list: crate::TrackEffectList,
    /// A grade on every picture of the track, after each clip's own grade
    /// and its grade group's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<crate::Grade>,
    /// An audio chain on every sound of the track, after each clip's own
    /// chain. Each effect carries its own bypass.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<AudioEffectInstance>,
}

impl TrackEffects {
    /// Whether the track changes nothing.
    pub fn is_empty(&self) -> bool {
        self.video_effects().is_empty() && self.track_grade().is_none_or(|g| g.is_identity()) && self.audio.is_empty()
    }

    /// Whether the track changes anything about a picture.
    pub fn touches_picture(&self) -> bool {
        !self.video_effects().is_empty() || self.track_grade().is_some_and(|g| !g.is_identity())
    }

    /// The grade a render copy appends to `clip`'s on a track of `kind`
    /// (see [`Sequence::bake_track_effects`]): a grade that changes
    /// something, on a picture track, for a clip that is a picture of it
    /// (not a gap or an adjustment layer).
    pub fn grade_onto(&self, kind: TrackKind, clip: &Clip) -> Option<&klipp_nodes::NodeGraph> {
        if !matches!(kind, TrackKind::Video | TrackKind::Text) || matches!(clip.kind, ClipKind::Gap | ClipKind::AdjustmentLayer) {
            return None;
        }
        self.track_grade().filter(|g| !g.is_identity())
    }
}

/// A track's grade after a clip's in `graph`: its nodes keep their ids
/// unless `graph` holds one already, which takes
/// [`klipp_nodes::graph::spliced_id`]'s by the track's id (M3 step 16a).
/// Where each of its nodes is in `graph`, by its own id.
pub fn append_track_grade(graph: &mut klipp_nodes::NodeGraph, track: TrackId, grade: &klipp_nodes::NodeGraph) -> HashMap<klipp_nodes::NodeId, klipp_nodes::NodeId> {
    let held: std::collections::HashSet<_> = graph.nodes.iter().map(|n| n.id).collect();
    let (grade, renamed) = grade.with_unique_ids(track.0, |id| held.contains(&id));
    if graph.append_keeping_ids(&grade).is_err() {
        return HashMap::new();
    }
    let own = |id: klipp_nodes::NodeId| renamed.iter().find(|(_, now)| **now == id).map_or(id, |(was, _)| *was);
    grade.nodes.iter().filter(|n| !held.contains(&n.id) && graph.nodes.iter().any(|m| m.id == n.id)).map(|n| (own(n.id), n.id)).collect()
}

/// What Copy Track Attributes carries (#846): a track's levels and its
/// own effects, without its clips, its name or its switches.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackSettings {
    pub opacity: f32,
    pub volume: f32,
    pub pan: f32,
    pub effects: TrackEffects,
}

/// Which parts of a track's settings a paste takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackPaste {
    /// Volume, pan and opacity.
    pub levels: bool,
    /// Picture effects and the grade.
    pub picture: bool,
    /// The audio chain.
    pub sound: bool,
}

impl TrackPaste {
    pub const ALL: Self = Self { levels: true, picture: true, sound: true };

    /// Whether the paste takes anything.
    pub fn any(self) -> bool {
        self.levels || self.picture || self.sound
    }
}

/// A track's mixer controls, as the mixer and the track header set them
/// (#3165): one undo step changes them, a fader drag one step whole.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackMix {
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
}

impl Track {
    /// Its volume, pan, mute and solo.
    pub fn mix(&self) -> TrackMix {
        TrackMix { volume: self.volume, pan: self.pan, muted: self.muted, solo: self.solo }
    }

    /// Put `mix` on the track.
    pub fn set_mix(&mut self, mix: TrackMix) {
        (self.volume, self.pan, self.muted, self.solo) = (mix.volume, mix.pan, mix.muted, mix.solo);
    }

    /// The track's levels and own effects, as Copy Track Attributes takes them.
    pub fn settings(&self) -> TrackSettings {
        TrackSettings { opacity: self.opacity, volume: self.volume, pan: self.pan, effects: self.effects.clone() }
    }

    /// Put `settings` on the track whole.
    pub fn set_settings(&mut self, settings: &TrackSettings) {
        self.opacity = settings.opacity;
        self.volume = settings.volume;
        self.pan = settings.pan;
        self.effects = settings.effects.clone();
    }

    /// This track's settings with the parts `what` names taken from
    /// `from`: what a paste of `from` onto this track makes of it.
    pub fn pasted(&self, from: &TrackSettings, what: TrackPaste) -> TrackSettings {
        let mut s = self.settings();
        if what.levels {
            s.opacity = from.opacity;
            s.volume = from.volume;
            s.pan = from.pan;
        }
        if what.picture {
            s.effects.set_video_effect_list(from.effects.video_effects().to_vec(), from.effects.video_effects_enabled().to_vec());
            s.effects.grade = from.effects.grade.clone();
        }
        if what.sound {
            s.effects.audio = from.effects.audio.clone();
        }
        s
    }
}

/// What a clip of a render copy had before its track's effects were put
/// on it (see [`Sequence::bake_track_effects`]): its own effects and
/// audio chain. Never saved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OwnEffects {
    pub builtin_effects: Vec<klipp_effects::BuiltinEffect>,
    pub builtin_effects_enabled: Vec<bool>,
    pub audio_effects: Vec<AudioEffectInstance>,
    /// The effects' node ids, one each, so that taking them back out
    /// (see [`Sequence::with_own_grades`]) gives the clip the ids the
    /// document has, and a path read from it names the real node.
    /// Empty when not known: the effects then get new ids.
    pub ids: Vec<crate::EffectId>,
}

fn default_opacity() -> f32 { 1.0 }
fn default_volume() -> f32 { 1.0 }
fn default_true() -> bool { true }
fn default_clip_volume() -> f32 { 1.0 }
fn default_speed() -> f64 { 1.0 }
fn default_enabled() -> bool { true }

impl Track {
    pub fn new(name: &str, kind: TrackKind) -> Self {
        Self {
            id: TrackId::new(),
            name: name.to_string(),
            kind,
            clips: Vec::new(),
            transitions: Vec::new(),
            opacity: 1.0,
            volume: 1.0,
            muted: false,
            solo: false,
            visible: true,
            pan: 0.0,
            locked: false,
            caption_style: None,
            language: None,
            magnetic_role: TrackRole::Free,
            storyline_anchor: None,
            custom_height: None,
            colour_label: 0,
            channel_waveforms: false,
            pre_collapse_height: None,
            precomposition_sound: false,
            source_file: None,
            sync_lock: false,
            targeted: true,
            source_patch: false,
            record_armed: false,
            automation_lane_visible: None,
            automation_lane_track: false,
            frozen: false,
            automation: Default::default(),
            automation_lane_height: None,
            effects: TrackEffects::default(),
            sends: Vec::new(),
        }
    }

    pub fn append_clip(&mut self, media_id: MediaId, source_duration: Timecode) -> ClipId {
        let timeline_in = self
            .clips
            .last()
            .map(|c| c.timeline_out)
            .unwrap_or(Timecode { value: 0, rate: source_duration.rate });
        // Rescale to source rate for frame-accurate timeline arithmetic
        let timeline_in = if timeline_in.rate != source_duration.rate {
            timeline_in.rescale_to(source_duration.rate)
        } else {
            timeline_in
        };
        let timeline_out = timeline_in + source_duration;
        let clip_id = ClipId::new();
        self.clips.push(Clip {
            id: clip_id,
            kind: ClipKind::Media { media_id },
            timeline_in,
            timeline_out,
            source_in: Timecode { value: 0, rate: source_duration.rate },
            source_out: source_duration,
            picture: crate::picture_store::PictureView {
                node_graph: crate::Grade::default(),
                effects: Default::default(),
                effect_list: Default::default(),
                own_node_graph: None,
                own_effects: None,
                opacity_keyframes: (Vec::new()).into(),
                transform_pos_x_keyframes: (Vec::new()).into(),
                transform_pos_y_keyframes: (Vec::new()).into(),
                transform_scale_keyframes: (Vec::new()).into(),
                transform_scale_y_keyframes: (Vec::new()).into(),
                transform_rotation_keyframes: (Vec::new()).into(),
                transform_tilt_x_keyframes: (Vec::new()).into(),
                transform_tilt_y_keyframes: (Vec::new()).into(),
                effect_keyframes: (std::collections::HashMap::new()).into(),
                param_modifiers: (HashMap::new()).into(),
                grade_versions: Vec::new(),
                active_grade_version: None,
            }.into(),
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            grade_group: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
            channel_config: ChannelConfig::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            frame_glide: None,
            role: ClipRole::default(),
            role_source: klipp_core::RoleSource::Unset,
            matched_to: None,
            speed: 1.0,
            speed_keyframes: Vec::new(),
            keep_pitch: false,
            hold: None,
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            depth: 1.0,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            multicam_sound: Default::default(),
            track_points: Vec::new(),
            surfaces: Vec::new(),
            surface_pin: None,
            name: None,
            colour_label: 0,
            grade_flag: crate::GradeFlag::None,
            precomp: None,
            baked_own: None,
        });
        clip_id
    }

    pub fn remove_clip(&mut self, clip_id: ClipId) -> bool {
        if let Some(pos) = self.clips.iter().position(|c| c.id == clip_id) {
            self.clips.remove(pos);
            true
        } else {
            false
        }
    }

    pub fn find_clip(&self, clip_id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == clip_id)
    }

    pub fn find_clip_mut(&mut self, clip_id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == clip_id)
    }

    pub fn clip_at_timecode(&self, tc: Timecode) -> Option<(usize, &Clip)> {
        self.clips
            .iter()
            .enumerate()
            .find(|(_, c)| tc >= c.timeline_in && tc < c.timeline_out)
    }

    pub fn sort_clips(&mut self) {
        self.clips.sort_by_key(|c| c.timeline_in);
    }

    /// Remove clip and shift all downstream clips left by the removed clip's duration.
    pub fn ripple_remove(&mut self, clip_id: ClipId) -> Option<Clip> {
        let pos = self.clips.iter().position(|c| c.id == clip_id)?;
        let clip = self.clips.remove(pos);
        let gap = clip.timeline_out - clip.timeline_in;
        for c in &mut self.clips[pos..] {
            c.timeline_in -= gap;
            c.timeline_out -= gap;
        }
        Some(clip)
    }

    /// Returns the clip whose OUT edge is at or before `tc`, maximised.
    /// When the playhead sits inside a clip, returns that clip. When it sits
    /// on a gap, returns the clip immediately before. Used by "trim OUT to
    /// playhead" (Q) to pick its target without requiring a selection.
    pub fn clip_with_out_at_or_before(&self, tc: &Timecode) -> Option<(usize, &Clip)> {
        self.clips
            .iter()
            .enumerate()
            .filter(|(_, c)| &c.timeline_out <= tc || (&c.timeline_in <= tc && tc < &c.timeline_out))
            .max_by_key(|(_, c)| c.timeline_out)
    }

    /// Returns the clip whose IN edge is at or after `tc`, minimised.
    /// When the playhead sits inside a clip, returns that clip. When it sits
    /// on a gap, returns the clip immediately after. Used by "trim IN to
    /// playhead" (W) to pick its target without requiring a selection.
    pub fn clip_with_in_at_or_after(&self, tc: &Timecode) -> Option<(usize, &Clip)> {
        self.clips
            .iter()
            .enumerate()
            .filter(|(_, c)| &c.timeline_in >= tc || (&c.timeline_in <= tc && tc < &c.timeline_out))
            .min_by_key(|(_, c)| c.timeline_in)
    }

    /// Ripple trim left edge: adjust source_in/timeline_in and shift downstream clips.
    pub fn ripple_trim_left(&mut self, clip_id: ClipId, new_source_in: Timecode, new_timeline_in: Timecode) {
        let pos = match self.clips.iter().position(|c| c.id == clip_id) {
            Some(p) => p,
            None => return,
        };
        let old_timeline_in = self.clips[pos].timeline_in;
        let delta = new_timeline_in - old_timeline_in;
        self.clips[pos].source_in = new_source_in;
        self.clips[pos].timeline_in = new_timeline_in;
        // Shift this clip and all downstream clips by delta
        for c in &mut self.clips[pos..] {
            if c.id != clip_id {
                c.timeline_in += delta;
                c.timeline_out += delta;
            }
        }
    }

    /// Ripple trim right edge: adjust source_out/timeline_out and shift downstream clips.
    pub fn ripple_trim_right(&mut self, clip_id: ClipId, new_source_out: Timecode, new_timeline_out: Timecode) {
        let pos = match self.clips.iter().position(|c| c.id == clip_id) {
            Some(p) => p,
            None => return,
        };
        let old_timeline_out = self.clips[pos].timeline_out;
        let delta = new_timeline_out - old_timeline_out;
        self.clips[pos].source_out = new_source_out;
        self.clips[pos].timeline_out = new_timeline_out;
        // Shift all clips after this one by delta
        for c in &mut self.clips[pos + 1..] {
            c.timeline_in += delta;
            c.timeline_out += delta;
        }
    }

    /// The blade tool: [`Self::split_at`], refusing a cut within a frame of
    /// either edge. That is a click-tolerance rule — a blade landing a
    /// hair inside a clip is almost always meant for the edge — and it
    /// belongs to the tool, not to the split. The frame is the
    /// sequence's own, so `fps` comes from the sequence the track is in.
    pub fn blade_at(&mut self, clip_id: ClipId, cut_at: Timecode, fps: Fps) -> Option<(Clip, Clip)> {
        let clip = self.clips.iter().find(|c| c.id == clip_id)?;
        let frame = one_frame(fps);
        if cut_at <= clip.timeline_in + frame || cut_at >= clip.timeline_out - frame {
            return None;
        }
        self.split_at(clip_id, cut_at)
    }

    /// Split a clip at `cut_at`, which must lie strictly inside it, into
    /// two clips that replace it in place: the left keeps the clip's head
    /// of source and the keyframes, markers and effect keyframes before
    /// the cut; the right takes the rest, re-timed from its own start.
    /// Both get new ids. Returns `(left, right)`.
    ///
    /// The fades stay at the clip's own ends — the left keeps its fade in
    /// and the right its fade out — and the two edges the cut makes have
    /// no fade at all, so a split never dips the join it creates. A fade
    /// longer than the half it lands on is shortened to it.
    pub fn split_at(&mut self, clip_id: ClipId, cut_at: Timecode) -> Option<(Clip, Clip)> {
        let pos = self.clips.iter().position(|c| c.id == clip_id)?;
        let clip = self.clips[pos].clone();
        if cut_at <= clip.timeline_in || cut_at >= clip.timeline_out {
            return None;
        }

        let offset = cut_at - clip.timeline_in;
        let cut_secs = offset.to_secs_f64();
        // Where the cut falls in the source: through the clip's speed, and
        // from its far end when it plays reversed, whose earlier half on
        // the timeline is the later part of its range (#617). The source
        // was split one to one with the timeline, so a half of a 2x clip
        // showed the wrong part of the file.
        let rate = clip.source_in.rate;
        let at_source = |secs: f64| Timecode::from_secs_f64(secs, rate);
        let moved = clip.speed_integral(cut_secs, SPLIT_STEP_FPS);
        let ((a_in, a_out), (b_in, b_out)) = if (clip.speed - 1.0).abs() < 1e-9 && clip.speed_keyframes.is_empty() {
            ((clip.source_in, clip.source_in + offset), (clip.source_in + offset, clip.source_out))
        } else if clip.is_reversed() {
            let split = at_source(clip.source_out.to_secs_f64() - moved);
            ((split, clip.source_out), (clip.source_in, split))
        } else {
            let split = at_source(clip.source_in.to_secs_f64() + moved);
            ((clip.source_in, split), (split, clip.source_out))
        };
        // A fade cannot outlast the piece it is left on: the left half may
        // be shorter than the clip's fade in, the right shorter than its
        // fade out.
        let left_secs = cut_secs as f32;
        let right_secs = (clip.timeline_out - cut_at).to_secs_f64() as f32;

        // Split keyframes at the cut point
        let split_kfs = |kfs: &[klipp_core::Keyframe]| -> (Vec<klipp_core::Keyframe>, Vec<klipp_core::Keyframe>) {
            let mut before = Vec::new();
            let mut after = Vec::new();
            for kf in kfs {
                if kf.time_secs < cut_secs {
                    before.push(kf.clone());
                } else {
                    let mut shifted = kf.clone();
                    shifted.time_secs -= cut_secs;
                    after.push(shifted);
                }
            }
            (before, after)
        };
        let (vol_a, vol_b) = split_kfs(&clip.volume_keyframes);
        let (pan_a, pan_b) = split_kfs(&clip.pan_keyframes);
        let (gain_a, gain_b) = split_kfs(&clip.gain_keyframes);
        let (opacity_a, opacity_b) = split_kfs(clip.opacity_keyframes());
        let (pos_x_a, pos_x_b) = split_kfs(clip.transform_pos_x_keyframes());
        let (pos_y_a, pos_y_b) = split_kfs(clip.transform_pos_y_keyframes());
        let (scale_a, scale_b) = split_kfs(clip.transform_scale_keyframes());
        let (scale_y_a, scale_y_b) = split_kfs(clip.transform_scale_y_keyframes());
        let (rot_a, rot_b) = split_kfs(clip.transform_rotation_keyframes());
        let (tilt_x_a, tilt_x_b) = split_kfs(clip.transform_tilt_x_keyframes());
        let (tilt_y_a, tilt_y_b) = split_kfs(clip.transform_tilt_y_keyframes());
        let (mfeath_a, mfeath_b) = split_kfs(&clip.matte_feather_keyframes);
        let (mchoke_a, mchoke_b) = split_kfs(&clip.matte_choke_keyframes);

        // Split effect keyframes at the cut point
        let mut effect_kfs_a = std::collections::HashMap::new();
        let mut effect_kfs_b = std::collections::HashMap::new();
        for (key, kfs) in clip.effect_keyframes() {
            let (a, b) = split_kfs(kfs);
            if !a.is_empty() {
                effect_kfs_a.insert(key.clone(), a);
            }
            if !b.is_empty() {
                effect_kfs_b.insert(key.clone(), b);
            }
        }

        // Split audio-effect keyframes at the cut point
        let mut audio_effect_kfs_a = std::collections::HashMap::new();
        let mut audio_effect_kfs_b = std::collections::HashMap::new();
        for (key, kfs) in &clip.audio_effect_keyframes {
            let (a, b) = split_kfs(kfs);
            if !a.is_empty() {
                audio_effect_kfs_a.insert(key.clone(), a);
            }
            if !b.is_empty() {
                audio_effect_kfs_b.insert(key.clone(), b);
            }
        }

        // Split clip markers at the cut point
        let mut markers_a = Vec::new();
        let mut markers_b = Vec::new();
        for m in &clip.clip_markers {
            if m.time_secs < cut_secs {
                markers_a.push(m.clone());
            } else {
                let mut shifted = m.clone();
                shifted.time_secs -= cut_secs;
                markers_b.push(shifted);
            }
        }

        // The clip's picture cut at the blade (M4 step 12f), and the clip
        // itself when it's picture-truth, taken before the halves are made
        // from its fields.
        let truth_of = clip.is_picture_truth().then(|| clip.clone());
        let cut = clip.picture_v2().map(|lifted| crate::picture_split::split_picture(&lifted, cut_secs));
        let clip_a = Clip {
            id: ClipId::new(),
            kind: clip.kind.clone(),
            timeline_in: clip.timeline_in,
            timeline_out: cut_at,
            source_in: a_in,
            source_out: a_out,
            picture: crate::picture_store::PictureView {
                node_graph: clip.picture.view().node_graph.clone(),
                effects: clip.picture.view().effects.clone(),
                effect_list: crate::EffectList::new(clip.builtin_effects().to_vec(), clip.builtin_effects_enabled().to_vec()),
                own_node_graph: None,
                own_effects: None,
                opacity_keyframes: (opacity_a).into(),
                transform_pos_x_keyframes: (pos_x_a).into(),
                transform_pos_y_keyframes: (pos_y_a).into(),
                transform_scale_keyframes: (scale_a).into(),
                transform_scale_y_keyframes: (scale_y_a).into(),
                transform_rotation_keyframes: (rot_a).into(),
                transform_tilt_x_keyframes: (tilt_x_a).into(),
                transform_tilt_y_keyframes: (tilt_y_a).into(),
                effect_keyframes: (effect_kfs_a).into(),
                param_modifiers: (clip.param_modifiers().clone()).into(),
                grade_versions: clip.picture.view().grade_versions.clone(),
                active_grade_version: clip.picture.view().active_grade_version,
            }.into(),
            linked_clip_id: clip.linked_clip_id,
            linked_title_asset_id: clip.linked_title_asset_id,
            group_id: clip.group_id,
            grade_group: clip.grade_group,
            magnetic_anchor: clip.magnetic_anchor.clone(),
            gain: clip.gain,
            volume: clip.volume,
            pan: clip.pan,
            fade_in_secs: clip.fade_in_secs.min(left_secs),
            // The cut is not an edge of the clip: a fade out here would
            // dip the join to nothing on a cut that should pass unheard.
            fade_out_secs: 0.0,
            fade_in_curve: clip.fade_in_curve,
            fade_out_curve: clip.fade_out_curve,
            channel_config: clip.channel_config,
            volume_keyframes: vol_a,
            pan_keyframes: pan_a,
            gain_keyframes: gain_a,
            matte_feather_keyframes: mfeath_a,
            matte_choke_keyframes: mchoke_a,
            clip_markers: markers_a,
            // A glide is the whole clip's move: each piece keeps its share of the keys (#611).
            frame_glide: None,
            role: clip.role,
            role_source: clip.role_source,
            matched_to: None,
            speed: clip.speed,
            speed_keyframes: clip.speed_keyframes.clone(),
            keep_pitch: clip.keep_pitch,
            hold: clip.hold,
            source_rotation: clip.source_rotation,
            enabled: clip.enabled,
            solo: clip.solo,
            blend_mode: clip.blend_mode,
            depth: clip.depth,
            matte_source: clip.matte_source.clone(),
            matte_stencil: clip.matte_stencil.clone(),
            consumed_as_matte: clip.consumed_as_matte,
            transform_link: None,
            multicam_group: clip.multicam_group,
            multicam_angle: clip.multicam_angle,
            multicam_cuts: clip.multicam_cuts.clone(),
            multicam_sound: clip.multicam_sound,
            audio_effects: clip.audio_effects.clone(),
            audio_effect_keyframes: audio_effect_kfs_a,
            track_points: Vec::new(),
            // The keys are in file time: each half keeps the track (#684).
            surfaces: clip.surfaces.clone(),
            surface_pin: clip.surface_pin,
            name: clip.name.clone(),
            colour_label: clip.colour_label,
            grade_flag: crate::GradeFlag::None,
            precomp: None,
            baked_own: None,
        };
        let clip_b = Clip {
            id: ClipId::new(),
            kind: clip.kind.clone(),
            timeline_in: cut_at,
            timeline_out: clip.timeline_out,
            source_in: b_in,
            source_out: b_out,
            picture: crate::picture_store::PictureView {
                node_graph: clip.picture.view().node_graph.clone(),
                effects: clip.picture.view().effects.clone(),
                effect_list: crate::EffectList::new(clip.builtin_effects().to_vec(), clip.builtin_effects_enabled().to_vec()),
                own_node_graph: None,
                own_effects: None,
                opacity_keyframes: (opacity_b).into(),
                transform_pos_x_keyframes: (pos_x_b).into(),
                transform_pos_y_keyframes: (pos_y_b).into(),
                transform_scale_keyframes: (scale_b).into(),
                transform_scale_y_keyframes: (scale_y_b).into(),
                transform_rotation_keyframes: (rot_b).into(),
                transform_tilt_x_keyframes: (tilt_x_b).into(),
                transform_tilt_y_keyframes: (tilt_y_b).into(),
                effect_keyframes: (effect_kfs_b).into(),
                param_modifiers: (clip.param_modifiers().clone()).into(),
                grade_versions: clip.picture.view().grade_versions.clone(),
                active_grade_version: clip.picture.view().active_grade_version,
            }.into(),
            linked_clip_id: clip.linked_clip_id,
            linked_title_asset_id: clip.linked_title_asset_id,
            group_id: clip.group_id,
            grade_group: clip.grade_group,
            magnetic_anchor: None, // right half gets no anchor
            gain: clip.gain,
            volume: clip.volume,
            pan: clip.pan,
            // The other side of the join: no fade in here either.
            fade_in_secs: 0.0,
            fade_out_secs: clip.fade_out_secs.min(right_secs),
            fade_in_curve: clip.fade_in_curve,
            fade_out_curve: clip.fade_out_curve,
            channel_config: clip.channel_config,
            volume_keyframes: vol_b,
            pan_keyframes: pan_b,
            gain_keyframes: gain_b,
            matte_feather_keyframes: mfeath_b,
            matte_choke_keyframes: mchoke_b,
            clip_markers: markers_b,
            // A glide is the whole clip's move: each piece keeps its share of the keys (#611).
            frame_glide: None,
            role: clip.role,
            role_source: clip.role_source,
            matched_to: None,
            speed: clip.speed,
            speed_keyframes: clip.speed_keyframes.clone(),
            keep_pitch: clip.keep_pitch,
            hold: clip.hold,
            source_rotation: clip.source_rotation,
            enabled: clip.enabled,
            solo: clip.solo,
            blend_mode: clip.blend_mode,
            depth: clip.depth,
            matte_source: clip.matte_source.clone(),
            matte_stencil: clip.matte_stencil.clone(),
            consumed_as_matte: clip.consumed_as_matte,
            transform_link: None,
            multicam_group: clip.multicam_group,
            multicam_angle: clip.multicam_angle,
            multicam_cuts: clip.multicam_cuts.clone(),
            multicam_sound: clip.multicam_sound,
            audio_effects: clip.audio_effects,
            audio_effect_keyframes: audio_effect_kfs_b,
            track_points: Vec::new(),
            surfaces: clip.surfaces.clone(),
            surface_pin: clip.surface_pin,
            name: clip.name.clone(),
            colour_label: clip.colour_label,
            grade_flag: crate::GradeFlag::None,
            precomp: None,
            baked_own: None,
        };
        // Each half takes its cut of the clip's picture (M4 step 12f); a
        // picture-truth clip's halves are picture-truth too.
        let (mut clip_a, mut clip_b) = (clip_a, clip_b);
        if let Some(source) = &truth_of {
            clip_a.take_picture_truth_of(source);
            clip_b.take_picture_truth_of(source);
        }
        let (mut clip_a, mut clip_b) = match cut {
            Ok((a, b)) => (Clip::from_picture(&clip_a, a), Clip::from_picture(&clip_b, b)),
            Err(why) => {
                tracing::warn!("split: clip {:?}: its picture: {why}", clip_a.id);
                (clip_a, clip_b)
            }
        };
        let whole = (clip.timeline_out - clip.timeline_in).to_secs_f64();
        clip_a.keep_angle_cuts_on_piece(0.0, cut_secs);
        clip_b.keep_angle_cuts_on_piece(cut_secs, whole);

        self.clips.remove(pos);
        self.clips.insert(pos, clip_b.clone());
        self.clips.insert(pos, clip_a.clone());

        Some((clip_a, clip_b))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Transition {
    pub id: TransitionId,
    pub kind: TransitionKind,
    pub duration: Timecode,
    pub clip_a_id: ClipId,
    pub clip_b_id: ClipId,
    /// Per-kind tunable parameters (feather, colour, rotations, etc.).
    /// Missing from older project files — defaults are kind-neutral.
    #[serde(default)]
    pub user_params: klipp_core::TransitionUserParams,
    /// Where the transition sits relative to the cut between its two clips.
    /// Defaults to centred for backwards compatibility with older projects.
    #[serde(default)]
    pub alignment: klipp_core::TransitionAlignment,
}

impl Transition {
    /// The sequence time it runs over, from `cut`, the start of its
    /// incoming clip, as its alignment places it.
    pub fn span(&self, cut: Timecode) -> (Timecode, Timecode) {
        match self.alignment {
            klipp_core::TransitionAlignment::Center => {
                let half = klipp_core::RationalTime { value: self.duration.value / 2, rate: self.duration.rate };
                (cut - half, cut + half)
            }
            klipp_core::TransitionAlignment::StartAtCut => (cut, cut + self.duration),
            klipp_core::TransitionAlignment::EndAtCut => (cut - self.duration, cut),
        }
    }
}

// TransitionKind is defined in klipp-core and re-exported here for backward compatibility.
pub use klipp_core::TransitionKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
    Text,
    Subtitle,
}

/// A named snapshot of a clip's node graph (color grade).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeVersion {
    pub name: String,
    pub node_graph: crate::Grade,
}

/// A project-level reusable colour grade. The user saves the current
/// clip's node graph as a named "Look" and applies it to other clips
/// later. Persisted under `.klipp/looks/{id}.json`. Distinct from
/// `GradeVersion`, which is per-clip undo-style versioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Look {
    pub id: uuid::Uuid,
    pub name: String,
    pub node_graph: klipp_nodes::NodeGraph,
}

/// A marker placed at a specific time within a clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipMarker {
    /// Seconds from the clip's start on the timeline.
    pub time_secs: f64,
    /// Display name.
    pub name: String,
    /// Color.
    pub color: MarkerColor,
    /// Free-text comment / note. Empty by default — legacy projects
    /// without this field deserialise to "" via the serde default.
    /// Mirrors `Marker.comment` for sequence markers; kind is omitted
    /// here intentionally (clip markers stay simple — promote to a
    /// sequence marker if you need a kind).
    #[serde(default)]
    pub comment: String,
}

/// Composition rule for a clip whose `transform_link` points at
/// another clip. `Absolute` overrides this clip's transform with the
/// target's; `Additive` composes (positions add, scales multiply,
/// rotations add). Anchor inheritance is always Absolute regardless
/// of mode — anchor is the *origin* the other ops happen from, so
/// "additive anchor" doesn't have a well-defined meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LinkMode {
    #[default]
    Absolute,
    Additive,
}

/// Inherit one clip's transform from another. Used by hyperzoom (V2
/// fill scales with V3 text), parenting (lower-third tracks a
/// speaker), compound rigs, etc. Per-axis inherit flags let a
/// child inherit just the bits it needs (e.g., "follow my parent's
/// position but keep my own scale"). Self-link, cycles, and
/// AdjustmentLayer targets are rejected at command-dispatch time.
///
/// Render-path cost is zero when no clip in the active sequence has
/// a `transform_link` set. When at least one does, the engine builds
/// a per-frame `HashMap<ClipId, ResolvedTransform>` cache so a deep
/// chain or many-children-of-one-parent setup resolves in O(N) rather
/// than O(N·depth).
/// Where a [`TransformLink`] inherits its values from. Today's
/// dominant case is `Clip(ClipId)` — inherit from another clip's
/// resolved transform. `Tracker { … }` is HZ-MATTE-NEXT-TRACKER-
/// DRIVER's growth point: the link target is a tracker on
/// `clip_id`'s `track_points[point_index]`, and the engine
/// synthesises a position-only `ResolvedTransform` from the
/// tracker's per-frame `(x, y)` output.
///
/// **Save format back-compat** is the load-bearing concern. Old
/// `.klipp` files have `"target": "<uuid>"` directly. The
/// `#[serde(untagged)]` attribute below makes this enum accept
/// both shapes:
/// * Bare uuid string → `Clip(ClipId)` (legacy + the default
///   for new files using `Clip` variant — same on-disk shape, no
///   migration needed)
/// * Tagged object `{ "clip_id": ..., "point_index": N }` →
///   `Tracker { ... }`
///
/// Round-trip stability: serializing `Clip(uuid)` produces a bare
/// uuid string (newtype-style serde for `ClipId`), and loading a
/// bare uuid back lands in `Clip(...)`. Serializing
/// `Tracker { ... }` produces an object; loading the object lands
/// in `Tracker(...)`. Tests pin both directions plus the legacy-
/// load path explicitly.
// `#[serde(untagged)]` dispatches between variants based on **shape**,
// not a tag field. That's deliberate — it lets the legacy bare-uuid
// form (a JSON string) deserialise as `Clip(uuid)` without a wire-
// format break. **The constraint this places on future variants**:
// any variant added here MUST serialise to a JSON shape that's
// structurally distinct from every existing variant. If a future
// variant happens to deserialise from the same shape as `Clip`
// (a string) or `Tracker` (an object with `clip_id` + `point_index`),
// untagged dispatch becomes ambiguous and serde picks the first
// matching variant — silent data corruption. When adding variants:
// either use a clearly different shape (a number, an array, an
// object with a unique field), or migrate this enum to
// `#[serde(tag = "kind")]` with explicit migration logic.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TransformLinkTarget {
    /// Inherit from another clip's resolved transform. Serialises
    /// as a **bare uuid string** — intentionally byte-identical to
    /// the pre-migration `target: ClipId` field. This means a
    /// project saved by current Klipp using the `Clip` variant can
    /// be loaded by an older Klipp build that pre-dates the enum
    /// migration. Forward-compat for the dominant case.
    Clip(ClipId),
    Tracker {
        clip_id: ClipId,
        point_index: u32,
    },
}

impl TransformLinkTarget {
    /// Convenience: when the caller only cares about the clip the
    /// link references (e.g. the `cascade_clear_transform_link`
    /// sweep, the cycle-detection walker, the inspector candidate
    /// filter), reduce both variants to a single `ClipId`. For
    /// `Clip(id)` it's the linked clip; for `Tracker { clip_id, .. }`
    /// it's the clip the tracker lives on.
    pub fn referenced_clip_id(&self) -> ClipId {
        match self {
            Self::Clip(id) => *id,
            Self::Tracker { clip_id, .. } => *clip_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransformLink {
    /// What this link inherits from. `Clip(...)` for the dominant
    /// "follow another clip's resolved transform" case;
    /// `Tracker { ... }` for the "follow a tracker on a clip" case
    /// (HZ-MATTE-NEXT-TRACKER-DRIVER). Old project files load the
    /// bare-uuid form into `Clip(...)` via `#[serde(untagged)]` —
    /// see [`TransformLinkTarget`] for the migration story.
    pub target: TransformLinkTarget,
    /// Composition rule. See [`LinkMode`].
    #[serde(default)]
    pub mode: LinkMode,
    /// Inherit the target's `(scale_x, scale_y)`. Composed with the
    /// child's scale per `mode` (multiplied for Additive, replaced
    /// for Absolute). When `false`, the child uses its own scale.
    #[serde(default = "default_true")]
    pub inherit_scale: bool,
    /// Inherit the target's `(position_x, position_y)`. Composed
    /// per `mode` (added for Additive, replaced for Absolute).
    #[serde(default = "default_true")]
    pub inherit_position: bool,
    /// Inherit the target's `rotation` (degrees, Z-axis). Composed
    /// per `mode` (added for Additive, replaced for Absolute).
    #[serde(default = "default_true")]
    pub inherit_rotation: bool,
    /// Inherit the target's `(anchor_x, anchor_y)`. Always Absolute
    /// regardless of `mode` — see the type-level note on `LinkMode`.
    /// When `false`, the child uses its own anchor.
    #[serde(default = "default_true")]
    pub inherit_anchor: bool,
}

impl TransformLink {
    /// Build a link to `target` with all four inherit flags on, in
    /// the default `Absolute` mode. Matches the easy-path defaults a
    /// command-dispatch path uses when a user simply drops one clip
    /// onto another with "link transforms".
    ///
    /// Accepts a bare `ClipId` and wraps it in
    /// `TransformLinkTarget::Clip` — keeps the call-site API the
    /// same after the schema migration so the dominant
    /// "link clip → clip" path doesn't churn at every site. Use
    /// `TransformLink::new_tracker(...)` (or build the struct
    /// directly) for the tracker-target case.
    pub fn new(target: ClipId) -> Self {
        Self {
            target: TransformLinkTarget::Clip(target),
            mode: LinkMode::default(),
            inherit_scale: true,
            inherit_position: true,
            inherit_rotation: true,
            inherit_anchor: true,
        }
    }

    /// Build a link that follows a tracker on `clip_id`'s
    /// `track_points[point_index]`. Sibling to [`Self::new`] for
    /// the HZ-MATTE-NEXT-TRACKER-DRIVER case; defaults match
    /// `new` (Absolute mode, all four inherit flags on).
    pub fn new_tracker(clip_id: ClipId, point_index: u32) -> Self {
        Self {
            target: TransformLinkTarget::Tracker { clip_id, point_index },
            mode: LinkMode::default(),
            inherit_scale: true,
            inherit_position: true,
            inherit_rotation: true,
            inherit_anchor: true,
        }
    }
}

/// Binds another timeline clip as the alpha/luma source ("track matte") for
/// the clip that owns this binding. The render path resolves
/// `source_clip_id`'s rendered RGBA at the same timeline time, extracts the
/// chosen `channel`, applies `invert`/`feather_px`/`choke_px`, and multiplies
/// the consumer clip's alpha by the resulting mask before compositing.
///
/// The matte source clip is rendered as a normal clip; whether it is also
/// shown in the visible composite is controlled by the source clip's
/// `consumed_as_matte` flag, not by this binding (so a single matte source
/// can drive multiple consumers and one consumer can keep the source visible
/// for debugging).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatteBinding {
    /// Which clip's rendered output drives this matte. Validated at bind
    /// time: must not be self, must not introduce a cycle.
    pub source_clip_id: ClipId,
    /// Which channel of the source becomes the mask signal.
    #[serde(default)]
    pub channel: klipp_nodes::params::MatteChannel,
    /// Invert the mask (mask = 1 - mask) after channel extraction.
    #[serde(default)]
    pub invert: bool,
    /// Soften the mask edges by this radius in pixels. 0 = hard edge.
    /// Applied as a Gaussian blur on the single-channel mask only.
    #[serde(default)]
    pub feather_px: f32,
    /// Erode the mask by this radius in pixels (negative-feather). 0 = no
    /// choke. Used by retouchers to tighten chromakey edges.
    #[serde(default)]
    pub choke_px: f32,
}

impl MatteBinding {
    /// Build a default binding pointing at `source_clip_id`. Defaults match
    /// the "easy path" UX: Luminance channel, not inverted, no feather/choke.
    pub fn new(source_clip_id: ClipId) -> Self {
        Self {
            source_clip_id,
            channel: klipp_nodes::params::MatteChannel::default(),
            invert: false,
            feather_px: 0.0,
            choke_px: 0.0,
        }
    }
}

/// Phase 4B — stencil settings on a clip. When set, the owning clip's
/// rendered RGBA acts as a mask that multiplies the alpha of EVERY
/// clip on a lower track at the same timeline time. The stencil clip
/// itself is implicitly hidden from the visible composite (AE
/// convention — the stencil composite mode replaces the layer's own
/// visibility with its mask role).
///
/// Distinct from `MatteBinding` (the consumer-side bind) so the two
/// can coexist on the same clip with different settings: e.g. a clip
/// could be both stencil-source for everything below AND have its
/// own alpha masked by a separate clip above.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StencilSettings {
    /// Which channel of the stencil clip's RGBA becomes the mask.
    #[serde(default)]
    pub channel: klipp_nodes::params::MatteChannel,
    /// Invert the mask (mask = 1 - mask).
    #[serde(default)]
    pub invert: bool,
    /// Soften the edges (Gaussian blur radius, px).
    #[serde(default)]
    pub feather_px: f32,
    /// Erode the mask (px).
    #[serde(default)]
    pub choke_px: f32,
}

impl Default for StencilSettings {
    fn default() -> Self {
        Self {
            channel: klipp_nodes::params::MatteChannel::default(),
            invert: false,
            feather_px: 0.0,
            choke_px: 0.0,
        }
    }
}

fn default_depth() -> f32 {
    1.0
}

fn is_default_depth(d: &f32) -> bool {
    (*d - 1.0).abs() < 1e-6
}

fn default_blend_mode() -> klipp_core::BlendMode { klipp_core::BlendMode::Normal }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub kind: ClipKind,
    pub timeline_in: Timecode,
    pub timeline_out: Timecode,
    pub source_in: Timecode,
    pub source_out: Timecode,
    /// The clip's picture: its grade, effects, picture animation and grade
    /// versions (#1089, M1b). Its fields are reached only through the
    /// clip's methods, so nothing outside can change one part of it
    /// behind the rest.
    #[serde(flatten)]
    pub picture: ClipPictureFields,
    #[serde(default)]
    pub linked_clip_id: Option<ClipId>,
    /// If `Some`, this clip is a *linked instance* of a project-scoped
    /// title asset (a `MediaItem` with `MediaItemKind::SyntheticTitle`).
    /// The renderer reads the canonical `TitleData` from the asset
    /// store keyed by this `MediaId`, so editing the asset updates
    /// every linked instance on the timeline at once. The clip's
    /// inline `ClipKind::Title(TitleData)` is kept in sync as a
    /// fallback for paths that don't have asset-store access (export,
    /// snapshot tools), but the asset is the source of truth while
    /// linked. `None` = independent / unlinked title clip.
    #[serde(default)]
    pub linked_title_asset_id: Option<klipp_core::MediaId>,
    #[serde(default)]
    pub group_id: Option<GroupId>,
    /// The grade group this clip's grade runs inside (#237), whose
    /// pre-clip and post-clip grades wrap its own. A different thing from
    /// `group_id`, which moves clips together on the timeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade_group: Option<GradeGroupId>,
    /// If Some, this clip is connected to an anchor on the primary storyline.
    #[serde(default)]
    pub magnetic_anchor: Option<MagneticAnchor>,
    #[serde(default = "default_clip_volume")]
    pub gain: f32,
    #[serde(default = "default_clip_volume")]
    pub volume: f32,
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub fade_in_secs: f32,
    #[serde(default)]
    pub fade_out_secs: f32,
    #[serde(default)]
    pub fade_in_curve: FadeCurve,
    #[serde(default)]
    pub fade_out_curve: FadeCurve,
    #[serde(default)]
    pub channel_config: ChannelConfig,
    #[serde(default)]
    pub volume_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub pan_keyframes: Vec<klipp_core::Keyframe>,
    #[serde(default)]
    pub gain_keyframes: Vec<klipp_core::Keyframe>,
    /// Phase 4D — track-matte feather radius keyframes (px). Empty
    /// for the common case (constant feather); evaluated against
    /// `matte_source.feather_px` as the fallback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matte_feather_keyframes: Vec<klipp_core::Keyframe>,
    /// Phase 4D — track-matte choke radius keyframes (px). Same
    /// fallback contract as `matte_feather_keyframes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matte_choke_keyframes: Vec<klipp_core::Keyframe>,
    /// Per-clip markers at specific times within the clip.
    #[serde(default)]
    pub clip_markers: Vec<ClipMarker>,
    /// The clip's Frame Glide (#611): a move between two framings, written
    /// into its position and scale keys over its visible length. `None`
    /// for a clip without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_glide: Option<crate::frame_glide::FrameGlide>,
    /// Content role — determines timeline color coding.
    #[serde(default)]
    pub role: ClipRole,
    /// Whether the role was set by hand or proposed from the sound
    /// (#2216).
    #[serde(default, skip_serializing_if = "klipp_core::RoleSource::is_unset")]
    pub role_source: klipp_core::RoleSource,
    /// The shot this clip is kept matched to (#2236): it starts and ends
    /// with it, following its trims, rolls and moves. A piece of a split
    /// or a copy of the clip matches nothing; the link is dropped when the
    /// shot is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_to: Option<ClipId>,
    /// Playback speed multiplier (1.0 = normal, 2.0 = 2x fast, 0.5 = half speed)
    #[serde(default = "default_speed")]
    pub speed: f64,
    /// Speed keyframes for variable speed envelopes (time-varying speed).
    /// When non-empty, these override the constant `speed` field.
    #[serde(default)]
    pub speed_keyframes: Vec<klipp_core::Keyframe>,
    /// A retimed clip's sound keeps its pitch, played longer or shorter,
    /// where by default it moves with the speed as tape's does (#925).
    /// Forward speeds only: a reversed clip's sound is tape's.
    #[serde(default)]
    pub keep_pitch: bool,
    /// A freeze frame (#2773): the source time shown for the clip's whole
    /// length, whatever its speed, its sound silent. Its length is its
    /// timeline length; `None` plays the clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<Timecode>,
    /// Source media rotation in degrees (0, 90, 180, 270). Auto-detected from metadata.
    /// Applied as a pre-transform via the GPU pipeline. Users can override per-clip.
    #[serde(default)]
    pub source_rotation: i32,
    /// Whether this clip is enabled. Disabled clips are skipped during render/playback
    /// but remain visible (dimmed) on the timeline.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Solo: when true, only this clip's audio plays on its track (others muted).
    #[serde(default)]
    pub solo: bool,
    /// Per-clip blend mode for the visible composite. Default `Normal` keeps
    /// existing behaviour; the engine and export compositors are wired for
    /// all 13 modes already, just hard-coded to Normal at the call sites.
    #[serde(default = "default_blend_mode")]
    pub blend_mode: klipp_core::BlendMode,
    /// How far back the clip sits behind the composed plane, for the
    /// sequence camera: 1 is the plane itself, 3 is three times as far
    /// and drifts a third as much, 0.5 is nearer and moves twice as much.
    /// 0 fixes the clip to the screen, out of the camera's reach — type
    /// that should hold still while the scene drifts.
    #[serde(default = "default_depth", skip_serializing_if = "is_default_depth")]
    pub depth: f32,
    /// Track-matte binding: another clip's rendered RGBA drives this clip's
    /// alpha mask. `None` = no matte (default). Render path multiplies this
    /// clip's source alpha by the resolved mask before blending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_source: Option<MatteBinding>,
    /// Phase 4B — stencil mode: this clip's pixels mask EVERY clip on a
    /// lower track at the same timeline time. Implicit visibility-hide
    /// (the stencil clip itself only contributes as mask, not as
    /// visible pixels). `None` = not a stencil (default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_stencil: Option<StencilSettings>,
    /// True when this clip is being used as a matte source by another clip
    /// AND the user wants it hidden from the visible composite (the
    /// "Hide matte source" inspector toggle, default on). When false, the
    /// clip renders both as a matte input and as a visible layer — useful
    /// for matte debugging or rare creative effects. The render path still
    /// renders the clip either way; this flag only gates the visible blend.
    #[serde(default)]
    pub consumed_as_matte: bool,
    /// Inherit transform from another clip in this sequence. When
    /// `Some`, the engine resolves the linked clip's evaluated
    /// transform at the same playhead and composes it into this
    /// clip's transform per `link.mode` and per-axis flags. None →
    /// no inheritance, this clip's own transform stands alone (the
    /// existing behaviour). Validated at command-dispatch time:
    /// self-link / cycle / AdjustmentLayer-target are rejected.
    /// Cascade-cleared in `remove_clip_impl` when the target is
    /// removed, so a stale `target` ClipId can never linger past
    /// the target's lifetime.
    #[serde(default)]
    pub transform_link: Option<TransformLink>,
    /// Multicam group ID: clips sharing this ID are different angles of the same event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam_group: Option<GroupId>,
    /// Multicam angle index within the group (0-based). Angle 0 = primary/default.
    #[serde(default)]
    pub multicam_angle: u8,
    /// For the primary multicam clip: cut list mapping timecodes to angle indices.
    /// Each entry is (time_secs_from_clip_start, angle_index).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub multicam_cuts: Vec<(f64, u8)>,
    /// For the primary multicam clip: whose sound the group plays.
    #[serde(default, skip_serializing_if = "crate::MulticamSound::is_default")]
    pub multicam_sound: crate::MulticamSound,
    /// Per-clip audio effect chain (EQ, compressor, reverb, etc.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio_effects: Vec<AudioEffectInstance>,
    /// Per-audio-effect keyframes: maps "{effect_index}:{param_name}" to keyframe track.
    /// Reserved param names: "__bypass" (0/1, step) and "__wetdry" (0..1, linear).
    #[serde(default)]
    pub audio_effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    /// Tracked points for motion tracking.
    #[serde(default)]
    pub track_points: Vec<klipp_core::TrackPoint>,
    /// Surfaces tracked in this clip (#684).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surfaces: Vec<crate::Surface>,
    /// The surface on another clip this clip is pinned to (#684): its
    /// corner pin follows that surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_pin: Option<crate::SurfacePin>,
    /// The clip's own name on the timeline (#920). `None` shows its
    /// media's name, as before; see [`Clip::label`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The clip's own colour label (#920), on the media pool's scale:
    /// 0 none, then red, orange, yellow, green, blue and purple. 0 keeps
    /// the colour the timeline gives it by track and role.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub colour_label: u8,
    /// Where the clip's grade stands (#899): the colourist's flag, apart
    /// from its colour label, which says what the clip is.
    #[serde(default, skip_serializing_if = "GradeFlag::is_none")]
    pub grade_flag: GradeFlag,
    /// A precomposition's scene, resolved for rendering: the inner
    /// sequence with its compounds flattened, its own precompositions
    /// resolved, and every clip re-identified for this instance. Set by
    /// [`crate::flatten::flatten_sequence`], never saved — the file holds
    /// the `sequence_id` and nothing else.
    #[serde(skip)]
    pub precomp: Option<std::sync::Arc<Sequence>>,
    /// What of a render copy's effects are the clip's own (M4 step 12f),
    /// set by [`Sequence::bake_track_effects`] alone: the bake appends its
    /// track's after them, so they are the first in list order. `None` on
    /// any clip the bake didn't make, all of whose effects are its own.
    /// Never saved, and not part of its picture.
    #[serde(skip)]
    pub baked_own: Option<BakedOwn>,
}

/// How many of a baked render copy's effects are the clip's own: its
/// first `effects` built-in effects and first `audio` audio effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BakedOwn {
    pub effects: usize,
    pub audio: usize,
}

/// How many built-in effects `clip`'s picture lists.
fn listed_effects(clip: &Clip) -> usize {
    (0..).take_while(|i| clip.effect_node_at(false, *i).is_some()).count()
}

/// A clip's own name, colour label (#920) and grade flag (#899), as one
/// value: what a rename, a recolour or a flag changes, and what undo puts
/// back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipLabel {
    pub name: Option<String>,
    /// On the media pool's colour label scale; 0 is none.
    pub colour: u8,
    pub flag: GradeFlag,
}

pub use klipp_core::GradeFlag;

impl Clip {
    /// Whether any of the clip's plug-in effects is on (#995). A clip
    /// whose plug-ins are all off renders as one without any: no 8-bit
    /// round trip, and nothing keeps it off the GPU paths.
    pub fn has_enabled_plugins(&self) -> bool {
        // As the clip's picture holds them (M4 step 12f).
        self.picture_ofx().is_ok_and(|ofx| ofx.iter().any(|p| p.entry.enabled))
    }

    pub fn media_id(&self) -> Option<MediaId> {
        match &self.kind {
            ClipKind::Media { media_id } => Some(*media_id),
            _ => None,
        }
    }

    pub fn title_data(&self) -> Option<&crate::clip_kind::TitleData> {
        match &self.kind {
            ClipKind::Title(data) => Some(data),
            _ => None,
        }
    }

    pub fn generator_data(&self) -> Option<&klipp_core::GeneratorData> {
        match &self.kind {
            ClipKind::Generator(data) => Some(data),
            _ => None,
        }
    }

    pub fn duration(&self) -> Timecode {
        self.timeline_out - self.timeline_in
    }

    /// The clip's own name, colour label and grade flag together (#920).
    pub fn own_label(&self) -> ClipLabel {
        ClipLabel { name: self.name.clone(), colour: self.colour_label, flag: self.grade_flag }
    }

    pub fn set_own_label(&mut self, label: &ClipLabel) {
        self.name = label.name.clone();
        self.colour_label = label.colour;
        self.grade_flag = label.flag;
    }

    /// What the clip is called on the timeline (#920): its own name when
    /// it has one, otherwise `fallback`, its media's name.
    pub fn label<'a>(&'a self, fallback: &'a str) -> &'a str {
        match self.name.as_deref() {
            Some(name) if !name.trim().is_empty() => name,
            _ => fallback,
        }
    }

    pub fn source_duration(&self) -> Timecode {
        self.source_out - self.source_in
    }

    /// Evaluate speed at a given clip-relative time (seconds from clip start).
    /// Returns the constant speed if no keyframes, or the keyframed value.
    pub fn speed_at(&self, clip_time_secs: f64) -> f64 {
        if self.speed_keyframes.is_empty() {
            self.speed
        } else {
            klipp_core::keyframe::evaluate(&self.speed_keyframes, clip_time_secs, self.speed as f32) as f64
        }
    }

    /// Integrate the speed curve from 0 to `timeline_elapsed_secs` and return
    /// the source-time offset that a constant-speed clip would reach in the
    /// same interval. Uses `dt = 1/fps` Riemann sum clamped to [0.1, 16.0].
    /// The magnitude: how far through its range the clip has moved, in
    /// whichever direction it plays.
    pub fn speed_integral(&self, timeline_elapsed_secs: f64, fps: f64) -> f64 {
        speed_integral_keyframes(&self.speed_keyframes, timeline_elapsed_secs, fps, self.speed)
    }

    /// Whether the clip's own sound is heard. At a constant speed it is,
    /// varispeed: read at that speed, its pitch moving with it as tape's
    /// does, backward when reversed (#642); playback and export read it
    /// alike. A speed ramp has no one speed to read at, and is silent.
    pub fn plays_its_sound(&self) -> bool {
        self.speed_keyframes.is_empty() && self.hold.is_none()
    }

    /// Whether its sound is read at another speed than its own.
    pub fn is_varispeed(&self) -> bool {
        (self.speed - 1.0).abs() > 1e-6
    }

    /// Whether the clip plays its source backward: a negative speed (#617).
    /// Its first frame on the timeline is the last of its range, and it
    /// walks back to `source_in` at its end.
    pub fn is_reversed(&self) -> bool {
        self.speed < 0.0
    }

    /// The source second shown `elapsed` timeline seconds into the clip,
    /// through its speed curve and in its direction. Before the clip or
    /// past its end (a transition running into a handle) it goes on at the
    /// edge's speed.
    ///
    /// A reversed clip is the forward one mirrored, a frame earlier: its
    /// first timeline frame shows the frame that starts one frame before
    /// `source_out` — the last of its range — and its last shows the frame
    /// at `source_in`.
    pub fn source_secs_at(&self, elapsed: f64, fps: f64) -> f64 {
        if let Some(held) = self.hold {
            return held.to_secs_f64();
        }
        let dur = (self.timeline_out - self.timeline_in).to_secs_f64();
        let offset = |e: f64| {
            if e < 0.0 {
                e * self.speed_at(0.0).abs()
            } else if e > dur {
                self.speed_integral(dur, fps) + (e - dur) * self.speed_at(dur).abs()
            } else {
                self.speed_integral(e, fps)
            }
        };
        if self.is_reversed() {
            self.source_out.to_secs_f64() - offset(elapsed + 1.0 / fps.max(1.0))
        } else {
            self.source_in.to_secs_f64() + offset(elapsed)
        }
    }

    /// The inverse of [`Self::source_secs_at`]: the timeline seconds into
    /// the clip at which `source_secs` is shown.
    ///
    /// Which frame that is depends on what the time is (#691). A moment of
    /// sound, a word's edge or a pause, has no frame of its own and goes on
    /// the nearest frame boundary: this, times the rate, rounded. A change
    /// in the file's picture belongs on the first frame that shows it,
    /// which is [`Self::first_frame_showing`]; rounding this instead lands
    /// a frame early whenever the file's rate is not the sequence's.
    pub fn elapsed_for_source_secs(&self, source_secs: f64, fps: f64) -> f64 {
        if self.is_reversed() {
            let moved = (self.source_out.to_secs_f64() - source_secs).max(0.0);
            (effective_timeline_duration_for_keyframes(&self.speed_keyframes, moved, fps, self.speed) - 1.0 / fps.max(1.0)).max(0.0)
        } else {
            let moved = (source_secs - self.source_in.to_secs_f64()).max(0.0);
            effective_timeline_duration_for_keyframes(&self.speed_keyframes, moved, fps, self.speed)
        }
    }

    /// Where a change in the file's picture at `source_secs` (the start of
    /// one of its frames, at `media_fps`) belongs on the timeline, at
    /// `fps`: the first sequence frame, counted from the timeline's start,
    /// that shows that frame of the file or one past it (one before it,
    /// on a reversed clip). `None` when no frame of the clip does (#691).
    ///
    /// A sequence frame shows the file's frame nearest the source time it
    /// asks for, as the decoders take it, so the change comes on the first
    /// frame whose source time is past the midpoint between that frame of
    /// the file and the one before. Rounding the mapped time instead puts
    /// the cut a frame early whenever the rates differ, or at 2x on an odd
    /// frame, and the new piece opens on a frame of the old shot. Found by
    /// searching the forward map, which is what the renderer evaluates, so
    /// it is exact through a speed ramp too.
    pub fn first_frame_showing(&self, source_secs: f64, media_fps: f64, fps: f64) -> Option<i64> {
        if !(media_fps > 0.0 && fps > 0.0 && source_secs.is_finite()) {
            return None;
        }
        // The file's frame as the decoder stamps it, and what makes a
        // sequence frame show it: export's ClipStream::seek_forward takes
        // a frame whose time is at or before the time asked for plus half
        // a source frame.
        let frame_time = (source_secs * media_fps).round() / media_fps;
        let half = 0.5 / media_fps;
        // Elapsed time in whole ticks, as the renderer takes it from
        // timecodes, so a tie falls the same way here as there.
        let rate = self.timeline_in.rate.max(1);
        let ticks_per_frame = (rate as f64 / fps).round().max(1.0) as i64;
        let first = (self.timeline_in.value as f64 / ticks_per_frame as f64).round() as i64;
        let end = (self.timeline_out.value as f64 / ticks_per_frame as f64).round() as i64;
        let reversed = self.is_reversed();
        let past = |f: i64| {
            let elapsed = (f * ticks_per_frame - self.timeline_in.value) as f64 / rate as f64;
            let reaches = frame_time <= self.source_secs_at(elapsed, fps) + half;
            reaches != reversed
        };
        // The source time only moves one way through the clip, so the
        // frames past the change are a run at its end.
        let (mut lo, mut hi) = (first, end);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if past(mid) { hi = mid } else { lo = mid + 1 }
        }
        (lo < end).then_some(lo)
    }

    /// Map a sequence time (timeline-space) to the corresponding source
    /// time on this clip's media. Honours `clip.source_in` plus the
    /// constant `speed` field plus any `speed_keyframes` envelope.
    /// Returns `None` if `seq_time` falls outside the clip's timeline
    /// range — caller should treat that as "no source time at this
    /// position".
    ///
    /// Inverse of [`Self::sequence_time_for_source_time`]. The two map cleanly
    /// in both directions for monotonic speed curves; the round-trip
    /// drift is bounded by the integration step (~1 frame at the
    /// project framerate).
    pub fn source_time_for_sequence_time(
        &self,
        seq_time: Timecode,
        fps: f64,
    ) -> Option<Timecode> {
        if seq_time < self.timeline_in || seq_time >= self.timeline_out {
            return None;
        }
        let timeline_elapsed = (seq_time - self.timeline_in).to_secs_f64().max(0.0);
        let source_secs = self.source_secs_at(timeline_elapsed, fps);
        Some(Timecode::from_secs_f64(source_secs, self.source_in.rate))
    }

    /// Map a source time on this clip's media back to its sequence
    /// time. Inverse of [`Self::source_time_for_sequence_time`]. Returns
    /// `None` if `source_time` falls outside `[clip.source_in,
    /// clip.source_out)`.
    ///
    /// For variable-speed clips this calls the same Riemann-integral
    /// search the duration helpers use, so the timeline_elapsed it
    /// returns is the t at which the speed integral first reaches the
    /// target source offset.
    pub fn sequence_time_for_source_time(
        &self,
        source_time: Timecode,
        fps: f64,
    ) -> Option<Timecode> {
        if source_time < self.source_in || source_time >= self.source_out {
            return None;
        }
        let timeline_elapsed = self.elapsed_for_source_secs(source_time.to_secs_f64(), fps);
        let timeline_secs = self.timeline_in.to_secs_f64() + timeline_elapsed;
        // Clamp to the clip's timeline range so a near-edge round-trip
        // doesn't land 1 sample past timeline_out.
        let clamped = timeline_secs.min(self.timeline_out.to_secs_f64() - 1e-9);
        Some(Timecode::from_secs_f64(clamped, self.timeline_in.rate))
    }

    /// Set the InputTransform node's log_format on this clip's node
    /// graph. If the format is Srgb (default), does nothing. If no
    /// InputTransform node exists (current default graph is Source
    /// → Corrector → Output and ships without one), splice a fresh
    /// IDT in immediately after Source so the IDT runs before any
    /// grading. Without this splice, calling this method on a
    /// default-graph clip silently no-ops and the user sees flat
    /// log footage in the program viewer.
    pub fn set_input_log_format(&mut self, format: klipp_core::LogFormat) {
        if format == klipp_core::LogFormat::Srgb {
            return;
        }
        let graph = &mut self.view_mut().node_graph.0;
        if let Some(node) = graph.nodes.iter_mut()
            .find(|n| n.kind == klipp_nodes::NodeKind::InputTransform)
        {
            node.params.log_format = Some(format);
            return;
        }
        // No IDT — splice one in after the Source node. We need the
        // existing Source → next-node edge so we can re-route it
        // through the new IDT.
        let source_id = match graph.nodes.iter()
            .find(|n| n.kind == klipp_nodes::NodeKind::Source)
            .map(|n| n.id)
        {
            Some(id) => id,
            None => return, // graph has no Source — bail rather than corrupt it
        };
        let new_idt_id = klipp_nodes::NodeId::new_v4();
        let mut idt_node = klipp_nodes::Node {
            id: new_idt_id,
            kind: klipp_nodes::NodeKind::InputTransform,
            label: "Input Transform".into(),
            enabled: true,
            params: klipp_nodes::NodeParams::default(),
            keyframes: klipp_nodes::NodeKeyframeTracks::default(),
            output_link: None,
            input_links: Vec::new(),
        };
        idt_node.params.log_format = Some(format);
        // Re-route every edge that started at Source to start at the
        // new IDT instead, then add the Source → IDT bridge. Keeps
        // every downstream consumer wired exactly as before, just
        // with one extra node in front.
        for edge in graph.edges.iter_mut() {
            if edge.from_node == source_id {
                edge.from_node = new_idt_id;
            }
        }
        graph.edges.push(klipp_nodes::Edge {
            from_node: source_id,
            from_port: 0,
            to_node: new_idt_id,
            to_port: 0,
        });
        // Insert the new node right after Source so editor layout
        // reads top-to-bottom in the expected order. Position it
        // halfway between Source and the next node so the V2 editor
        // doesn't draw it on top of an existing node.
        let source_idx = graph.nodes.iter()
            .position(|n| n.id == source_id)
            .unwrap_or(0);
        graph.nodes.insert(source_idx + 1, idt_node);
        // Position the new node 110 px right of Source — same step
        // size the default graph layout uses.
        let source_pos = graph.node_positions
            .get(&source_id)
            .copied()
            .unwrap_or(klipp_nodes::NodePos { x: 0.0, y: 0.0 });
        graph.node_positions.insert(
            new_idt_id,
            klipp_nodes::NodePos { x: source_pos.x + 110.0, y: source_pos.y },
        );
    }

    /// Create a gap placeholder clip with the given timeline extent.
    /// Rescales timeline_out to match timeline_in's rate if they differ.
    pub fn gap(timeline_in: Timecode, timeline_out: Timecode) -> Self {
        let timeline_out = if timeline_out.rate != timeline_in.rate {
            timeline_out.rescale_to(timeline_in.rate)
        } else {
            timeline_out
        };
        Self {
            id: ClipId::new(),
            kind: ClipKind::Gap,
            timeline_in,
            timeline_out,
            source_in: Timecode { value: 0, rate: timeline_in.rate },
            source_out: timeline_out - timeline_in,
            picture: crate::picture_store::PictureView {
                node_graph: crate::Grade::default(),
                effects: Default::default(),
                effect_list: Default::default(),
                own_node_graph: None,
                own_effects: None,
                opacity_keyframes: (Vec::new()).into(),
                transform_pos_x_keyframes: (Vec::new()).into(),
                transform_pos_y_keyframes: (Vec::new()).into(),
                transform_scale_keyframes: (Vec::new()).into(),
                transform_scale_y_keyframes: (Vec::new()).into(),
                transform_rotation_keyframes: (Vec::new()).into(),
                transform_tilt_x_keyframes: (Vec::new()).into(),
                transform_tilt_y_keyframes: (Vec::new()).into(),
                effect_keyframes: (std::collections::HashMap::new()).into(),
                param_modifiers: (HashMap::new()).into(),
                grade_versions: Vec::new(),
                active_grade_version: None,
            }.into(),
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            grade_group: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
            channel_config: ChannelConfig::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            frame_glide: None,
            role: ClipRole::default(),
            role_source: klipp_core::RoleSource::Unset,
            matched_to: None,
            speed: 1.0,
            speed_keyframes: Vec::new(),
            keep_pitch: false,
            hold: None,
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            depth: 1.0,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            multicam_sound: Default::default(),
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            track_points: Vec::new(),
            surfaces: Vec::new(),
            surface_pin: None,
            name: None,
            colour_label: 0,
            grade_flag: crate::GradeFlag::None,
            precomp: None,
            baked_own: None,
        }
    }

    /// The clip's own grade: on a render copy with grade groups baked in,
    /// without its group's (see [`Sequence::bake_grade_groups`]).
    pub fn own_grade(&self) -> &klipp_nodes::NodeGraph {
        self.picture.view().own_node_graph.as_deref().unwrap_or(&self.picture.view().node_graph.0)
    }

    /// The sequence's timeline grade as the clip the grade stages already
    /// run: an adjustment layer over the whole sequence holding the
    /// graph, its time the sequence's own. Its id is fixed for the
    /// sequence (see [`Sequence::timeline_grade_clip_id`]), so per-clip
    /// caches keyed on it hold from frame to frame. `None` when there is
    /// no timeline grade, or it changes nothing.
    pub fn timeline_grade_of(seq: &Sequence) -> Option<Self> {
        seq.timeline_grade().filter(|g| !g.is_identity())?;
        Some(Self::timeline_grade_proxy(seq))
    }

    /// [`Clip::timeline_grade_of`] whether or not there is a grade yet —
    /// the default graph when there is none — for editing it as a clip.
    pub fn timeline_grade_proxy(seq: &Sequence) -> Self {
        let rate = seq.fps.rate();
        let zero = Timecode { value: 0, rate };
        // Far past any sequence: it covers every frame there is.
        let end = Timecode { value: i64::from(rate) * 60 * 60 * 24 * 365, rate };
        let mut clip = Self::media(MediaId::nil(), zero, end, zero, end);
        clip.id = seq.timeline_grade_clip_id();
        clip.kind = ClipKind::AdjustmentLayer;
        clip.view_mut().node_graph = seq.timeline_graph.clone().unwrap_or_default();
        clip
    }

    /// Create a media clip with the given source and timeline extents.
    pub fn media(
        media_id: MediaId,
        timeline_in: Timecode,
        timeline_out: Timecode,
        source_in: Timecode,
        source_out: Timecode,
    ) -> Self {
        Self {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in,
            timeline_out,
            source_in,
            source_out,
            picture: crate::picture_store::PictureView {
                node_graph: crate::Grade::default(),
                effects: Default::default(),
                effect_list: Default::default(),
                own_node_graph: None,
                own_effects: None,
                opacity_keyframes: (Vec::new()).into(),
                transform_pos_x_keyframes: (Vec::new()).into(),
                transform_pos_y_keyframes: (Vec::new()).into(),
                transform_scale_keyframes: (Vec::new()).into(),
                transform_scale_y_keyframes: (Vec::new()).into(),
                transform_rotation_keyframes: (Vec::new()).into(),
                transform_tilt_x_keyframes: (Vec::new()).into(),
                transform_tilt_y_keyframes: (Vec::new()).into(),
                effect_keyframes: (std::collections::HashMap::new()).into(),
                param_modifiers: (HashMap::new()).into(),
                grade_versions: Vec::new(),
                active_grade_version: None,
            }.into(),
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            grade_group: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
            channel_config: ChannelConfig::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            frame_glide: None,
            role: ClipRole::default(),
            role_source: klipp_core::RoleSource::Unset,
            matched_to: None,
            speed: 1.0,
            speed_keyframes: Vec::new(),
            keep_pitch: false,
            hold: None,
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            depth: 1.0,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            multicam_sound: Default::default(),
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            track_points: Vec::new(),
            surfaces: Vec::new(),
            surface_pin: None,
            name: None,
            colour_label: 0,
            grade_flag: crate::GradeFlag::None,
            precomp: None,
            baked_own: None,
        }
    }

    /// Create a compound clip (nested sequence) with the given timeline extent.
    pub fn sequence(
        sequence_id: klipp_core::SequenceId,
        timeline_in: Timecode,
        timeline_out: Timecode,
    ) -> Self {
        Self { kind: ClipKind::Sequence { sequence_id }, ..Self::nested(sequence_id, timeline_in, timeline_out) }
    }

    /// Create a precomposition of `sequence_id` with the given timeline
    /// extent: the same timing as a compound clip, composed in isolation.
    pub fn precomposition(
        sequence_id: klipp_core::SequenceId,
        timeline_in: Timecode,
        timeline_out: Timecode,
    ) -> Self {
        Self { kind: ClipKind::Precomposition { sequence_id }, ..Self::nested(sequence_id, timeline_in, timeline_out) }
    }

    /// What a compound clip and a precomposition share: a source range
    /// that starts at the inner sequence's beginning and runs the clip's
    /// length, and nothing else set.
    fn nested(
        sequence_id: klipp_core::SequenceId,
        timeline_in: Timecode,
        timeline_out: Timecode,
    ) -> Self {
        let dur = timeline_out - timeline_in;
        Self {
            id: ClipId::new(),
            kind: ClipKind::Sequence { sequence_id },
            timeline_in,
            timeline_out,
            source_in: Timecode { value: 0, rate: timeline_in.rate },
            source_out: dur,
            picture: crate::picture_store::PictureView {
                node_graph: crate::Grade::default(),
                effects: Default::default(),
                effect_list: Default::default(),
                own_node_graph: None,
                own_effects: None,
                opacity_keyframes: (Vec::new()).into(),
                transform_pos_x_keyframes: (Vec::new()).into(),
                transform_pos_y_keyframes: (Vec::new()).into(),
                transform_scale_keyframes: (Vec::new()).into(),
                transform_scale_y_keyframes: (Vec::new()).into(),
                transform_rotation_keyframes: (Vec::new()).into(),
                transform_tilt_x_keyframes: (Vec::new()).into(),
                transform_tilt_y_keyframes: (Vec::new()).into(),
                effect_keyframes: (std::collections::HashMap::new()).into(),
                param_modifiers: (HashMap::new()).into(),
                grade_versions: Vec::new(),
                active_grade_version: None,
            }.into(),
            linked_clip_id: None,
            linked_title_asset_id: None,
            group_id: None,
            grade_group: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: FadeCurve::default(),
            fade_out_curve: FadeCurve::default(),
            channel_config: ChannelConfig::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            frame_glide: None,
            role: ClipRole::default(),
            role_source: klipp_core::RoleSource::Unset,
            matched_to: None,
            speed: 1.0,
            speed_keyframes: Vec::new(),
            keep_pitch: false,
            hold: None,
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            depth: 1.0,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            multicam_sound: Default::default(),
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            track_points: Vec::new(),
            surfaces: Vec::new(),
            surface_pin: None,
            name: None,
            colour_label: 0,
            grade_flag: crate::GradeFlag::None,
            precomp: None,
            baked_own: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Time Remap — free helpers
// ═══════════════════════════════════════════════════════════════════

/// The integration step [`Track::split_at`] finds a ramped clip's cut in
/// its source with: it has no sequence rate to hand, and a sixtieth of a
/// second puts the cut within a frame of where playback has it.
const SPLIT_STEP_FPS: f64 = 60.0;

/// Speed clamp used by the time-remap integrator. Matches the UI graph range
/// (0.1× – 16×) and the `SetClipSpeed` handler clamp. Prevents zero/negative
/// step sizes from stalling integration or producing NaN source times.
pub const SPEED_MIN: f64 = 0.1;
pub const SPEED_MAX: f64 = 16.0;

/// Evaluate the keyframed speed curve at `clip_time_secs`. Returns `default_speed`
/// if there are no keyframes, otherwise interpolates via `klipp_core::keyframe::evaluate`.
/// Does NOT clamp — callers that feed this into the clock must clamp themselves.
pub fn speed_at_keyframes(
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    default_speed: f64,
) -> f64 {
    if keyframes.is_empty() {
        default_speed
    } else {
        klipp_core::keyframe::evaluate(keyframes, clip_time_secs, default_speed as f32) as f64
    }
}

/// Integrate the (clamped) speed curve's magnitude — a reversed clip moves
/// through its source as far as a forward one — from 0 to `timeline_elapsed_secs` using
/// `dt = 1/fps` Riemann sums with a partial-step correction on the tail.
///
/// Returns the corresponding source-time offset: for constant speed=1.0 this is
/// just `timeline_elapsed_secs`; for a 2× ramp this returns ~2× the elapsed.
///
/// Falls back to `default_speed * timeline_elapsed_secs` when keyframes is empty.
pub fn speed_integral_keyframes(
    keyframes: &[klipp_core::Keyframe],
    timeline_elapsed_secs: f64,
    fps: f64,
    default_speed: f64,
) -> f64 {
    if timeline_elapsed_secs <= 0.0 {
        return 0.0;
    }
    if keyframes.is_empty() {
        return default_speed.abs().clamp(SPEED_MIN, SPEED_MAX) * timeline_elapsed_secs;
    }
    let dt = 1.0 / fps.max(1.0);
    // Number of full steps; the last partial step gets a separate correction.
    let full_steps = (timeline_elapsed_secs / dt).floor() as usize;
    let mut source_offset = 0.0f64;
    for i in 0..full_steps {
        let t = i as f64 * dt;
        let spd = speed_at_keyframes(keyframes, t, default_speed).abs().clamp(SPEED_MIN, SPEED_MAX);
        source_offset += spd * dt;
    }
    // Tail: the fractional step between full_steps*dt and timeline_elapsed_secs.
    let tail_start = full_steps as f64 * dt;
    let tail_len = (timeline_elapsed_secs - tail_start).max(0.0);
    if tail_len > 0.0 {
        let spd = speed_at_keyframes(keyframes, tail_start, default_speed).abs().clamp(SPEED_MIN, SPEED_MAX);
        source_offset += spd * tail_len;
    }
    source_offset
}

/// Given a clip's source duration and speed envelope, compute the timeline-space
/// duration that exhausts the source: i.e. find T such that
/// `speed_integral_keyframes(keyframes, T, fps, default) == source_duration_secs`.
///
/// Uses forward Riemann integration with `dt = 1/fps`. Falls back to
/// `source_duration_secs / default_speed` for the no-keyframes case.
///
/// Caps at 20× the source duration as a safety valve against a curve that
/// dwells near `SPEED_MIN` forever.
pub fn effective_timeline_duration_for_keyframes(
    keyframes: &[klipp_core::Keyframe],
    source_duration_secs: f64,
    fps: f64,
    default_speed: f64,
) -> f64 {
    if source_duration_secs <= 0.0 {
        return 0.0;
    }
    if keyframes.is_empty() {
        let spd = default_speed.abs().clamp(SPEED_MIN, SPEED_MAX);
        return source_duration_secs / spd;
    }
    let dt = 1.0 / fps.max(1.0);
    let safety_cap = source_duration_secs * 20.0;
    let mut source_offset = 0.0f64;
    let mut timeline_t = 0.0f64;
    while source_offset < source_duration_secs && timeline_t < safety_cap {
        let spd = speed_at_keyframes(keyframes, timeline_t, default_speed).abs().clamp(SPEED_MIN, SPEED_MAX);
        let step_source = spd * dt;
        if source_offset + step_source >= source_duration_secs {
            // Partial final step: only use the fraction needed to hit the target.
            let remaining = source_duration_secs - source_offset;
            timeline_t += remaining / spd;
            break;
        }
        source_offset += step_source;
        timeline_t += dt;
    }
    timeline_t
}

#[cfg(test)]
mod tests {
    /// #2724: a version is older than its master once the master's cut
    /// changes (a trim, a shot added), not when a grade does.
    #[test]
    fn a_version_goes_stale_when_its_masters_cut_changes() {
        let at = |s: f64| RationalTime::from_secs_f64(s, 30);
        let mut master = Sequence::new_default();
        master.tracks[0].clips.push(Clip::media(MediaId::new(), at(0.0), at(4.0), at(0.0), at(4.0)));
        let mut version = master.clone();
        version.id = SequenceId::new();
        version.version_of = Some(VersionOf { master: master.id, cut: master.cut_stamp() });
        let fresh = |m: &Sequence, v: &Sequence| v.version_is_stale(&[m.clone(), v.clone()]);
        assert_eq!(fresh(&master, &version), Some(false));
        master.tracks[0].clips[0].gain = 0.5;
        assert_eq!(fresh(&master, &version), Some(false), "a gain is not the cut");
        master.tracks[0].clips[0].timeline_out = at(3.0);
        assert_eq!(fresh(&master, &version), Some(true), "trimmed");
        assert_eq!(master.version_is_stale(&[master.clone()]), None, "the master is no version");
    }

    use super::*;

    /// #496: an explicit size and rate survive the first clip; an
    /// "Automatic" one follows it, and only while the sequence has no
    /// video yet.
    #[test]
    fn the_first_clip_sets_only_what_was_left_automatic() {
        let vertical = Resolution { width: 1080, height: 1920 };
        let uhd = Resolution { width: 3840, height: 2160 };
        let (pal, ntsc) = (Fps::new(25, 1), Fps::new(30000, 1001));
        let fresh = |auto: FirstClipAutoSet| {
            let mut seq = Sequence::new_default();
            seq.resolution = vertical;
            seq.fps = pal;
            seq.first_clip_autoset = auto;
            seq
        };

        let mut chosen = fresh(FirstClipAutoSet { resolution: false, fps: false, rate_answered: false });
        assert_eq!(chosen.adopt_first_clip(uhd, ntsc), None);
        assert_eq!((chosen.resolution, chosen.fps), (vertical, pal));

        let mut rate_only = fresh(FirstClipAutoSet { resolution: false, fps: true, rate_answered: false });
        assert_eq!(rate_only.adopt_first_clip(uhd, ntsc), Some(((vertical, pal), (vertical, ntsc))));

        let mut automatic = fresh(FirstClipAutoSet::default());
        assert_eq!(automatic.adopt_first_clip(uhd, ntsc), Some(((vertical, pal), (uhd, ntsc))));

        // Once there is picture, a later clip changes nothing.
        let mut placed = fresh(FirstClipAutoSet::default());
        let video = placed.tracks.iter().position(|t| t.kind == TrackKind::Video).expect("a video track");
        let tc = |f| Timecode::from_frames(f, 25);
        placed.tracks[video].clips.push(crate::Clip::media(MediaId::new(), tc(0), tc(25), tc(0), tc(25)));
        assert_eq!(placed.adopt_first_clip(uhd, ntsc), None);
        assert_eq!((placed.resolution, placed.fps), (vertical, pal));

        // A project saved before the setting existed follows the footage.
        let old: FirstClipAutoSet = serde_json::from_str("{}").unwrap();
        assert_eq!(old, FirstClipAutoSet::default());
    }

    /// #2550 (and #262 before it): a sequence that follows its first clip
    /// takes a 23.976 clip's rate as the exact 24000/1001, never the 24/1
    /// it was made with, and a second clip at that rate is not asked about.
    #[test]
    fn a_23976_first_clip_gives_the_exact_ntsc_rate() {
        let ntsc = Fps::new(24000, 1001);
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(24, 1);
        seq.first_clip_autoset = FirstClipAutoSet::default();
        assert_eq!(seq.first_clip_rate_question(ntsc), None, "a sequence that follows its first clip asks nothing");
        seq.adopt_first_clip(Resolution { width: 1920, height: 1080 }, ntsc);
        assert_eq!((seq.fps.num, seq.fps.den), (24000, 1001));
        assert_eq!(seq.fps.rate(), 24000);
        let video = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).expect("a video track");
        let tc = |f: i64| Timecode::from_frames(f * 1001, 24000);
        seq.tracks[video].clips.push(crate::Clip::media(MediaId::new(), tc(0), tc(24), tc(0), tc(24)));
        assert_eq!(seq.first_clip_rate_question(ntsc), None);
    }

    /// #2550: the rate is asked about only for the first video clip of a
    /// sequence with a rate of its own; with picture down, a clip at
    /// another rate is placed without a question.
    #[test]
    fn only_a_first_clip_at_another_rate_is_asked_about() {
        let (pal, ntsc) = (Fps::new(25, 1), Fps::new(24000, 1001));
        let mut seq = Sequence::new_default();
        seq.fps = pal;
        seq.first_clip_autoset = FirstClipAutoSet { resolution: false, fps: false, rate_answered: false };
        assert_eq!(seq.first_clip_rate_question(ntsc), Some(pal));
        assert_eq!(seq.first_clip_rate_question(pal), None, "the same rate");

        let video = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).expect("a video track");
        let tc = |f| Timecode::from_frames(f, 25);
        seq.tracks[video].clips.push(crate::Clip::media(MediaId::new(), tc(0), tc(25), tc(0), tc(25)));
        assert_eq!(seq.first_clip_rate_question(ntsc), None, "a timeline that holds clips");
    }
    use klipp_core::MediaId;

    const RATE: u32 = 24;

    fn tc(frames: i64) -> Timecode {
        RationalTime::from_frames(frames, RATE)
    }

    /// The rate `tc` counts in, as the sequence's fps.
    fn test_fps() -> Fps {
        Fps::new(RATE, 1)
    }

    fn make_clip(timeline_in: i64, timeline_out: i64, src_in: i64, src_out: i64) -> Clip {
        Clip::media(MediaId::new(), tc(timeline_in), tc(timeline_out), tc(src_in), tc(src_out))
    }

    fn make_track(clips: Vec<Clip>) -> Track {
        let mut t = Track::new("V1", TrackKind::Video);
        t.clips = clips;
        t
    }

    // ═════════════════════════════════════════════════════════════════
    // Track-matte data model (Phase 1)
    // ═════════════════════════════════════════════════════════════════

    #[test]
    fn matte_binding_default_constructor_uses_safe_defaults() {
        // The "easy path" UX produces a binding with these defaults; if
        // they shift the right-click "Use clip above as matte" gesture
        // changes meaning silently. Pin them.
        let src = ClipId::new();
        let b = MatteBinding::new(src);
        assert_eq!(b.source_clip_id, src);
        assert_eq!(b.channel, klipp_nodes::params::MatteChannel::Luminance);
        assert!(!b.invert);
        assert!((b.feather_px - 0.0).abs() < f32::EPSILON);
        assert!((b.choke_px - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn matte_binding_round_trips_through_serde() {
        let src = ClipId::new();
        let b = MatteBinding {
            source_clip_id: src,
            channel: klipp_nodes::params::MatteChannel::Alpha,
            invert: true,
            feather_px: 4.5,
            choke_px: 1.25,
        };
        let json = serde_json::to_string(&b).expect("serialise MatteBinding");
        let back: MatteBinding = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, b);
    }

    #[test]
    fn transform_link_default_constructor_uses_safe_defaults() {
        // `TransformLink::new(target)` is what the "easy path" UX
        // produces — pin the defaults so a casual edit can't shift
        // their meaning. All four inherit flags on, Absolute mode.
        let target = ClipId::new();
        let link = TransformLink::new(target);
        assert_eq!(link.target, TransformLinkTarget::Clip(target));
        assert_eq!(link.mode, LinkMode::Absolute);
        assert!(link.inherit_scale);
        assert!(link.inherit_position);
        assert!(link.inherit_rotation);
        assert!(link.inherit_anchor);
    }

    #[test]
    fn transform_link_round_trips_through_serde() {
        let target = ClipId::new();
        let link = TransformLink {
            target: TransformLinkTarget::Clip(target),
            mode: LinkMode::Additive,
            inherit_scale: true,
            inherit_position: false,
            inherit_rotation: true,
            inherit_anchor: false,
        };
        let json = serde_json::to_string(&link).expect("serialise TransformLink");
        let back: TransformLink = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, link);
    }

    // ─── HZ-MATTE-NEXT-TRACKER-DRIVER: TransformLinkTarget migration ───
    //
    // Focused tests for the schema migration shape itself. Pre-migration
    // the wire format was a bare uuid string at `target`; post-migration
    // the field is `TransformLinkTarget` with `#[serde(untagged)]`. The
    // round-trip + legacy-load tests below pin that the migration is
    // bidirectionally byte-compatible — every existing `.klipp` file in
    // the wild still loads, and re-saving doesn't change the on-disk
    // shape for the dominant `Clip` variant.

    #[test]
    fn transform_link_target_clip_serialises_as_bare_uuid_string() {
        // Pin: the `Clip(uuid)` variant of `TransformLinkTarget`
        // serialises as the SAME bare-uuid string the legacy
        // `target: ClipId` field used. New files writing this
        // variant produce on-disk shapes byte-identical to old
        // files — so a project saved with this build can be opened
        // by the previous build (forward-compat for the dominant
        // case).
        let target = ClipId::new();
        let t = TransformLinkTarget::Clip(target);
        let json = serde_json::to_string(&t).expect("serialise");
        // ClipId is a tuple-newtype around Uuid; serde serialises
        // it as a bare uuid string in JSON.
        let expected = format!("\"{}\"", target.0);
        assert_eq!(json, expected, "Clip variant must serialise as bare uuid string");
    }

    #[test]
    fn transform_link_target_tracker_serialises_as_tagged_object() {
        // The `Tracker` variant doesn't have a back-compat constraint
        // (no legacy form exists); it serialises as the natural
        // untagged-struct shape.
        let clip_id = ClipId::new();
        let t = TransformLinkTarget::Tracker { clip_id, point_index: 7 };
        let json = serde_json::to_string(&t).expect("serialise");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_object(), "Tracker variant serialises as object");
        assert_eq!(parsed["point_index"].as_u64(), Some(7));
        assert_eq!(parsed["clip_id"].as_str(), Some(clip_id.0.to_string().as_str()));
    }

    #[test]
    fn transform_link_target_legacy_bare_uuid_loads_as_clip_variant() {
        // The load-bearing migration test. A pre-migration save file
        // wrote `"target": "<uuid>"` directly; the new
        // `TransformLinkTarget` enum's `#[serde(untagged)]` must
        // accept this and deserialise it as `Clip(<uuid>)`. Without
        // this, every existing `.klipp` file with a transform_link
        // would fail to load.
        let target = ClipId::new();
        let bare_uuid_json = format!("\"{}\"", target.0);
        let back: TransformLinkTarget =
            serde_json::from_str(&bare_uuid_json).expect("legacy bare uuid loads");
        assert_eq!(back, TransformLinkTarget::Clip(target));
    }

    #[test]
    fn transform_link_target_tracker_object_loads_as_tracker_variant() {
        let clip_id = ClipId::new();
        let json = format!(
            r#"{{"clip_id":"{}","point_index":3}}"#,
            clip_id.0,
        );
        let back: TransformLinkTarget = serde_json::from_str(&json).expect("tracker object loads");
        assert_eq!(back, TransformLinkTarget::Tracker { clip_id, point_index: 3 });
    }

    #[test]
    fn transform_link_target_round_trip_clip_variant() {
        let target = ClipId::new();
        let t = TransformLinkTarget::Clip(target);
        let json = serde_json::to_string(&t).expect("serialise");
        let back: TransformLinkTarget = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, t, "round-trip stable for Clip variant");
    }

    #[test]
    fn transform_link_target_round_trip_tracker_variant() {
        let clip_id = ClipId::new();
        let t = TransformLinkTarget::Tracker { clip_id, point_index: 12 };
        let json = serde_json::to_string(&t).expect("serialise");
        let back: TransformLinkTarget = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, t, "round-trip stable for Tracker variant");
    }

    #[test]
    fn transform_link_legacy_json_defaults_inherit_flags_to_true() {
        // Older save format may have only written `target`. Loading
        // such JSON should fill the inherit flags + mode with their
        // safe defaults so a roundtrip through an older project file
        // doesn't silently disable inheritance on every link.
        let target = ClipId::new();
        let json = format!(r#"{{"target":"{}"}}"#, target.0);
        let back: TransformLink = serde_json::from_str(&json).expect("deserialise minimal");
        assert_eq!(back.target, TransformLinkTarget::Clip(target));
        assert_eq!(back.mode, LinkMode::Absolute);
        assert!(back.inherit_scale);
        assert!(back.inherit_position);
        assert!(back.inherit_rotation);
        assert!(back.inherit_anchor);
    }

    #[test]
    fn clip_default_blend_mode_is_normal_and_no_matte() {
        let c = Clip::media(MediaId::new(), tc(0), tc(24), tc(0), tc(24));
        assert_eq!(c.blend_mode, klipp_core::BlendMode::Normal);
        assert!(c.matte_source.is_none());
        assert!(!c.consumed_as_matte);
    }

    #[test]
    fn blade_at_propagates_blend_and_matte_to_both_halves() {
        // Splitting a clip should preserve its blend_mode + matte
        // binding + consumed_as_matte on both halves — anything else
        // would silently break compositing at every blade. Hit this
        // before someone special-cases one of the new fields and not
        // the others.
        let mut c = make_clip(0, 24, 0, 24);
        c.blend_mode = klipp_core::BlendMode::Multiply;
        let src = ClipId::new();
        c.matte_source = Some(MatteBinding {
            source_clip_id: src,
            channel: klipp_nodes::params::MatteChannel::Alpha,
            invert: true,
            feather_px: 3.0,
            choke_px: 0.5,
        });
        c.consumed_as_matte = true;
        let id = c.id;
        let mut track = make_track(vec![c]);

        let (a, b) = track.blade_at(id, tc(12), test_fps()).expect("split");
        for half in [&a, &b] {
            assert_eq!(half.blend_mode, klipp_core::BlendMode::Multiply);
            assert!(half.consumed_as_matte);
            let binding = half.matte_source.as_ref().expect("binding survived split");
            assert_eq!(binding.source_clip_id, src);
            assert_eq!(binding.channel, klipp_nodes::params::MatteChannel::Alpha);
            assert!(binding.invert);
            assert!((binding.feather_px - 3.0).abs() < f32::EPSILON);
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // ripple_trim_left
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn ripple_trim_left_shrinks_clip_shifts_downstream() {
        // Three adjacent clips: [0..24], [24..48], [48..72]
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let c2 = make_clip(48, 72, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0, c1, c2]);

        // Trim clip 0's left edge inward by 6 frames (shrink it)
        track.ripple_trim_left(id0, tc(6), tc(6));

        // Clip 0: timeline_in=6, source_in=6 (smaller by 6 frames)
        assert_eq!(track.clips[0].timeline_in, tc(6));
        assert_eq!(track.clips[0].source_in, tc(6));
        // timeline_out unchanged for clip 0
        assert_eq!(track.clips[0].timeline_out, tc(24));

        // Downstream clips shift by +6
        assert_eq!(track.clips[1].timeline_in, tc(30));
        assert_eq!(track.clips[1].timeline_out, tc(54));
        assert_eq!(track.clips[2].timeline_in, tc(54));
        assert_eq!(track.clips[2].timeline_out, tc(78));
    }

    #[test]
    fn ripple_trim_left_expands_clip_shifts_downstream() {
        // Clip with source headroom: source starts at frame 12
        let c0 = make_clip(12, 36, 12, 36);
        let c1 = make_clip(36, 60, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0, c1]);

        // Expand clip 0's left edge out by 12 frames
        track.ripple_trim_left(id0, tc(0), tc(0));

        assert_eq!(track.clips[0].timeline_in, tc(0));
        assert_eq!(track.clips[0].source_in, tc(0));
        // Downstream shifts by -12
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
    }

    #[test]
    fn ripple_trim_left_nonexistent_clip_is_noop() {
        let c0 = make_clip(0, 24, 0, 24);
        let mut track = make_track(vec![c0]);
        let fake_id = ClipId::new();
        track.ripple_trim_left(fake_id, tc(6), tc(6));
        // Nothing changed
        assert_eq!(track.clips[0].timeline_in, tc(0));
    }

    // ═══════════════════════════════════════════════════════════════
    // ripple_trim_right
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn ripple_trim_right_shrinks_clip_shifts_downstream() {
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let c2 = make_clip(48, 72, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0, c1, c2]);

        // Trim clip 0's right edge inward by 6 frames
        track.ripple_trim_right(id0, tc(18), tc(18));

        assert_eq!(track.clips[0].timeline_out, tc(18));
        assert_eq!(track.clips[0].source_out, tc(18));
        // Downstream clips shift left by 6
        assert_eq!(track.clips[1].timeline_in, tc(18));
        assert_eq!(track.clips[1].timeline_out, tc(42));
        assert_eq!(track.clips[2].timeline_in, tc(42));
        assert_eq!(track.clips[2].timeline_out, tc(66));
    }

    #[test]
    fn ripple_trim_right_expands_clip_shifts_downstream() {
        // Clip with source tail room
        let c0 = make_clip(0, 24, 0, 24); // source has more (we allow setting source_out > original)
        let c1 = make_clip(24, 48, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0, c1]);

        // Extend clip 0 by 12 frames
        track.ripple_trim_right(id0, tc(36), tc(36));

        assert_eq!(track.clips[0].timeline_out, tc(36));
        assert_eq!(track.clips[0].source_out, tc(36));
        // Downstream shifts right by 12
        assert_eq!(track.clips[1].timeline_in, tc(36));
        assert_eq!(track.clips[1].timeline_out, tc(60));
    }

    #[test]
    fn ripple_trim_right_no_downstream_clips() {
        let c0 = make_clip(0, 24, 0, 24);
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);

        track.ripple_trim_right(id0, tc(12), tc(12));

        assert_eq!(track.clips[0].timeline_out, tc(12));
        assert_eq!(track.clips[0].source_out, tc(12));
    }

    // ═══════════════════════════════════════════════════════════════
    // ripple_remove
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn ripple_remove_shifts_downstream() {
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let c2 = make_clip(48, 72, 0, 24);
        let id1 = c1.id;
        let mut track = make_track(vec![c0, c1, c2]);

        let removed = track.ripple_remove(id1).unwrap();
        assert_eq!(removed.timeline_in, tc(24));

        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0].timeline_out, tc(24));
        // c2 shifted left by 24 frames
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].timeline_out, tc(48));
    }

    // ═══════════════════════════════════════════════════════════════
    // blade_at
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn blade_splits_clip_correctly() {
        let c0 = make_clip(0, 48, 0, 48);
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);

        let result = track.blade_at(id0, tc(24), test_fps());
        assert!(result.is_some());

        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0].timeline_out, tc(24));
        assert_eq!(track.clips[0].source_out, tc(24));
        assert_eq!(track.clips[1].timeline_in, tc(24));
        assert_eq!(track.clips[1].source_in, tc(24));
    }

    /// #429: the cut is not an end of the clip, so neither half fades at
    /// it. A blade through a clip with a fade out used to leave a fade
    /// out on the left half and a fade in on the right, dipping to
    /// nothing at a join that should pass unnoticed.
    /// #2216: a role set by hand stays set by hand in both halves, so a
    /// proposal from the sound never replaces it after a blade.
    #[test]
    fn split_keeps_where_the_role_came_from() {
        let mut c = make_clip(0, 96, 0, 96);
        c.role = ClipRole::Music;
        c.role_source = klipp_core::RoleSource::ByHand;
        let id = c.id;
        let mut track = make_track(vec![c]);
        let (a, b) = track.split_at(id, tc(48)).expect("split");
        for half in [&a, &b] {
            assert_eq!((half.role, half.role_source), (ClipRole::Music, klipp_core::RoleSource::ByHand));
        }
    }

    #[test]
    fn split_keeps_each_half_only_its_own_outer_fade() {
        let mut c = make_clip(0, 96, 0, 96);
        c.fade_in_secs = 0.5;
        c.fade_out_secs = 0.75;
        let id = c.id;
        let mut track = make_track(vec![c]);

        let (a, b) = track.split_at(id, tc(48)).expect("split");
        assert_eq!((a.fade_in_secs, a.fade_out_secs), (0.5, 0.0), "the left half keeps its fade in and does not fade out at the cut");
        assert_eq!((b.fade_in_secs, b.fade_out_secs), (0.0, 0.75), "the right half keeps its fade out and does not fade in at the cut");
        assert_eq!((track.clips[0].fade_out_secs, track.clips[1].fade_in_secs), (0.0, 0.0), "the clips on the track carry the same fades");
    }

    /// A fade that no longer fits the half it is left on is shortened to
    /// it: `Op::Audio` refuses a fade longer than its clip, so a split
    /// must not make one. A 4 s clip fading in over 3 s and out over
    /// 3.5 s, cut a second in, leaves a 1 s head and a 3 s tail.
    #[test]
    fn split_shortens_a_fade_that_outlasts_its_half() {
        let mut c = make_clip(0, 96, 0, 96);
        c.fade_in_secs = 3.0;
        c.fade_out_secs = 3.5;
        let id = c.id;
        let mut track = make_track(vec![c]);

        let (a, b) = track.split_at(id, tc(24)).expect("split");
        assert_eq!((a.fade_in_secs, a.fade_out_secs), (1.0, 0.0), "the left half fades in over the whole second it has");
        assert_eq!((b.fade_in_secs, b.fade_out_secs), (0.0, 3.0), "the right half fades out over the three seconds it has");
    }

    #[test]
    fn blade_at_edge_returns_none() {
        let c0 = make_clip(0, 48, 0, 48);
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);

        // Cut at 1 frame from start (within 1-frame guard)
        assert!(track.blade_at(id0, tc(1), test_fps()).is_none());
        // Cut at 1 frame from end
        assert!(track.blade_at(id0, tc(47), test_fps()).is_none());
    }

    /// #434: the guard is a frame of the sequence, not a tick of the
    /// clip's rate. At 29.97 a frame is 1001 ticks of the 30000 rate, so
    /// a cut a frame from the head used to clear a guard a thousandth of
    /// that wide and leave a sliver of a frame nobody can trim.
    #[test]
    fn blade_at_measures_its_edge_guard_in_frames_at_a_fractional_rate() {
        let fps = Fps::ntsc_29970();
        assert_eq!(one_frame(fps), Timecode { value: 1001, rate: 30000 }, "a frame at 29.97 is 1001 ticks");
        let f = |frames: i64| Timecode { value: frames * 1001, rate: 30000 };
        let c = Clip::media(MediaId::new(), f(0), f(100), f(0), f(100));
        let id = c.id;
        let mut track = make_track(vec![c]);

        assert!(track.blade_at(id, f(1), fps).is_none(), "a cut one frame from the head is meant for the head");
        assert!(track.blade_at(id, f(99), fps).is_none(), "and one frame from the tail for the tail");
        assert_eq!(track.clips.len(), 1, "nothing was cut");
        assert!(track.blade_at(id, f(2), fps).is_some(), "two frames in is a cut");
        assert_eq!(track.clips.len(), 2);
    }

    // ═══════════════════════════════════════════════════════════════
    // audio_effect_keyframes split round-trip — Phase 1 of audio
    // keyframe support. Mirrors the existing video `effect_keyframes`
    // split logic at sequence.rs:618-626. The whole point of Option B
    // (separate field) is that audio kfs travel through every clip
    // operation just like video kfs do — blade is the most rigorous
    // exercise because it has to split the kf track at the cut and
    // shift the right half's times back to the new clip's source_in.
    // ═══════════════════════════════════════════════════════════════
    #[test]
    fn blade_splits_audio_effect_keyframes() {
        use klipp_core::{Interpolation, Keyframe};
        let mut c0 = make_clip(0, 48, 0, 48);
        // Three keyframes on `0:gain` at clip-relative times 0.25s, 0.75s,
        // 1.5s. Cut is at frame 24 (= 1.0s @ 24fps), so the first two
        // belong to clip_a and the third to clip_b (with time shifted to
        // 0.5s relative to the new clip).
        let mk = |t: f64, v: f32| Keyframe {
            time_secs: t,
            value: v,
            interpolation: Interpolation::Linear,
            tangent_in: (0.0, 0.0),
            tangent_out: (0.0, 0.0),
            handles: Default::default(),
        };
        c0.audio_effect_keyframes.insert(
            "0:gain".to_string(),
            vec![mk(0.25, 0.5), mk(0.75, 0.7), mk(1.5, 0.9)],
        );
        // Bypass kf on the second-half side only (at 1.25s = 0.25s into clip_b).
        c0.audio_effect_keyframes.insert(
            "0:__bypass".to_string(),
            vec![mk(1.25, 1.0)],
        );
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);

        let result = track.blade_at(id0, tc(24), test_fps());
        assert!(result.is_some(), "blade should succeed at frame 24");
        assert_eq!(track.clips.len(), 2);

        // clip_a: keeps the first two `gain` kfs, no bypass kf.
        let a_gain = track.clips[0].audio_effect_keyframes.get("0:gain")
            .expect("clip_a must keep the gain track");
        assert_eq!(a_gain.len(), 2, "clip_a gets the two pre-cut kfs");
        assert!((a_gain[0].time_secs - 0.25).abs() < 1e-9);
        assert!((a_gain[1].time_secs - 0.75).abs() < 1e-9);
        assert!(!track.clips[0].audio_effect_keyframes.contains_key("0:__bypass"),
            "clip_a must not carry the bypass kf (it sits past the cut)");

        // clip_b: the third gain kf, with time shifted back by 1.0s
        // (the cut point), and the bypass kf shifted likewise.
        let b_gain = track.clips[1].audio_effect_keyframes.get("0:gain")
            .expect("clip_b must inherit the post-cut gain kf");
        assert_eq!(b_gain.len(), 1);
        assert!((b_gain[0].time_secs - 0.5).abs() < 1e-9,
            "clip_b's kf time must be shifted: 1.5s - 1.0s = 0.5s, got {}", b_gain[0].time_secs);
        let b_bypass = track.clips[1].audio_effect_keyframes.get("0:__bypass")
            .expect("clip_b must carry the bypass kf");
        assert!((b_bypass[0].time_secs - 0.25).abs() < 1e-9,
            "bypass kf time must be 1.25s - 1.0s = 0.25s, got {}", b_bypass[0].time_secs);
    }

    #[test]
    fn blade_with_no_audio_effect_keyframes_leaves_both_halves_empty() {
        // Sanity: clips without audio kfs split into two clips that also
        // have empty maps — no spurious keys, no panics.
        let c0 = make_clip(0, 48, 0, 48);
        let id0 = c0.id;
        let mut track = make_track(vec![c0]);
        track.blade_at(id0, tc(24), test_fps());
        assert!(track.clips[0].audio_effect_keyframes.is_empty());
        assert!(track.clips[1].audio_effect_keyframes.is_empty());
    }

    // ═══════════════════════════════════════════════════════════════
    // Slip (source in/out shift, timeline fixed)
    // ═══════════════════════════════════════════════════════════════
    // Slip is UI-level only (no Track method), but we test the invariants:
    // - timeline_in and timeline_out must NOT change
    // - source_in and source_out shift by the same delta
    // - source_in >= 0, source_out <= media_duration

    #[test]
    fn slip_preserves_timeline_position() {
        // Clip at timeline [24..48], showing source [12..36] of a 72-frame media
        let mut clip = make_clip(24, 48, 12, 36);

        // Slip forward by 6 frames
        let delta = tc(6);
        clip.source_in += delta;
        clip.source_out += delta;

        assert_eq!(clip.timeline_in, tc(24)); // unchanged
        assert_eq!(clip.timeline_out, tc(48)); // unchanged
        assert_eq!(clip.source_in, tc(18));
        assert_eq!(clip.source_out, tc(42));
        assert_eq!(clip.duration(), tc(24)); // same duration
        assert_eq!(clip.source_duration(), tc(24)); // same source duration
    }

    #[test]
    fn slip_clamps_at_source_boundaries() {
        let mut clip = make_clip(0, 24, 0, 24);
        let media_duration = tc(48);

        // Try to slip backward (source_in would go negative)
        let delta = tc(-6);
        let clamped_delta = if clip.source_in + delta < tc(0) {
            tc(0) - clip.source_in
        } else {
            delta
        };
        clip.source_in += clamped_delta;
        clip.source_out += clamped_delta;
        // Should clamp at 0
        assert_eq!(clip.source_in, tc(0));
        assert_eq!(clip.source_out, tc(24));

        // Slip forward to the end
        let delta = tc(30);
        let clamped_delta = if clip.source_out + delta > media_duration {
            media_duration - clip.source_out
        } else {
            delta
        };
        clip.source_in += clamped_delta;
        clip.source_out += clamped_delta;
        assert_eq!(clip.source_in, tc(24));
        assert_eq!(clip.source_out, tc(48));
    }

    // ═══════════════════════════════════════════════════════════════
    // Roll (boundary between two adjacent clips shifts)
    // ═══════════════════════════════════════════════════════════════
    // Roll is UI-level, but we test the invariants:
    // - Clip A's out and Clip B's in move by same delta
    // - Total span A+B is preserved
    // - Neither clip goes below minimum duration

    #[test]
    fn roll_moves_boundary_preserves_total_span() {
        // Two adjacent clips: A=[0..24], B=[24..48]
        // Source: A shows [0..24] of 48-frame media, B shows [0..24] of 48-frame media
        let mut clip_a = make_clip(0, 24, 0, 24);
        let mut clip_b = make_clip(24, 48, 0, 24);

        let total_span = clip_b.timeline_out - clip_a.timeline_in;

        // Roll right by 6 frames: A gets longer, B gets shorter
        let delta = tc(6);
        clip_a.source_out += delta;
        clip_a.timeline_out += delta;
        clip_b.source_in += delta;
        clip_b.timeline_in += delta;

        assert_eq!(clip_a.timeline_out, tc(30));
        assert_eq!(clip_b.timeline_in, tc(30));
        assert_eq!(clip_a.duration(), tc(30));
        assert_eq!(clip_b.duration(), tc(18));
        // Total span preserved
        assert_eq!(clip_b.timeline_out - clip_a.timeline_in, total_span);
    }

    #[test]
    fn roll_clamped_to_minimum_duration() {
        let mut clip_a = make_clip(0, 24, 0, 24);
        let mut clip_b = make_clip(24, 48, 0, 24);

        let min_dur = tc(1); // 1 frame minimum

        // Try to roll right by 30 frames (would make B negative duration)
        let delta = tc(30);
        let max_delta = clip_b.duration() - min_dur; // 23 frames max
        let clamped = if delta > max_delta { max_delta } else { delta };

        clip_a.source_out += clamped;
        clip_a.timeline_out += clamped;
        clip_b.source_in += clamped;
        clip_b.timeline_in += clamped;

        assert_eq!(clip_b.duration(), min_dur); // B is at minimum
        assert_eq!(clip_a.duration(), tc(47)); // A got the rest
    }

    // ═══════════════════════════════════════════════════════════════
    // Slide (clip moves, neighbors absorb the shift)
    // ═══════════════════════════════════════════════════════════════
    // Slide is UI-level. Invariants:
    // - Center clip's source_in/out unchanged, timeline shifts
    // - Left neighbor's out expands/shrinks by delta
    // - Right neighbor's in expands/shrinks by -delta
    // - No gaps between any of the three clips

    #[test]
    fn slide_moves_clip_adjusts_neighbors() {
        // Three adjacent clips: L=[0..24], C=[24..48], R=[48..72]
        // All have source headroom (48 frame sources)
        let mut left = make_clip(0, 24, 0, 24);
        let mut center = make_clip(24, 48, 0, 24);
        let mut right = make_clip(48, 72, 0, 24);

        let orig_center_src_in = center.source_in;
        let orig_center_src_out = center.source_out;

        // Slide center right by 6 frames
        let delta = tc(6);
        // Center moves on timeline
        center.timeline_in += delta;
        center.timeline_out += delta;
        // Left neighbor extends to fill
        left.timeline_out += delta;
        left.source_out += delta;
        // Right neighbor shrinks from the left
        right.timeline_in += delta;
        right.source_in += delta;

        // No gaps
        assert_eq!(left.timeline_out, center.timeline_in);
        assert_eq!(center.timeline_out, right.timeline_in);

        // Center source unchanged
        assert_eq!(center.source_in, orig_center_src_in);
        assert_eq!(center.source_out, orig_center_src_out);

        // Verify positions
        assert_eq!(left.timeline_out, tc(30));
        assert_eq!(center.timeline_in, tc(30));
        assert_eq!(center.timeline_out, tc(54));
        assert_eq!(right.timeline_in, tc(54));
    }

    #[test]
    fn slide_clamped_by_neighbor_minimum_duration() {
        let left = make_clip(0, 24, 0, 48);  // 48-frame source
        let _center = make_clip(24, 48, 0, 24);
        let right = make_clip(48, 72, 0, 48); // 48-frame source

        let min_dur = tc(1);

        // Slide right: limited by right neighbor's minimum duration
        let delta = tc(30);
        let max_right = right.duration() - min_dur; // 23
        // Slide left: limited by left neighbor's minimum duration
        let max_left = left.duration() - min_dur; // 23

        let clamped = delta.min(max_right).min(max_left);
        // Should be 23 frames
        assert_eq!(clamped, tc(23));
    }

    // ═══════════════════════════════════════════════════════════════
    // find_clip / find_clip_mut
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn find_clip_returns_correct_clip() {
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let id0 = c0.id;
        let id1 = c1.id;
        let track = make_track(vec![c0, c1]);

        assert_eq!(track.find_clip(id0).unwrap().timeline_in, tc(0));
        assert_eq!(track.find_clip(id1).unwrap().timeline_in, tc(24));
        assert!(track.find_clip(ClipId::new()).is_none());
    }

    /// #2940: a sound-only source never lands on the dialogue under it
    /// unless the source is patched there: with nothing chosen it goes to
    /// the first free audio track, or none (a new one) when all are taken;
    /// a badge click aims it, alone of its kind while all were targeted.
    #[test]
    fn a_sound_only_source_goes_to_a_free_or_chosen_audio_track() {
        let tc = |s: i64| Timecode::from_secs_f64(s as f64, 24);
        let sound = |from: i64, to: i64| Clip::media(klipp_core::MediaId::new(), tc(from), tc(to), tc(0), tc(to - from));
        let mut seq = Sequence::new_default();
        seq.tracks = vec![Track::new("V1", TrackKind::Video), Track::new("A1", TrackKind::Audio), Track::new("A2", TrackKind::Audio)];
        seq.tracks[1].clips = vec![sound(0, 10)];
        assert_eq!(seq.sound_source_track(tc(2), tc(20)), Some(2), "A1 holds the dialogue: the free A2");
        seq.tracks[2].clips = vec![sound(15, 30)];
        assert_eq!(seq.sound_source_track(tc(2), tc(20)), None, "no audio track free: a new one");
        assert_eq!(seq.sound_source_track(tc(10), tc(15)), Some(1), "free on A1 after its clip");

        // A2's badge clicked while every track is targeted: A2 alone of the
        // audio tracks, and the source patched to it.
        seq.target_from_badge(2);
        assert_eq!((seq.tracks[1].targeted, seq.tracks[2].targeted, seq.tracks[0].targeted), (false, true, true));
        assert!(seq.tracks[2].source_patch);
        assert_eq!(seq.sound_source_track(tc(2), tc(20)), Some(2), "patched there on purpose: A2, taken or not");
        // Clicked again: untargeted, the patch gone; nothing chosen.
        seq.target_from_badge(2);
        assert!(!seq.tracks[2].targeted && !seq.tracks[2].source_patch);
        // A1 targeted alone, without a patch (the track menu): only A1 is
        // aimed at, and it is not free.
        seq.tracks[1].targeted = true;
        seq.tracks[2].targeted = false;
        assert_eq!(seq.sound_source_track(tc(10), tc(14)), Some(1));
        assert_eq!(seq.sound_source_track(tc(2), tc(20)), None, "never over A1's dialogue unpatched");
        // A locked track is not free.
        seq.tracks[1].targeted = true;
        seq.tracks[2].targeted = true;
        seq.tracks[2].clips.clear();
        seq.tracks[2].locked = true;
        assert_eq!(seq.sound_source_track(tc(2), tc(20)), None);
    }

    #[test]
    fn source_edits_go_to_the_patched_track_and_targets_default_to_all() {
        let mut seq = Sequence::new_default();
        seq.tracks = vec![
            Track::new("V2", TrackKind::Video),
            Track::new("V1", TrackKind::Video),
            Track::new("A1", TrackKind::Audio),
            Track::new("A2", TrackKind::Audio),
        ];
        assert_eq!(seq.source_track(TrackKind::Video), Some(0), "no patch: the first of the kind");
        seq.patch_source_to(1);
        seq.patch_source_to(3);
        assert_eq!((seq.source_track(TrackKind::Video), seq.source_track(TrackKind::Audio)), (Some(1), Some(3)));
        seq.patch_source_to(0);
        assert!(!seq.tracks[1].source_patch, "one patch per kind");
        assert!(seq.tracks[3].source_patch, "the other kind's patch stays");

        assert!((0..4).all(|i| seq.is_targeted(i)), "every track is targeted by default");
        seq.tracks[2].targeted = false;
        assert!(!seq.is_targeted(2) && seq.is_targeted(3));
        for t in &mut seq.tracks {
            t.targeted = false;
        }
        assert!((0..4).all(|i| seq.is_targeted(i)), "none targeted acts on all");
    }

    #[test]
    fn clip_at_timecode_finds_correct_clip() {
        let c0 = make_clip(0, 24, 0, 24);
        let c1 = make_clip(24, 48, 0, 24);
        let id0 = c0.id;
        let id1 = c1.id;
        let track = make_track(vec![c0, c1]);

        assert_eq!(track.clip_at_timecode(tc(0)).unwrap().1.id, id0);
        assert_eq!(track.clip_at_timecode(tc(12)).unwrap().1.id, id0);
        assert_eq!(track.clip_at_timecode(tc(24)).unwrap().1.id, id1);
        assert!(track.clip_at_timecode(tc(48)).is_none()); // past end
    }

    // ─── Time Remap helpers ────────────────────────────────────────

    fn kf(t: f64, v: f32) -> klipp_core::Keyframe {
        klipp_core::Keyframe {
            time_secs: t,
            value: v,
            interpolation: klipp_core::keyframe::Interpolation::Linear,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
            handles: Default::default(),
        }
    }

    #[test]
    fn constant_speed_no_keyframes_returns_base_speed() {
        // No keyframes → speed_at_keyframes returns the default.
        assert_eq!(speed_at_keyframes(&[], 0.0, 1.0), 1.0);
        assert_eq!(speed_at_keyframes(&[], 5.0, 1.0), 1.0);
        assert_eq!(speed_at_keyframes(&[], 5.0, 2.5), 2.5);

        // And the integral is just speed * duration.
        let src = speed_integral_keyframes(&[], 4.0, 60.0, 2.0);
        assert!((src - 8.0).abs() < 1e-9, "got {src}");
    }

    #[test]
    fn ramp_up_integrates_correctly() {
        // Linear ramp from 0.5× at t=0 to 2.0× at t=10.
        // Integral from 0 to 5 must be between the min-constant and max-constant
        // bounds (0.5*5 = 2.5 and 2.0*5 = 10) and close to the exact value for
        // a linear ramp: mean speed over [0,5] is (0.5 + 1.25)/2 = 0.875,
        // giving an integral of 4.375.
        let kfs = vec![kf(0.0, 0.5), kf(10.0, 2.0)];
        let src = speed_integral_keyframes(&kfs, 5.0, 60.0, 1.0);
        assert!(src > 2.5, "expected > min-constant; got {src}");
        assert!(src < 10.0, "expected < max-constant; got {src}");
        assert!((src - 4.375).abs() < 0.05, "expected ~4.375, got {src}");
    }

    #[test]
    fn speed_clamped_to_minimum() {
        // A keyframe at 0.0× would stall the clock; the integrator must clamp to SPEED_MIN.
        let kfs = vec![kf(0.0, 0.0), kf(10.0, 0.0)];
        let src = speed_integral_keyframes(&kfs, 10.0, 60.0, 1.0);
        // Expected: SPEED_MIN * 10.0
        let expected = SPEED_MIN * 10.0;
        assert!((src - expected).abs() < 1e-6, "expected {expected}, got {src}");
    }

    #[test]
    fn speed_clamped_to_maximum() {
        // A keyframe at 999× must clamp to SPEED_MAX.
        let kfs = vec![kf(0.0, 999.0), kf(10.0, 999.0)];
        let src = speed_integral_keyframes(&kfs, 1.0, 60.0, 1.0);
        let expected = SPEED_MAX * 1.0;
        assert!((src - expected).abs() < 1e-6, "expected {expected}, got {src}");
    }

    #[test]
    fn effective_timeline_duration_constant_speed() {
        // 10s of source at 2× plays back in 5s of timeline.
        let t = effective_timeline_duration_for_keyframes(&[], 10.0, 60.0, 2.0);
        assert!((t - 5.0).abs() < 1e-6, "got {t}");

        // 10s of source at 0.5× takes 20s of timeline.
        let t = effective_timeline_duration_for_keyframes(&[], 10.0, 60.0, 0.5);
        assert!((t - 20.0).abs() < 1e-6, "got {t}");
    }

    #[test]
    fn effective_timeline_duration_roundtrips_integral() {
        // For any curve: integrating over the computed timeline duration should
        // recover the source duration we started with (within integrator error).
        let kfs = vec![kf(0.0, 0.5), kf(10.0, 2.0)];
        let source_dur = 12.0;
        let t = effective_timeline_duration_for_keyframes(&kfs, source_dur, 60.0, 1.0);
        let back = speed_integral_keyframes(&kfs, t, 60.0, 1.0);
        assert!((back - source_dur).abs() < 0.05, "roundtrip {source_dur} → t={t} → src={back}");
    }

    // ═══════════════════════════════════════════════════════════════
    // Playhead-proximity clip lookup (Q/W trim-to-playhead helpers)
    // ═══════════════════════════════════════════════════════════════

    fn qw_track() -> Track {
        // Three clips with a gap: [0..24], [24..48], gap, [72..96]
        make_track(vec![
            make_clip(0, 24, 0, 24),
            make_clip(24, 48, 0, 24),
            make_clip(72, 96, 0, 24),
        ])
    }

    #[test]
    fn clip_with_out_at_or_before_inside_clip() {
        let t = qw_track();
        // Playhead at frame 12 is inside clip 0 → returns clip 0.
        let (idx, c) = t.clip_with_out_at_or_before(&tc(12)).expect("some");
        assert_eq!(idx, 0);
        assert_eq!(c.timeline_in, tc(0));
    }

    #[test]
    fn clip_with_out_at_or_before_on_gap() {
        let t = qw_track();
        // Playhead at frame 60 is in the gap between clip 1 (ends at 48)
        // and clip 2 (starts at 72) → returns clip 1 (the one before).
        let (idx, c) = t.clip_with_out_at_or_before(&tc(60)).expect("some");
        assert_eq!(idx, 1);
        assert_eq!(c.timeline_out, tc(48));
    }

    #[test]
    fn clip_with_out_at_or_before_before_first_clip() {
        // Single-clip track starting at frame 24; nothing before.
        let t = make_track(vec![make_clip(24, 48, 0, 24)]);
        assert!(t.clip_with_out_at_or_before(&tc(0)).is_none());
    }

    #[test]
    fn clip_with_in_at_or_after_inside_clip() {
        let t = qw_track();
        // Playhead at frame 30 is inside clip 1 → returns clip 1.
        let (idx, c) = t.clip_with_in_at_or_after(&tc(30)).expect("some");
        assert_eq!(idx, 1);
        assert_eq!(c.timeline_in, tc(24));
    }

    #[test]
    fn clip_with_in_at_or_after_on_gap() {
        let t = qw_track();
        // Playhead at frame 60 is in the gap between clip 1 and clip 2 →
        // returns clip 2 (the one after).
        let (idx, c) = t.clip_with_in_at_or_after(&tc(60)).expect("some");
        assert_eq!(idx, 2);
        assert_eq!(c.timeline_in, tc(72));
    }

    #[test]
    fn clip_with_in_at_or_after_after_last_clip() {
        let t = qw_track();
        // Playhead well past the last clip (which ends at 96).
        assert!(t.clip_with_in_at_or_after(&tc(200)).is_none());
    }

    // ─── source_time_for_sequence_time / sequence_time_for_source_time ──

    /// Convert a Timecode → frame count at RATE for ergonomic
    /// assertions. Production code can use `to_secs_f64()` directly.
    fn tc_to_frames(t: Timecode) -> i64 {
        (t.to_secs_f64() * RATE as f64).round() as i64
    }

    /// #617: a reversed clip shows its range's last frame first and its
    /// first frame last, a frame a frame at full speed, each frame twice
    /// at half; the inverse finds the timeline frame a source frame is
    /// shown on.
    #[test]
    fn a_reversed_clip_maps_its_range_backward() {
        let fps = RATE as f64;
        let frame_at = |clip: &Clip, k: i64| (clip.source_secs_at(k as f64 / fps, fps) * fps + 1e-6).floor() as i64;
        // Timeline [24, 72), source [10, 58): 48 frames, reversed.
        let mut clip = make_clip(24, 72, 10, 58);
        clip.speed = -1.0;
        assert_eq!(frame_at(&clip, 0), 57, "the first timeline frame is the range's last");
        assert_eq!(frame_at(&clip, 47), 10, "the last is its first");
        assert!((1..48).all(|k| frame_at(&clip, k) == frame_at(&clip, k - 1) - 1), "one frame back per frame");
        for k in [0, 5, 47] {
            let src = clip.source_secs_at(k as f64 / fps, fps);
            assert!((clip.elapsed_for_source_secs(src, fps) * fps - k as f64).abs() < 1e-6, "inverse at {k}");
        }
        // Half speed: 96 timeline frames, each source frame twice.
        let mut slow = make_clip(0, 96, 10, 58);
        slow.speed = -0.5;
        let shown: Vec<i64> = (0..96).map(|k| frame_at(&slow, k)).collect();
        let want: Vec<i64> = (10..58).rev().flat_map(|n| [n, n]).collect();
        assert_eq!(shown, want);
        // Forward is as it was.
        let fwd = make_clip(24, 72, 10, 58);
        assert_eq!((frame_at(&fwd, 0), frame_at(&fwd, 47)), (10, 57));
    }

    /// #617: a split cuts the source where the clip shows it — through its
    /// speed, and from the far end when it plays reversed, whose earlier
    /// half is the later part of its range.
    #[test]
    fn a_split_cuts_the_source_where_the_clip_shows_it() {
        let split = |speed: f64, src: (i64, i64), len: i64| {
            let mut c = make_clip(0, len, src.0, src.1);
            c.speed = speed;
            let id = c.id;
            let mut t = make_track(vec![c]);
            let (a, b) = t.split_at(id, tc(12)).expect("splits");
            ((tc_to_frames(a.source_in), tc_to_frames(a.source_out)), (tc_to_frames(b.source_in), tc_to_frames(b.source_out)))
        };
        assert_eq!(split(1.0, (0, 48), 48), ((0, 12), (12, 48)));
        assert_eq!(split(2.0, (0, 48), 24), ((0, 24), (24, 48)), "half the clip at 2x is 24 frames of source");
        assert_eq!(split(-1.0, (0, 48), 48), ((36, 48), (0, 36)), "reversed: the first half is the range's end");
    }

    #[test]
    fn match_frame_constant_speed_unity_round_trips() {
        // Clip on timeline frames [24, 72), source [0, 48), speed 1.0.
        // Mid-clip (frame 48 sequence ↔ frame 24 source) round-trips.
        let clip = make_clip(24, 72, 0, 48);
        let src = clip.source_time_for_sequence_time(tc(48), 24.0).unwrap();
        assert_eq!(tc_to_frames(src), 24, "1×: seq frame 48 → src frame 24");
        let back = clip.sequence_time_for_source_time(src, 24.0).unwrap();
        assert_eq!(tc_to_frames(back), 48, "round-trip preserves seq frame 48");
    }

    #[test]
    fn match_frame_constant_speed_2x_doubles_source_offset() {
        // Clip on timeline [24, 48), source [10, 58), speed 2.0
        // (24-frame timeline span covers 48 source frames).
        let mut clip = make_clip(24, 48, 10, 58);
        clip.speed = 2.0;
        // Sequence frame 36 (= 12 frames into the clip on timeline) →
        // source frame 10 + 24 = 34 at 2× speed.
        let src = clip.source_time_for_sequence_time(tc(36), 24.0).unwrap();
        assert_eq!(tc_to_frames(src), 34, "2×: seq frame 36 → src frame 34");
        let back = clip.sequence_time_for_source_time(src, 24.0).unwrap();
        assert_eq!(tc_to_frames(back), 36, "2× round-trip preserves seq frame 36");
    }

    #[test]
    fn match_frame_constant_speed_half_halves_source_offset() {
        // Speed 0.5 — clip plays 24 source frames over 48 timeline
        // frames. Sequence frame 48 (= 24 frames into clip) → source
        // frame 0 + 12 = 12.
        let mut clip = make_clip(24, 72, 0, 24);
        clip.speed = 0.5;
        let src = clip.source_time_for_sequence_time(tc(48), 24.0).unwrap();
        assert_eq!(tc_to_frames(src), 12, "0.5×: seq frame 48 → src frame 12");
        let back = clip.sequence_time_for_source_time(src, 24.0).unwrap();
        assert_eq!(tc_to_frames(back), 48, "0.5× round-trip preserves seq frame 48");
    }

    #[test]
    fn match_frame_outside_clip_returns_none() {
        // Both directions must reject out-of-range times. Match Frame's
        // caller relies on this to skip non-matching tracks instead of
        // wrapping silently to a clamped value.
        let clip = make_clip(24, 72, 0, 48);
        assert!(clip.source_time_for_sequence_time(tc(0), 24.0).is_none(),
            "before timeline_in must be None");
        assert!(clip.source_time_for_sequence_time(tc(72), 24.0).is_none(),
            "at timeline_out (exclusive) must be None");
        assert!(clip.source_time_for_sequence_time(tc(120), 24.0).is_none(),
            "after timeline_out must be None");
        assert!(clip.sequence_time_for_source_time(tc(48), 24.0).is_none(),
            "at source_out (exclusive) must be None");
        assert!(clip.sequence_time_for_source_time(tc(60), 24.0).is_none(),
            "after source_out must be None");
    }

    #[test]
    fn match_frame_variable_speed_round_trips_within_one_frame() {
        // A 2-frame ramp from 0.5× to 2.0× over a 48-frame clip.
        // Round-trip drift is bounded by the integration step at the
        // project fps (~1/24 s) — well within one frame.
        let mut clip = make_clip(0, 48, 0, 48);
        clip.speed_keyframes = vec![kf(0.0, 0.5), kf(2.0, 2.0)];
        for seq_frame in [6, 12, 24, 36] {
            let src = clip.source_time_for_sequence_time(tc(seq_frame), 24.0)
                .expect("in-range");
            let back = clip.sequence_time_for_source_time(src, 24.0)
                .expect("inverse in-range");
            let drift = (tc_to_frames(back) - seq_frame).abs();
            assert!(
                drift <= 1,
                "seq frame {seq_frame} drifted by {drift} frames in round-trip \
                 ({} → {} → {})",
                seq_frame, tc_to_frames(src), tc_to_frames(back),
            );
        }
    }

    #[test]
    fn match_frame_at_clip_start_lands_on_source_in() {
        // The clip's first sequence frame must map exactly to source_in
        // — that's the contract that lets Match Frame land on a
        // discoverable head frame in the source viewer.
        let mut clip = make_clip(120, 168, 36, 84);
        clip.speed = 1.5;
        let src = clip.source_time_for_sequence_time(tc(120), 24.0).unwrap();
        assert_eq!(tc_to_frames(src), 36, "first seq frame must hit source_in");
    }

    // ═════════════════════════════════════════════════════════════════
    // Swap-adjacent-clips (GAP-EDIT-012)
    // ═════════════════════════════════════════════════════════════════

    fn make_swap_seq() -> Sequence {
        Sequence {
            title_nits: crate::DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            buses: Vec::new(),
            drop_frame: false,
            camera: Default::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            id: SequenceId::new(),
            name: "test".to_string(),
            fps: Fps::new(24, 1),
            resolution: Resolution::hd_1080(),
            first_clip_autoset: Default::default(),
            tracks: vec![make_track(vec![])],
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
            awaiting_refit: false,
            version_of: None,
        }
    }

    fn frames_of(c: &Clip) -> (i64, i64) {
        (c.timeline_in.value, c.timeline_out.value)
    }

    #[test]
    fn swap_two_adjacent_clips_exchanges_positions() {
        // A: [0..48], B: [48..96] adjacent on V1.
        // After swap: B should land at [0..48], A at [48..96] —
        // identical durations, so swap is also a no-op visually,
        // but the IDs prove the exchange.
        let mut seq = make_swap_seq();
        let a = make_clip(0, 48, 0, 48);
        let b = make_clip(48, 96, 0, 48);
        let a_id = a.id;
        let b_id = b.id;
        seq.tracks[0].clips.push(a);
        seq.tracks[0].clips.push(b);

        let result = seq.try_swap_adjacent_clips(a_id, b_id);
        assert!(result.is_ok());

        // After swap, B is the leftmost clip.
        let leftmost = &seq.tracks[0].clips[0];
        assert_eq!(leftmost.id, b_id);
        assert_eq!(frames_of(leftmost), (0, 48));
        let rightmost = &seq.tracks[0].clips[1];
        assert_eq!(rightmost.id, a_id);
        assert_eq!(frames_of(rightmost), (48, 96));
    }

    #[test]
    fn swap_preserves_total_span_with_unequal_durations() {
        // A: [0..24] (1s), B: [24..96] (3s). Combined span 0..96.
        // After swap: B at [0..72], A at [72..96]. Total preserved.
        let mut seq = make_swap_seq();
        let a = make_clip(0, 24, 0, 24);
        let b = make_clip(24, 96, 0, 72);
        let a_id = a.id;
        let b_id = b.id;
        seq.tracks[0].clips.push(a);
        seq.tracks[0].clips.push(b);

        seq.try_swap_adjacent_clips(a_id, b_id).expect("swap ok");

        // B is now leftmost with B's duration.
        let new_b = seq.tracks[0].clips.iter().find(|c| c.id == b_id).unwrap();
        assert_eq!(frames_of(new_b), (0, 72));
        let new_a = seq.tracks[0].clips.iter().find(|c| c.id == a_id).unwrap();
        assert_eq!(frames_of(new_a), (72, 96));
        // Span unchanged: leftmost.in == 0, rightmost.out == 96.
        assert_eq!(seq.tracks[0].clips.first().unwrap().timeline_in.value, 0);
        assert_eq!(seq.tracks[0].clips.last().unwrap().timeline_out.value, 96);
    }

    #[test]
    fn swap_rejects_non_adjacent_clips() {
        // A: [0..24], B: [48..72] — gap of 24 frames between them.
        let mut seq = make_swap_seq();
        let a = make_clip(0, 24, 0, 24);
        let b = make_clip(48, 72, 0, 24);
        let a_id = a.id;
        let b_id = b.id;
        seq.tracks[0].clips.push(a);
        seq.tracks[0].clips.push(b);

        let err = seq.try_swap_adjacent_clips(a_id, b_id);
        assert!(err.is_err());
        // Positions unchanged.
        let a_after = seq.tracks[0].clips.iter().find(|c| c.id == a_id).unwrap();
        let b_after = seq.tracks[0].clips.iter().find(|c| c.id == b_id).unwrap();
        assert_eq!(frames_of(a_after), (0, 24));
        assert_eq!(frames_of(b_after), (48, 72));
    }

    #[test]
    fn swap_rejects_clips_on_different_tracks() {
        let mut seq = make_swap_seq();
        seq.tracks.push(Track::new("V2", TrackKind::Video));
        let a = make_clip(0, 24, 0, 24);
        let b = make_clip(0, 24, 0, 24);
        let a_id = a.id;
        let b_id = b.id;
        seq.tracks[0].clips.push(a);
        seq.tracks[1].clips.push(b);

        let err = seq.try_swap_adjacent_clips(a_id, b_id);
        assert!(err.is_err());
    }

    #[test]
    fn swap_rejects_clips_in_same_group() {
        let mut seq = make_swap_seq();
        let mut a = make_clip(0, 24, 0, 24);
        let mut b = make_clip(24, 48, 0, 24);
        let gid = klipp_core::GroupId::new();
        a.group_id = Some(gid);
        b.group_id = Some(gid);
        let a_id = a.id;
        let b_id = b.id;
        seq.tracks[0].clips.push(a);
        seq.tracks[0].clips.push(b);

        let err = seq.try_swap_adjacent_clips(a_id, b_id);
        assert!(err.is_err());
    }

    #[test]
    fn swap_rejects_asymmetric_linked_partners() {
        // A has a partner, B doesn't — should refuse.
        let mut seq = make_swap_seq();
        seq.tracks.push(Track::new("A1", TrackKind::Audio));
        let mut a = make_clip(0, 24, 0, 24);
        let b = make_clip(24, 48, 0, 24);
        let partner = make_clip(0, 24, 0, 24);
        a.linked_clip_id = Some(partner.id);
        let a_id = a.id;
        let b_id = b.id;
        seq.tracks[0].clips.push(a);
        seq.tracks[0].clips.push(b);
        seq.tracks[1].clips.push(partner);

        let err = seq.try_swap_adjacent_clips(a_id, b_id);
        assert!(err.is_err());
    }

    #[test]
    fn swap_handles_linked_partner_pair() {
        // A and B both have linked partners pa, pb on track 1.
        // Both pairs should swap together so the A/V phase lock
        // is preserved.
        let mut seq = make_swap_seq();
        seq.tracks.push(Track::new("A1", TrackKind::Audio));
        let mut a = make_clip(0, 24, 0, 24);
        let mut b = make_clip(24, 48, 0, 24);
        let mut pa = make_clip(0, 24, 0, 24);
        let mut pb = make_clip(24, 48, 0, 24);
        a.linked_clip_id = Some(pa.id);
        b.linked_clip_id = Some(pb.id);
        pa.linked_clip_id = Some(a.id);
        pb.linked_clip_id = Some(b.id);
        let a_id = a.id;
        let b_id = b.id;
        let pa_id = pa.id;
        let pb_id = pb.id;
        seq.tracks[0].clips.push(a);
        seq.tracks[0].clips.push(b);
        seq.tracks[1].clips.push(pa);
        seq.tracks[1].clips.push(pb);

        let result = seq.try_swap_adjacent_clips(a_id, b_id).expect("swap ok");
        assert!(result.partners.is_some());

        // Primaries swapped on V1.
        let v1 = &seq.tracks[0];
        assert_eq!(v1.clips[0].id, b_id);
        assert_eq!(v1.clips[1].id, a_id);
        // Partners swapped on A1.
        let a1 = &seq.tracks[1];
        assert_eq!(a1.clips[0].id, pb_id);
        assert_eq!(a1.clips[1].id, pa_id);
    }

    #[test]
    fn swap_returns_pre_swap_positions_for_history() {
        // The SwapResult must preserve original positions keyed by
        // the user's clip_a / clip_b identity (regardless of which
        // ended up "left").
        let mut seq = make_swap_seq();
        let a = make_clip(0, 24, 0, 24);
        let b = make_clip(24, 96, 0, 72);
        let a_id = a.id;
        let b_id = b.id;
        seq.tracks[0].clips.push(a);
        seq.tracks[0].clips.push(b);

        let result = seq.try_swap_adjacent_clips(a_id, b_id).expect("swap ok");
        // a was [0..24] before swap.
        assert_eq!(result.a_old_in.value, 0);
        assert_eq!(result.a_old_out.value, 24);
        // b was [24..96] before swap.
        assert_eq!(result.b_old_in.value, 24);
        assert_eq!(result.b_old_out.value, 96);

        // Caller can now reverse the swap by writing these back —
        // proves the snapshot is sufficient for undo.
        seq.set_clip_position(a_id, result.a_old_in, result.a_old_out);
        seq.set_clip_position(b_id, result.b_old_in, result.b_old_out);
        let a_after = seq.tracks[0].clips.iter().find(|c| c.id == a_id).unwrap();
        let b_after = seq.tracks[0].clips.iter().find(|c| c.id == b_id).unwrap();
        assert_eq!(frames_of(a_after), (0, 24));
        assert_eq!(frames_of(b_after), (24, 96));
    }
}

/// A numbered default renumbered `n` in the style it had: "V1" stays a
/// letter and a number, "Video 1" a word. Turned into the word form, a
/// track added and taken off again came back with another name (#2802).
fn renumbered(name: &str, word: &str, letter: char, n: u32) -> String {
    if name.starts_with(word) { format!("{word} {n}") } else { format!("{letter}{n}") }
}

/// Whether `name` is a numbered default a renumbering may replace:
/// `"<word> <n>"` or `"<letter><n>"`. A name the user gave, even one that
/// begins with the same letter ("Ambience", "Voice-over"), is not: by
/// first letter alone, every add or remove of a track renamed them (#497).
fn is_default_track_name(name: &str, word: &str, letter: char) -> bool {
    let digits = |rest: &str| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit());
    name.strip_prefix(word).and_then(|r| r.strip_prefix(' ')).is_some_and(digits) || name.strip_prefix(letter).is_some_and(digits)
}

#[cfg(test)]
mod track_name_tests {
    use super::*;

    /// #497: renumbering renames only the numbered defaults, and (#2802)
    /// each in the style it had, a letter or a word, so a track added and
    /// taken off again leaves the names as they were.
    #[test]
    fn a_track_the_user_named_keeps_its_name() {
        let mut seq = Sequence::new_default();
        let names = [("V2", TrackKind::Video), ("Vlog", TrackKind::Video), ("Ambience", TrackKind::Audio), ("A1", TrackKind::Audio), ("Audio 7", TrackKind::Audio), ("Text 9", TrackKind::Text), ("Titles", TrackKind::Text)];
        seq.tracks = names.iter().map(|(n, k)| Track::new(n, *k)).collect();
        seq.renumber_tracks();
        let got: Vec<&str> = seq.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(got, ["V2", "Vlog", "Ambience", "A2", "Audio 3", "Text 1", "Titles"]);
    }
}

#[cfg(test)]
mod primary_holes_tests {
    use super::*;

    /// A primary saved with holes (before #3060 filled them) gets a gap
    /// clip for each when it is opened, in the clips' own rate; one
    /// without holes is left as it is.
    #[test]
    fn holes_on_the_primary_are_filled_on_open() {
        let t = |s: f64| RationalTime::from_secs_f64(s, 24);
        let mut seq = Sequence::new_default();
        let p = seq.primary_track_index().unwrap();
        let c = |a: f64, b: f64| Clip::media(klipp_core::MediaId::new(), t(a), t(b), t(0.0), t(b - a));
        seq.tracks[p].clips = vec![c(0.5, 4.0), c(14.0, 18.0), c(18.0, 22.0)];
        assert_eq!(seq.fill_primary_holes(), 2);
        let spans: Vec<(bool, RationalTime, RationalTime)> = seq.tracks[p].clips.iter().map(|c| (c.kind.is_gap(), c.timeline_in, c.timeline_out)).collect();
        assert_eq!(spans[0], (true, t(0.0), t(0.5)));
        assert_eq!(spans[2], (true, t(4.0), t(14.0)));
        #[cfg(debug_assertions)]
        seq.assert_primary_gapless();
        assert_eq!(seq.fill_primary_holes(), 0, "nothing left to fill");
    }
}

#[cfg(test)]
mod mend_overlaps_tests {
    use super::*;

    fn at(value: i64, rate: u32) -> Timecode {
        Timecode { value, rate }
    }

    /// #3230 as saved: a clip appended at its file's length (473 frames at
    /// 29.97, 15.782 s) on a 30 fps sequence runs over the next clip, which
    /// starts on frame 473; its linked sound already ends there. Opened,
    /// the clip ends where the next starts, its source cut by as much, and
    /// nothing else changes.
    #[test]
    fn a_clip_saved_over_the_next_one_ends_where_it_starts() {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(30, 1);
        seq.tracks.clear();
        seq.add_video_track("V1".into());
        seq.add_audio_track("A1".into());
        let media = MediaId::new();
        let long = at(473_473, 30_000);
        let frame_473 = at(473, 30);
        let mut a = Clip::media(media, at(0, 30_000), long, at(0, 30_000), long);
        let b = Clip::media(media, frame_473, at(600, 30), at(0, 30), at(127, 30));
        let mut sound = Clip::media(media, at(0, 30), frame_473, at(0, 30), frame_473);
        a.linked_clip_id = Some(sound.id);
        sound.linked_clip_id = Some(a.id);
        let (a_id, b_before) = (a.id, b.clone());
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let s = seq.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        seq.tracks[v].clips = vec![a, b];
        seq.tracks[s].clips = vec![sound.clone()];

        assert_eq!(seq.mend_overlaps(), 1, "the one clip over the next");
        let a = seq.tracks[v].find_clip(a_id).unwrap();
        assert!(a.timeline_out.cmp(&frame_473).is_eq(), "ends where the next starts: {:?}", a.timeline_out);
        assert!((a.source_out.to_secs_f64() - 473.0 / 30.0).abs() < 1e-4, "its source cut by as much: {:?}", a.source_out);
        assert_eq!((seq.tracks[v].clips[1].timeline_in, seq.tracks[v].clips[1].timeline_out, seq.tracks[v].clips[1].source_out), (b_before.timeline_in, b_before.timeline_out, b_before.source_out), "the next clip as it was");
        assert_eq!((seq.tracks[s].clips[0].timeline_out, seq.tracks[s].clips[0].source_out), (sound.timeline_out, sound.source_out), "the sound, already on the frame, as it was");
        assert_eq!(seq.mend_overlaps(), 0, "mended once");
    }

    /// A hostile project's times, far past any file, are mended without a
    /// panic.
    #[test]
    fn absurd_saved_times_are_mended_without_a_panic() {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::ntsc_29970();
        seq.tracks.clear();
        seq.add_video_track("V1".into());
        let media = MediaId::new();
        let mut a = Clip::media(media, at(0, 1), at(i64::MAX / 2, 7), at(0, 1), at(i64::MAX / 3, 3));
        a.speed = 1e300;
        let b = Clip::media(media, at(10, 1), at(i64::MAX / 2, 1), at(0, 1), at(1, 1));
        seq.tracks[0].clips = vec![a, b];
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| seq.clone().mend_overlaps())).expect("no panic");
    }

    /// The clip's linked sound that ended with it is ended with it.
    #[test]
    fn a_linked_clip_that_ended_with_it_is_mended_too() {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(30, 1);
        seq.tracks.clear();
        seq.add_video_track("V1".into());
        seq.add_audio_track("A1".into());
        let media = MediaId::new();
        let long = at(473_473, 30_000);
        let mut a = Clip::media(media, at(0, 30_000), long, at(0, 30_000), long);
        let b = Clip::media(media, at(473, 30), at(600, 30), at(0, 30), at(127, 30));
        let mut sound = Clip::media(media, at(0, 30_000), long, at(0, 30_000), long);
        a.linked_clip_id = Some(sound.id);
        sound.linked_clip_id = Some(a.id);
        let sound_id = sound.id;
        let v = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let s = seq.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        seq.tracks[v].clips = vec![a, b];
        seq.tracks[s].clips = vec![sound];
        assert_eq!(seq.mend_overlaps(), 2);
        assert!(seq.tracks[s].find_clip(sound_id).unwrap().timeline_out.cmp(&at(473, 30)).is_eq());
    }
}

#[cfg(test)]
mod frozen_track_tests {
    use super::*;

    /// #3207: a track saved before freezing existed opens live; a live one
    /// saves no `frozen`; a frozen one opens frozen.
    #[test]
    fn frozen_is_saved_only_when_on() {
        let live = Track::new("V1", TrackKind::Video);
        let json = serde_json::to_value(&live).unwrap();
        assert!(json.get("frozen").is_none(), "a live track says nothing of it");
        let back: Track = serde_json::from_value(json).unwrap();
        assert!(!back.frozen, "an old project's track opens live");
        let mut frozen = live.clone();
        frozen.frozen = true;
        let back: Track = serde_json::from_value(serde_json::to_value(&frozen).unwrap()).unwrap();
        assert!(back.frozen);
    }
}
