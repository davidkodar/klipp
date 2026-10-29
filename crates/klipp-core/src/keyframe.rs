use serde::{Deserialize, Serialize};

/// Which spatial axis of a title-object position is being keyframed.
/// Used by the per-layer position keyframe AppCommands so handlers
/// can `match` exhaustively rather than reading magic numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TitlePositionAxis {
    X,
    Y,
}

/// A separately animatable property of a title layer transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TitleTransformProperty {
    Width,
    Height,
    Rotation,
    AnchorX,
    AnchorY,
}

/// Interpolation method between keyframes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Interpolation {
    /// Linear interpolation between values.
    #[default]
    Linear,
    /// Hold the previous keyframe's value until the next keyframe.
    Step,
    /// Smooth cubic bezier interpolation (tangents on KeyframePoint).
    Bezier,
    /// A named easing curve over the segment ([`Ease::at`]): the key
    /// menu's presets, elastic and bounce among them, which one cubic
    /// cannot draw. The handles play no part.
    Eased(Ease),
}

impl Interpolation {
    /// A number for the interpolation, for a fingerprint: Linear, Step
    /// and Bezier as their places (0, 1, 2), an eased curve past them by
    /// its curve.
    pub fn code(self) -> u64 {
        match self {
            Interpolation::Linear => 0,
            Interpolation::Step => 1,
            Interpolation::Bezier => 2,
            Interpolation::Eased(ease) => 3 + ease as u64,
        }
    }
}

/// Set the interpolation of the key at `index` as the keyframe editor does:
/// that key and the one before it take `interpolation` (the segment into
/// the key and the one out of it), and for Bezier each of the two gets
/// tangents a third of the way to its neighbours where it has none yet.
/// An index past the end changes only the key before it, as before. An
/// eased curve shapes one move, so it goes to the segment leaving the key
/// alone (the one into it, for the last key).
pub fn set_interpolation_at(keyframes: &mut [Keyframe], index: usize, interpolation: Interpolation) {
    let len = keyframes.len();
    if let Interpolation::Eased(_) = interpolation {
        let segment = if index + 1 < len { index } else { index.saturating_sub(1) };
        if let Some(k) = keyframes.get_mut(segment) {
            k.interpolation = interpolation;
        }
        return;
    }
    if index < len {
        keyframes[index].interpolation = interpolation;
    }
    if index > 0 && index - 1 < len {
        keyframes[index - 1].interpolation = interpolation;
    }
    if interpolation == Interpolation::Bezier {
        for idx in [index.saturating_sub(1), index] {
            if idx >= len {
                continue;
            }
            let prev_t = if idx > 0 { keyframes[idx - 1].time_secs } else { keyframes[idx].time_secs - 1.0 };
            let next_t = if idx + 1 < len { keyframes[idx + 1].time_secs } else { keyframes[idx].time_secs + 1.0 };
            let span_out = (next_t - keyframes[idx].time_secs).abs() * 0.33;
            let span_in = (keyframes[idx].time_secs - prev_t).abs() * 0.33;
            let kf = &mut keyframes[idx];
            if kf.tangent_out == (0.0, 0.0) || kf.tangent_out.0.abs() < 0.5 {
                kf.tangent_out = (span_out, 0.0);
            }
            if kf.tangent_in == (0.0, 0.0) || kf.tangent_in.0.abs() < 0.5 {
                kf.tangent_in = (-span_in, 0.0);
            }
        }
    }
}

/// Which side of a key an ease slows, as a key menu offers it: the move
/// arriving at the key ("Ease In"), the move leaving it ("Ease Out"), or
/// both. Unlike [`Ease`], which shapes one move from rest to rest, this
/// is said of a key, as a grading or compositing app's key menu says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyEase {
    /// Slow into the key.
    In,
    /// Slow out of the key.
    Out,
    /// Slow into and out of the key.
    Both,
}

/// How long an eased key's flat handle is, as a share of its segment: the
/// standard ease's 0.42.
const EASE_INFLUENCE: f64 = 0.42;

/// Ease the key at `index` on `side`: each segment on that side becomes
/// Bezier, the key's handle there flat (the move slows to rest at the key)
/// and `EASE_INFLUENCE` of the segment long. The far key's handle is kept
/// where the segment already was Bezier, else set where a straight line
/// would put it, so the far end moves as it did. A side with no segment
/// (the first key's In, the last key's Out) is left; an index past the
/// end changes nothing.
pub fn ease_key(kfs: &mut [Keyframe], index: usize, side: KeyEase) {
    if index >= kfs.len() {
        return;
    }
    if matches!(side, KeyEase::In | KeyEase::Both) && index > 0 {
        let (a, b) = (index - 1, index);
        let (d, v) = (kfs[b].time_secs - kfs[a].time_secs, kfs[b].value - kfs[a].value);
        if kfs[a].interpolation != Interpolation::Bezier {
            kfs[a].tangent_out = (d / 3.0, v / 3.0);
        }
        kfs[a].interpolation = Interpolation::Bezier;
        kfs[b].tangent_in = (-d * EASE_INFLUENCE, 0.0);
    }
    if matches!(side, KeyEase::Out | KeyEase::Both) && index + 1 < kfs.len() {
        let (a, b) = (index, index + 1);
        let (d, v) = (kfs[b].time_secs - kfs[a].time_secs, kfs[b].value - kfs[a].value);
        if kfs[a].interpolation != Interpolation::Bezier {
            kfs[b].tangent_in = (-d / 3.0, -v / 3.0);
        }
        kfs[a].interpolation = Interpolation::Bezier;
        kfs[a].tangent_out = (d * EASE_INFLUENCE, 0.0);
    }
}

/// A single keyframe: a value at a specific time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Keyframe {
    /// Seconds from the clip's start on the timeline (not into its
    /// file): the clock that stretches with the clip (#3055).
    pub time_secs: f64,
    /// The parameter value at this time.
    pub value: f32,
    /// Interpolation method to the NEXT keyframe.
    pub interpolation: Interpolation,
    /// Bezier out-tangent: (time_delta, value_delta) relative to this keyframe.
    /// Positive time_delta means the handle extends to the right.
    #[serde(default)]
    pub tangent_out: (f64, f32),
    /// Bezier in-tangent: (time_delta, value_delta) relative to this keyframe.
    /// Negative time_delta means the handle extends to the left.
    #[serde(default)]
    pub tangent_in: (f64, f32),
    /// How the key's handles are made: given (`tangent_in`/`tangent_out`
    /// as stored) or worked out from its neighbours when the curve is
    /// evaluated. Unsaved when `Broken`, so a key without it reads as before.
    #[serde(default, skip_serializing_if = "Handles::is_broken")]
    pub handles: Handles,
}

/// How a key's two Bezier handles are made, as a curve editor's key menu
/// sets them. The worked-out kinds are solved from the neighbouring keys
/// each time the curve is evaluated ([`handles_at`]), so they follow a
/// neighbour that moves; the stored tangents are only kept in step for show and for a
/// later switch to a given kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Handles {
    /// Each handle as stored, moved on its own.
    #[default]
    Broken,
    /// As stored, kept in one line: moving one turns the other with it.
    Aligned,
    /// Smooth through the key: both handles along the line from the key
    /// before to the key after.
    Auto,
    /// As `Auto`, but never past the neighbours' values: flat at a peak, a
    /// dip or an end key, and short enough not to overshoot.
    Clamped,
    /// Both handles level: the move comes to rest at the key.
    Flat,
    /// The in-handle aimed straight at the key before; the out-handle as stored.
    LinearIn,
    /// The out-handle aimed straight at the key after; the in-handle as stored.
    LinearOut,
}

impl Handles {
    /// Every kind, in the order a key menu lists them, with its label.
    pub const ALL: [(Handles, &'static str); 7] = [
        (Handles::Auto, "Auto"),
        (Handles::Clamped, "Auto Clamped"),
        (Handles::Aligned, "Aligned"),
        (Handles::Broken, "Broken"),
        (Handles::Flat, "Flat"),
        (Handles::LinearIn, "Linear In"),
        (Handles::LinearOut, "Linear Out"),
    ];

    pub fn is_broken(&self) -> bool {
        *self == Handles::Broken
    }
}

/// The handles of key `i` of `kfs` as the curve is evaluated with them,
/// `(tangent_in, tangent_out)`: the stored ones for `Broken` and
/// `Aligned`, else worked out from the neighbouring keys, each handle a
/// third of its segment long. A side with no neighbour keeps its stored
/// handle (no segment reads it).
pub fn handles_at(kfs: &[Keyframe], i: usize) -> ((f64, f32), (f64, f32)) {
    let k = &kfs[i];
    let prev = i.checked_sub(1).and_then(|p| kfs.get(p));
    let next = kfs.get(i + 1);
    // The handles along `slope` (value per second), a third of each segment.
    let along = |slope: f64| {
        let tin = prev.map_or(k.tangent_in, |p| {
            let d = (k.time_secs - p.time_secs) / 3.0;
            (-d, (-slope * d) as f32)
        });
        let tout = next.map_or(k.tangent_out, |n| {
            let d = (n.time_secs - k.time_secs) / 3.0;
            (d, (slope * d) as f32)
        });
        (tin, tout)
    };
    let slope_to = |a: &Keyframe, b: &Keyframe| {
        let d = b.time_secs - a.time_secs;
        if d.abs() < 1e-12 { 0.0 } else { (b.value - a.value) as f64 / d }
    };
    match k.handles {
        Handles::Broken | Handles::Aligned => (k.tangent_in, k.tangent_out),
        Handles::Flat => along(0.0),
        Handles::Auto => along(match (prev, next) {
            (Some(p), Some(n)) => slope_to(p, n),
            (Some(p), None) => slope_to(p, k),
            (None, Some(n)) => slope_to(k, n),
            (None, None) => 0.0,
        }),
        Handles::Clamped => {
            let (Some(p), Some(n)) = (prev, next) else { return along(0.0) };
            if (k.value - p.value) * (n.value - k.value) <= 0.0 {
                return along(0.0);
            }
            // A handle a third along at this slope stays within its
            // segment's values: no overshoot.
            let limit = (3.0 * slope_to(p, k).abs()).min(3.0 * slope_to(k, n).abs());
            let s = slope_to(p, n);
            along(s.signum() * s.abs().min(limit))
        }
        Handles::LinearIn => {
            let tin = prev.map_or(k.tangent_in, |p| (-(k.time_secs - p.time_secs) / 3.0, -(k.value - p.value) / 3.0));
            (tin, k.tangent_out)
        }
        Handles::LinearOut => {
            let tout = next.map_or(k.tangent_out, |n| ((n.time_secs - k.time_secs) / 3.0, (n.value - k.value) / 3.0));
            (k.tangent_in, tout)
        }
    }
}

/// Give key `index` of `kfs` the handle kind `handles`, as a key menu
/// does: the segments the kind shapes turn Bezier (both, or the one side
/// of a Linear In or Out), a far key's handle on a segment that was not
/// Bezier set where a straight line would put it so that end moves as it
/// did, `Aligned` turns the in-handle into line with the out-handle, and
/// the stored tangents take the handles the curve now evaluates with. An
/// index past the end changes nothing.
pub fn set_handles(kfs: &mut [Keyframe], index: usize, handles: Handles) {
    if index >= kfs.len() {
        return;
    }
    let (into, out_of) = match handles {
        Handles::LinearIn => (true, false),
        Handles::LinearOut => (false, true),
        _ => (true, true),
    };
    if into && index > 0 {
        let (a, b) = (index - 1, index);
        if kfs[a].interpolation != Interpolation::Bezier {
            let (d, v) = (kfs[b].time_secs - kfs[a].time_secs, kfs[b].value - kfs[a].value);
            kfs[a].tangent_out = (d / 3.0, v / 3.0);
            kfs[b].tangent_in = (-d / 3.0, -v / 3.0);
            kfs[a].interpolation = Interpolation::Bezier;
        }
    }
    if out_of && index + 1 < kfs.len() {
        let (a, b) = (index, index + 1);
        if kfs[a].interpolation != Interpolation::Bezier {
            let (d, v) = (kfs[b].time_secs - kfs[a].time_secs, kfs[b].value - kfs[a].value);
            kfs[a].tangent_out = (d / 3.0, v / 3.0);
            kfs[b].tangent_in = (-d / 3.0, -v / 3.0);
            kfs[a].interpolation = Interpolation::Bezier;
        }
    }
    let k = &mut kfs[index];
    if handles == Handles::Aligned {
        let (ti, to) = (k.tangent_in, k.tangent_out);
        if to.0.abs() > 1e-12 && ti.0.abs() > 1e-12 {
            k.tangent_in = (ti.0, (to.1 as f64 * ti.0 / to.0) as f32);
        }
    }
    k.handles = handles;
    let (ti, to) = handles_at(kfs, index);
    kfs[index].tangent_in = ti;
    kfs[index].tangent_out = to;
}

/// A handle edit on key `index`, as the curve editor's handle drag and key
/// menu send it: the kind set first when one is given ([`set_handles`]),
/// then either handle that is given put in place.
pub fn edit_handles(kfs: &mut [Keyframe], index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)>, handles: Option<Handles>) {
    if let Some(h) = handles {
        set_handles(kfs, index, h);
    }
    if let Some(kf) = kfs.get_mut(index) {
        if let Some(ti) = tangent_in {
            kf.tangent_in = ti;
        }
        if let Some(to) = tangent_out {
            kf.tangent_out = to;
        }
    }
}

/// Timestamp tolerance for keyframe identity, in seconds. This absorbs rounding
/// from floating-point arithmetic without merging adjacent video frames.
/// This does not absorb quantization to a video frame.
/// Mouse hit testing should use a separate, pixel-based tolerance.
pub const KEYFRAME_TIME_EPSILON: f64 = 1e-6;

/// Update a key's value without changing its stored time, interpolation or
/// tangents; insert a Linear key when no timestamp matches.
pub fn upsert_keyframe(kfs: &mut Vec<Keyframe>, time_secs: f64, value: f32) {
    upsert_keyframe_with_interpolation(kfs, time_secs, value, Interpolation::Linear);
}

/// The key [`upsert_keyframe`] would leave at `time_secs` in `kfs`: the
/// matching key with `value`, its stored time, interpolation and tangents
/// kept, or a new Linear key. For a write that puts one whole key in place
/// (a path's `AddKeyAt`).
pub fn upserted(kfs: &[Keyframe], time_secs: f64, value: f32) -> Keyframe {
    let mut one: Vec<Keyframe> = kfs.iter().find(|k| (k.time_secs - time_secs).abs() < KEYFRAME_TIME_EPSILON).cloned().into_iter().collect();
    upsert_keyframe(&mut one, time_secs, value);
    one.remove(0)
}

/// As [`upsert_keyframe`], with a default interpolation for newly inserted keys.
/// Existing keys keep their curve shape, including when editing Step parameters.
pub fn upsert_keyframe_with_interpolation(kfs: &mut Vec<Keyframe>, time_secs: f64, value: f32, default: Interpolation) {
    if let Some(key) = kfs.iter_mut().find(|k| (k.time_secs - time_secs).abs() < KEYFRAME_TIME_EPSILON) {
        key.value = value;
        return;
    }
    kfs.push(Keyframe {
        time_secs, value, interpolation: default,
        tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0),
        handles: Default::default(),
    });
    kfs.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
}

/// Remove every keyframe at `time_secs` (within [`KEYFRAME_TIME_EPSILON`]). No-op when
/// nothing matches; matches the NLE-standard "click diamond again to
/// delete" affordance.
pub fn remove_keyframe_at(kfs: &mut Vec<Keyframe>, time_secs: f64) {
    kfs.retain(|k| (k.time_secs - time_secs).abs() >= KEYFRAME_TIME_EPSILON);
}

/// Move the keyframe at `index` to a new (time, value) and re-sort.
/// Returns true if the index was valid.
pub fn move_keyframe(
    kfs: &mut [Keyframe],
    index: usize,
    new_time_secs: f64,
    new_value: f32,
) -> bool {
    let Some(kf) = kfs.get_mut(index) else {
        return false;
    };
    kf.time_secs = new_time_secs;
    kf.value = new_value;
    kfs.sort_by(|a, b| {
        a.time_secs
            .partial_cmp(&b.time_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    true
}

/// Set the interpolation mode of the keyframe at `index`. Returns
/// true if the index was valid.
pub fn set_keyframe_interpolation(
    kfs: &mut [Keyframe],
    index: usize,
    interpolation: Interpolation,
) -> bool {
    if let Some(kf) = kfs.get_mut(index) {
        kf.interpolation = interpolation;
        true
    } else {
        false
    }
}

/// How a move eases into a keyframe: the named curves motion design
/// uses, written as the Bezier handles [`evaluate`] already solves — the
/// same model as CSS `cubic-bezier`, handles as fractions of the
/// segment's time and change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    /// Constant speed: starts and stops dead. Right for a slow drift.
    #[default]
    Linear,
    /// Starts slowly and arrives at speed.
    In,
    /// Leaves at speed and settles: an arrival.
    Out,
    /// Slow at both ends: the natural move from one rest to another.
    InOut,
    /// A fast arrival that settles long: how a card flies into place.
    ExpoOut,
    /// Arrives, overshoots a little and settles back.
    BackOut,
    /// Starts slowly and leaves fast: the reverse of `ExpoOut`.
    ExpoIn,
    /// Slow at both ends, fast through the middle.
    ExpoInOut,
    /// Draws back a little before it goes.
    BackIn,
    /// Draws back, goes, overshoots and settles.
    BackInOut,
    /// Winds up in growing swings before it goes.
    ElasticIn,
    /// Overshoots and swings about the key, settling like a spring.
    ElasticOut,
    /// Winds up, then springs into place.
    ElasticInOut,
    /// Bounces off the start, higher each time, before it goes.
    BounceIn,
    /// Falls onto the key and bounces to rest, as a dropped ball.
    BounceOut,
    /// Bounces off the start and onto the key.
    BounceInOut,
}

impl Ease {
    /// The presets a key menu offers, by family, with their labels:
    /// each family's In, Out and In-Out.
    pub const PRESETS: [(&'static str, [Ease; 3]); 4] = [
        ("Exponential", [Ease::ExpoIn, Ease::ExpoOut, Ease::ExpoInOut]),
        ("Back", [Ease::BackIn, Ease::BackOut, Ease::BackInOut]),
        ("Elastic", [Ease::ElasticIn, Ease::ElasticOut, Ease::ElasticInOut]),
        ("Bounce", [Ease::BounceIn, Ease::BounceOut, Ease::BounceInOut]),
    ];

    /// The curve's two handles, `(x1, y1, x2, y2)`, or `None` for a
    /// straight line or a curve one cubic cannot draw (elastic, bounce).
    pub fn handles(self) -> Option<(f64, f32, f64, f32)> {
        match self {
            Ease::Linear => None,
            Ease::In => Some((0.42, 0.0, 1.0, 1.0)),
            Ease::Out => Some((0.0, 0.0, 0.58, 1.0)),
            Ease::InOut => Some((0.42, 0.0, 0.58, 1.0)),
            Ease::ExpoOut => Some((0.16, 1.0, 0.3, 1.0)),
            Ease::BackOut => Some((0.34, 1.56, 0.64, 1.0)),
            Ease::ExpoIn => Some((0.7, 0.0, 0.84, 0.0)),
            Ease::ExpoInOut => Some((0.87, 0.0, 0.13, 1.0)),
            Ease::BackIn => Some((0.36, 0.0, 0.66, -0.56)),
            Ease::BackInOut => Some((0.68, -0.6, 0.32, 1.6)),
            Ease::ElasticIn | Ease::ElasticOut | Ease::ElasticInOut | Ease::BounceIn | Ease::BounceOut | Ease::BounceInOut => None,
        }
    }

    /// How far along the move is at `t` (0..1 of the segment's time):
    /// 0 at the start, 1 at the end, past either for an overshoot. A
    /// curve with handles is that cubic, solved as [`evaluate`] solves a
    /// Bezier segment; elastic and bounce are the standard closed forms.
    pub fn at(self, t: f64) -> f64 {
        use std::f64::consts::PI;
        let t = t.clamp(0.0, 1.0);
        let bounce_out = |t: f64| {
            let (n, d) = (7.5625, 2.75);
            if t < 1.0 / d {
                n * t * t
            } else if t < 2.0 / d {
                let t = t - 1.5 / d;
                n * t * t + 0.75
            } else if t < 2.5 / d {
                let t = t - 2.25 / d;
                n * t * t + 0.9375
            } else {
                let t = t - 2.625 / d;
                n * t * t + 0.984375
            }
        };
        match self {
            Ease::Linear => t,
            Ease::ElasticIn | Ease::ElasticOut | Ease::ElasticInOut if t <= 0.0 || t >= 1.0 => t,
            Ease::ElasticIn => -(2f64.powf(10.0 * t - 10.0)) * ((10.0 * t - 10.75) * (2.0 * PI / 3.0)).sin(),
            Ease::ElasticOut => 2f64.powf(-10.0 * t) * ((10.0 * t - 0.75) * (2.0 * PI / 3.0)).sin() + 1.0,
            Ease::ElasticInOut => {
                let c = 2.0 * PI / 4.5;
                if t < 0.5 {
                    -(2f64.powf(20.0 * t - 10.0) * ((20.0 * t - 11.125) * c).sin()) / 2.0
                } else {
                    2f64.powf(-20.0 * t + 10.0) * ((20.0 * t - 11.125) * c).sin() / 2.0 + 1.0
                }
            }
            Ease::BounceOut => bounce_out(t),
            Ease::BounceIn => 1.0 - bounce_out(1.0 - t),
            Ease::BounceInOut => {
                if t < 0.5 {
                    (1.0 - bounce_out(1.0 - 2.0 * t)) / 2.0
                } else {
                    (1.0 + bounce_out(2.0 * t - 1.0)) / 2.0
                }
            }
            _ => {
                let (x1, y1, x2, y2) = self.handles().unwrap_or((0.0, 0.0, 1.0, 1.0));
                cubic_at((x1, y1 as f64, x2, y2 as f64), t)
            }
        }
    }
}

/// How far along a segment shaped by the CSS-style cubic `(x1, y1, x2,
/// y2)` the value is at time `t` (0..1): the curve's x solved for `t` by
/// Newton's method, as [`evaluate`] solves a Bezier segment, then its y.
fn cubic_at((x1, y1, x2, y2): (f64, f64, f64, f64), t: f64) -> f64 {
    let bez = |a: f64, b: f64, u: f64| {
        let mu = 1.0 - u;
        3.0 * mu * mu * u * a + 3.0 * mu * u * u * b + u * u * u
    };
    let dx = |u: f64| {
        let mu = 1.0 - u;
        3.0 * mu * mu * x1 + 6.0 * mu * u * (x2 - x1) + 3.0 * u * u * (1.0 - x2)
    };
    let mut u = t;
    for _ in 0..8 {
        let d = dx(u);
        if d.abs() < 1e-12 {
            break;
        }
        u = (u - (bez(x1, x2, u) - t) / d).clamp(0.0, 1.0);
        if (bez(x1, x2, u) - t).abs() < 1e-7 {
            break;
        }
    }
    bez(y1, y2, u)
}

/// Ease the move from keyframe `i - 1` into keyframe `i`. The handles
/// are scaled to that segment, so the same ease means the same shape of
/// move whatever its length or distance.
pub fn ease_into(kfs: &mut [Keyframe], i: usize, ease: Ease) {
    if i == 0 || i >= kfs.len() {
        return;
    }
    let Some((x1, y1, x2, y2)) = ease.handles() else {
        kfs[i - 1].interpolation = if ease == Ease::Linear { Interpolation::Linear } else { Interpolation::Eased(ease) };
        return;
    };
    let d = kfs[i].time_secs - kfs[i - 1].time_secs;
    let v = kfs[i].value - kfs[i - 1].value;
    kfs[i - 1].interpolation = Interpolation::Bezier;
    kfs[i - 1].tangent_out = (x1 * d, y1 * v);
    kfs[i].tangent_in = ((x2 - 1.0) * d, (y2 - 1.0) * v);
}

/// Evaluate a keyframe track at a given time (relative to clip source_in).
/// Returns the interpolated value, or `default` if no keyframes exist.
pub fn evaluate(keyframes: &[Keyframe], time_secs: f64, default: f32) -> f32 {
    if keyframes.is_empty() {
        return default;
    }
    // Before first keyframe: use the first keyframe's value (holds backward)
    if time_secs <= keyframes[0].time_secs {
        return keyframes[0].value;
    }
    // After last keyframe: use last keyframe's value (holds)
    let last = &keyframes[keyframes.len() - 1];
    if time_secs >= last.time_secs {
        return last.value;
    }
    // Find surrounding keyframes and interpolate
    for i in 0..keyframes.len() - 1 {
        let a = &keyframes[i];
        let b = &keyframes[i + 1];
        if time_secs >= a.time_secs && time_secs <= b.time_secs {
            let t = if (b.time_secs - a.time_secs).abs() < 1e-9 {
                0.0
            } else {
                (time_secs - a.time_secs) / (b.time_secs - a.time_secs)
            };
            return match a.interpolation {
                Interpolation::Step => a.value,
                Interpolation::Linear => a.value + (b.value - a.value) * t as f32,
                Interpolation::Eased(ease) => a.value + (b.value - a.value) * ease.at(t) as f32,
                Interpolation::Bezier => {
                    let duration = b.time_secs - a.time_secs;
                    // The handles as each key's kind makes them.
                    let (_, a_out) = handles_at(keyframes, i);
                    let (b_in, _) = handles_at(keyframes, i + 1);

                    // Value control points (Y dimension)
                    let y0 = a.value;
                    let y3 = b.value;
                    let y1 = y0 + a_out.1;
                    let y2 = y3 + b_in.1;

                    // Check if tangent time offsets are effectively zero —
                    // if so, use the simple linear-t evaluation (no Newton-Raphson needed).
                    let tx_out = a_out.0;
                    let tx_in = b_in.0;
                    let has_time_tangents = duration.abs() > 1e-9
                        && (tx_out.abs() > 1e-9 || tx_in.abs() > 1e-9);

                    if !has_time_tangents {
                        // Simple cubic Bezier with linear t
                        let t = t as f32;
                        let mt = 1.0 - t;
                        mt * mt * mt * y0
                            + 3.0 * mt * mt * t * y1
                            + 3.0 * mt * t * t * y2
                            + t * t * t * y3
                    } else {
                        // Full 2D Bezier: solve for parameter u where bezier_x(u) = t_linear,
                        // then evaluate bezier_y(u).
                        //
                        // Normalized time control points (X dimension, 0..1 range):
                        // P0.x = 0, P3.x = 1
                        // P1.x = tangent_out.0 / duration
                        // P2.x = 1 + tangent_in.0 / duration  (tangent_in.0 is negative)
                        let x1 = (tx_out / duration).clamp(0.0, 1.0);
                        let x2 = (1.0 + tx_in / duration).clamp(0.0, 1.0);

                        let t_target = t; // already normalized 0..1

                        // Cubic Bezier X(u) = 3(1-u)^2 u x1 + 3(1-u) u^2 x2 + u^3
                        // (since x0=0 and x3=1)
                        // Derivative X'(u) = 3(1-u)^2 x1 + 6(1-u)u(x2-x1) + 3u^2(1-x2)
                        let bezier_x = |u: f64| -> f64 {
                            let mu = 1.0 - u;
                            3.0 * mu * mu * u * x1
                                + 3.0 * mu * u * u * x2
                                + u * u * u
                        };
                        let bezier_x_deriv = |u: f64| -> f64 {
                            let mu = 1.0 - u;
                            3.0 * mu * mu * x1
                                + 6.0 * mu * u * (x2 - x1)
                                + 3.0 * u * u * (1.0 - x2)
                        };

                        // Newton-Raphson: find u where bezier_x(u) = t_target
                        let mut u = t_target; // initial guess
                        for _ in 0..8 {
                            let x_val = bezier_x(u) - t_target;
                            let x_deriv = bezier_x_deriv(u);
                            if x_deriv.abs() < 1e-12 {
                                break;
                            }
                            u -= x_val / x_deriv;
                            u = u.clamp(0.0, 1.0);
                            // Early exit if close enough
                            if (bezier_x(u) - t_target).abs() < 1e-7 {
                                break;
                            }
                        }
                        u = u.clamp(0.0, 1.0);

                        // Evaluate Y (value) at parameter u
                        let u = u as f32;
                        let mu = 1.0 - u;
                        mu * mu * mu * y0
                            + 3.0 * mu * mu * u * y1
                            + 3.0 * mu * u * u * y2
                            + u * u * u * y3
                    }
                }
            };
        }
    }
    default
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kf(time: f64, value: f32, interp: Interpolation) -> Keyframe {
        Keyframe {
            time_secs: time,
            value,
            interpolation: interp,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
            handles: Default::default(),
        }
    }

    /// A key eased in slows to rest at it: the segment into it is Bezier,
    /// the key's in-handle flat, the far key's out-handle where a straight
    /// line puts it. A quarter of the way along, the move is past the
    /// straight line's quarter (it left at speed); three quarters along it
    /// is short of the line's (it is slowing). Eased out, the segment after
    /// it does the opposite; the first key has no In and the last no Out.
    #[test]
    fn an_eased_key_slows_on_its_side() {
        let line = || vec![kf_linear(0.0, 0.0), kf_linear(2.0, 1.0), kf_linear(4.0, 0.0)];
        let mut keys = line();
        ease_key(&mut keys, 1, KeyEase::In);
        assert_eq!(keys[0].interpolation, Interpolation::Bezier);
        assert_eq!(keys[1].interpolation, Interpolation::Linear, "the segment out of the key is the Out side's");
        assert_eq!(keys[1].tangent_in, (-2.0 * 0.42, 0.0));
        assert_eq!(keys[0].tangent_out, (2.0 / 3.0, 1.0 / 3.0));
        assert!(evaluate(&keys, 0.5, 0.0) > 0.25, "{}", evaluate(&keys, 0.5, 0.0));
        assert!(evaluate(&keys, 1.5, 0.0) > 0.75, "slowing late means arriving early: {}", evaluate(&keys, 1.5, 0.0));
        assert!((evaluate(&keys, 3.0, 0.0) - 0.5).abs() < 1e-6, "the next segment is untouched");

        let mut keys = line();
        ease_key(&mut keys, 1, KeyEase::Out);
        assert_eq!(keys[0].interpolation, Interpolation::Linear);
        assert_eq!(keys[1].interpolation, Interpolation::Bezier);
        assert_eq!(keys[1].tangent_out, (2.0 * 0.42, 0.0));
        assert!(evaluate(&keys, 2.5, 0.0) > 0.75, "leaving slowly stays near the key: {}", evaluate(&keys, 2.5, 0.0));

        let mut keys = line();
        ease_key(&mut keys, 1, KeyEase::Both);
        assert_eq!((keys[0].interpolation, keys[1].interpolation), (Interpolation::Bezier, Interpolation::Bezier));

        let mut keys = line();
        ease_key(&mut keys, 0, KeyEase::In);
        ease_key(&mut keys, 2, KeyEase::Out);
        ease_key(&mut keys, 3, KeyEase::Both);
        assert_eq!(keys, line(), "no segment on that side, no change");
    }

    /// A key saved without handles reads as Broken (its tangents as
    /// stored), and a Broken key is saved without the field, so a file
    /// is written as before; any other kind is saved and read back.
    #[test]
    fn handles_are_saved_only_when_not_broken() {
        let key: Keyframe = serde_json::from_str(r#"{"time_secs":1.0,"value":2.0,"interpolation":"Bezier","tangent_out":[0.5,0.0],"tangent_in":[-0.5,0.0]}"#).unwrap();
        assert_eq!(key.handles, Handles::Broken);
        assert!(!serde_json::to_string(&key).unwrap().contains("handles"));
        for (h, _) in Handles::ALL {
            let k = Keyframe { handles: h, ..key.clone() };
            assert_eq!(serde_json::from_str::<Keyframe>(&serde_json::to_string(&k).unwrap()).unwrap(), k);
        }
    }

    /// The slope of the curve just either side of `t`.
    fn slopes(keys: &[Keyframe], t: f64) -> (f32, f32) {
        let h = 1e-4;
        ((evaluate(keys, t, 0.0) - evaluate(keys, t - h, 0.0)) / h as f32, (evaluate(keys, t + h, 0.0) - evaluate(keys, t, 0.0)) / h as f32)
    }

    fn three(handles: Handles) -> Vec<Keyframe> {
        let mut keys = vec![kf_linear(0.0, 0.0), kf_linear(1.0, 1.0), kf_linear(3.0, 1.05)];
        set_handles(&mut keys, 1, handles);
        keys
    }

    /// Auto handles run smooth through the key along the line between its
    /// neighbours, and follow a neighbour that moves without being set
    /// again; Flat ones are level there.
    #[test]
    fn auto_handles_are_smooth_and_follow_their_neighbours() {
        let mut keys = three(Handles::Auto);
        assert_eq!((keys[0].interpolation, keys[1].interpolation), (Interpolation::Bezier, Interpolation::Bezier));
        let (l, r) = slopes(&keys, 1.0);
        assert!((l - r).abs() < 0.01 && (l - 0.35).abs() < 0.01, "{l} {r}");
        keys[2].value = 3.0;
        let (l, r) = slopes(&keys, 1.0);
        assert!((l - r).abs() < 0.01 && (l - 1.0).abs() < 0.01, "moved neighbour: {l} {r}");

        let (l, r) = slopes(&three(Handles::Flat), 1.0);
        assert!(l.abs() < 0.01 && r.abs() < 0.01, "{l} {r}");
    }

    /// Plain Auto overshoots past a key that is nearly level with the next;
    /// Clamped stays within the neighbours' values, and goes flat at a peak.
    #[test]
    fn clamped_handles_never_overshoot() {
        let over = |keys: &[Keyframe]| (0..=300).map(|i| evaluate(keys, i as f64 / 100.0, 0.0)).fold(f32::MIN, f32::max);
        assert!(over(&three(Handles::Auto)) > 1.05 + 1e-3);
        assert!(over(&three(Handles::Clamped)) <= 1.05 + 1e-4, "{}", over(&three(Handles::Clamped)));
        let mut peak = vec![kf_linear(0.0, 0.0), kf_linear(1.0, 1.0), kf_linear(2.0, 0.0)];
        set_handles(&mut peak, 1, Handles::Clamped);
        let (l, r) = slopes(&peak, 1.0);
        assert!(l.abs() < 0.01 && r.abs() < 0.01 && over(&peak) <= 1.0 + 1e-4);
    }

    /// Linear In aims the in-handle at the key before and leaves the
    /// segment out alone; Aligned puts the handles in one line; Broken
    /// keeps them as they are. The stored tangents end up as evaluated.
    #[test]
    fn linear_aligned_and_broken_handles() {
        let keys = three(Handles::LinearIn);
        assert_eq!(keys[1].interpolation, Interpolation::Linear, "the segment out is not this kind's");
        assert!((evaluate(&keys, 0.5, 0.0) - 0.5).abs() < 1e-4, "a straight line in");
        assert_eq!(handles_at(&keys, 1), (keys[1].tangent_in, keys[1].tangent_out));

        let mut keys = vec![kf_bezier(0.0, 0.0, (0.3, 0.0), (0.0, 0.0)), kf_bezier(1.0, 1.0, (0.5, 0.5), (-0.2, 0.4)), kf_linear(2.0, 0.0)];
        let broken = keys.clone();
        set_handles(&mut keys, 1, Handles::Broken);
        assert_eq!(keys[1].tangent_in, broken[1].tangent_in);
        set_handles(&mut keys, 1, Handles::Aligned);
        let (ti, to) = (keys[1].tangent_in, keys[1].tangent_out);
        assert!(((ti.1 as f64) / ti.0 - (to.1 as f64) / to.0).abs() < 1e-6, "{ti:?} {to:?}");
        assert_eq!(keys[1].handles, Handles::Aligned);
    }

    /// A handle drag on a worked-out key names the kind it becomes, then
    /// the handle; the drag's handle is the one evaluated.
    #[test]
    fn a_handle_edit_sets_the_kind_then_the_handle() {
        let mut keys = three(Handles::Auto);
        edit_handles(&mut keys, 1, None, Some((0.4, 0.9)), Some(Handles::Broken));
        assert_eq!(keys[1].handles, Handles::Broken);
        assert_eq!(handles_at(&keys, 1).1, (0.4, 0.9));
    }

    /// Every named curve starts at 0 and ends at 1. Elastic swings past
    /// both ends, back overshoots, bounce never passes its end. A curve one
    /// cubic can draw evaluates as `ease_into`'s Bezier segment does, so a
    /// preset and an automatic op's ease give the same move.
    #[test]
    fn eased_curves_run_from_start_to_end() {
        let all = Ease::PRESETS.iter().flat_map(|(_, e)| *e).chain([Ease::Linear, Ease::In, Ease::Out, Ease::InOut]);
        for e in all {
            assert!(e.at(0.0).abs() < 1e-6 && (e.at(1.0) - 1.0).abs() < 1e-6, "{e:?}");
            let line = vec![kf_linear(0.0, 0.0), kf_linear(2.0, 10.0)];
            let mut eased = line.clone();
            ease_into(&mut eased, 1, e);
            for i in 1..20 {
                let t = i as f64 / 20.0;
                assert!((evaluate(&eased, 2.0 * t, 0.0) - 10.0 * e.at(t) as f32).abs() < 1e-3, "{e:?} at {t}");
            }
        }
        let range = |e: Ease| (0..=1000).map(|i| e.at(i as f64 / 1000.0)).fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        assert!(range(Ease::ElasticOut).1 > 1.2 && range(Ease::ElasticIn).0 < -0.2);
        assert!(range(Ease::BackOut).1 > 1.0 && range(Ease::BackIn).0 < 0.0);
        assert!(range(Ease::BounceOut).1 <= 1.0 + 1e-9 && range(Ease::BounceIn).0 >= -1e-9);
        assert!((Ease::BounceOut.at(1.0 / 2.75) - 1.0).abs() < 1e-9, "the first fall lands on the end");
    }

    /// An eased curve goes to the segment leaving the key (the one into
    /// it for the last key), alone, and that segment evaluates along it.
    #[test]
    fn an_eased_curve_shapes_one_segment() {
        let line = || vec![kf_linear(0.0, 0.0), kf_linear(1.0, 1.0), kf_linear(2.0, 0.0)];
        let elastic = Interpolation::Eased(Ease::ElasticOut);
        let mut keys = line();
        set_interpolation_at(&mut keys, 1, elastic);
        assert_eq!(keys.iter().map(|k| k.interpolation).collect::<Vec<_>>(), [Interpolation::Linear, elastic, Interpolation::Linear]);
        assert!((evaluate(&keys, 1.3, 0.0) - (1.0 - Ease::ElasticOut.at(0.3) as f32)).abs() < 1e-5);
        let mut keys = line();
        set_interpolation_at(&mut keys, 2, elastic);
        assert_eq!(keys[1].interpolation, elastic, "the last key's is the segment into it");
        let json = serde_json::to_string(&keys[1]).unwrap();
        assert_eq!(serde_json::from_str::<Keyframe>(&json).unwrap(), keys[1], "{json}");
    }

    fn kf_linear(time: f64, value: f32) -> Keyframe {
        kf(time, value, Interpolation::Linear)
    }

    /// `upserted` is the key `upsert_keyframe` leaves: a matching key's
    /// time and shape with the new value, else a new Linear key.
    #[test]
    fn upserted_is_the_key_upsert_leaves() {
        let shaped = Keyframe { tangent_out: (0.1, 0.2), tangent_in: (-0.3, 0.4), ..kf(0.5, 1.0, Interpolation::Bezier) };
        let keys = vec![kf_linear(0.0, 0.0), shaped.clone()];
        for (t, v) in [(0.5 + 1e-8, 3.0), (0.9, 2.0)] {
            let mut upserted_all = keys.clone();
            upsert_keyframe(&mut upserted_all, t, v);
            let left = upserted_all.iter().find(|k| (k.time_secs - t).abs() < KEYFRAME_TIME_EPSILON).unwrap();
            assert_eq!(&upserted(&keys, t, v), left);
        }
        assert_eq!(upserted(&keys, 0.5, 3.0), Keyframe { value: 3.0, ..shaped });
    }

    fn kf_step(time: f64, value: f32) -> Keyframe {
        kf(time, value, Interpolation::Step)
    }

    fn kf_bezier(time: f64, value: f32, tan_out: (f64, f32), tan_in: (f64, f32)) -> Keyframe {
        Keyframe {
            time_secs: time,
            value,
            interpolation: Interpolation::Bezier,
            tangent_out: tan_out,
            tangent_in: tan_in,
            handles: Default::default(),
        }
    }

    // ---- 1. Empty keyframes returns default ----

    #[test]
    fn empty_keyframes_returns_default() {
        assert_eq!(evaluate(&[], 0.0, 42.0), 42.0);
        assert_eq!(evaluate(&[], 1.0, -7.5), -7.5);
        assert_eq!(evaluate(&[], -1.0, 100.0), 100.0);
    }

    // ---- 2. Single keyframe returns its value at any time ----

    #[test]
    fn single_keyframe_at_exact_time() {
        let kfs = [kf_linear(1.0, 50.0)];
        assert_eq!(evaluate(&kfs, 1.0, 0.0), 50.0);
    }

    #[test]
    fn single_keyframe_before() {
        let kfs = [kf_linear(1.0, 50.0)];
        assert_eq!(evaluate(&kfs, 0.0, 0.0), 50.0);
    }

    #[test]
    fn single_keyframe_after() {
        let kfs = [kf_linear(1.0, 50.0)];
        assert_eq!(evaluate(&kfs, 5.0, 0.0), 50.0);
    }

    // ---- 3. Two keyframes, linear interpolation ----

    #[test]
    fn two_keyframes_linear_at_endpoints() {
        let kfs = [kf_linear(0.0, 0.0), kf_linear(1.0, 100.0)];
        assert_eq!(evaluate(&kfs, 0.0, -1.0), 0.0);
        assert_eq!(evaluate(&kfs, 1.0, -1.0), 100.0);
    }

    #[test]
    fn two_keyframes_linear_at_25_percent() {
        let kfs = [kf_linear(0.0, 0.0), kf_linear(1.0, 100.0)];
        let v = evaluate(&kfs, 0.25, -1.0);
        assert!((v - 25.0).abs() < 0.01, "expected ~25.0, got {v}");
    }

    #[test]
    fn two_keyframes_linear_at_50_percent() {
        let kfs = [kf_linear(0.0, 0.0), kf_linear(1.0, 100.0)];
        let v = evaluate(&kfs, 0.5, -1.0);
        assert!((v - 50.0).abs() < 0.01, "expected ~50.0, got {v}");
    }

    #[test]
    fn two_keyframes_linear_at_75_percent() {
        let kfs = [kf_linear(0.0, 0.0), kf_linear(1.0, 100.0)];
        let v = evaluate(&kfs, 0.75, -1.0);
        assert!((v - 75.0).abs() < 0.01, "expected ~75.0, got {v}");
    }

    #[test]
    fn two_keyframes_linear_non_zero_start() {
        let kfs = [kf_linear(2.0, 10.0), kf_linear(4.0, 30.0)];
        let v = evaluate(&kfs, 3.0, 0.0);
        assert!((v - 20.0).abs() < 0.01, "expected ~20.0, got {v}");
    }

    // ---- 4. Two keyframes, step interpolation ----

    #[test]
    fn two_keyframes_step_holds_previous() {
        let kfs = [kf_step(0.0, 10.0), kf_step(1.0, 90.0)];
        assert_eq!(evaluate(&kfs, 0.0, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 0.25, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 0.5, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 0.99, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 1.0, -1.0), 90.0);
    }

    #[test]
    fn step_then_linear_segments() {
        let kfs = [
            kf_step(0.0, 0.0),
            kf_linear(1.0, 100.0),
            kf_linear(2.0, 200.0),
        ];
        assert_eq!(evaluate(&kfs, 0.5, -1.0), 0.0);
        let v = evaluate(&kfs, 1.5, -1.0);
        assert!((v - 150.0).abs() < 0.01, "expected ~150.0, got {v}");
    }

    // ---- 5. Before first keyframe (hold backward) ----

    #[test]
    fn before_first_keyframe_holds_backward() {
        let kfs = [kf_linear(5.0, 42.0), kf_linear(10.0, 100.0)];
        assert_eq!(evaluate(&kfs, 0.0, -1.0), 42.0);
        assert_eq!(evaluate(&kfs, 4.99, -1.0), 42.0);
    }

    // ---- 6. After last keyframe (hold forward) ----

    #[test]
    fn after_last_keyframe_holds_forward() {
        let kfs = [kf_linear(0.0, 10.0), kf_linear(1.0, 20.0)];
        assert_eq!(evaluate(&kfs, 1.0, -1.0), 20.0);
        assert_eq!(evaluate(&kfs, 100.0, -1.0), 20.0);
    }

    // ---- 7. Three keyframes, correct per-segment interpolation ----

    #[test]
    fn three_keyframes_first_segment() {
        let kfs = [
            kf_linear(0.0, 0.0),
            kf_linear(1.0, 100.0),
            kf_linear(2.0, 50.0),
        ];
        let v = evaluate(&kfs, 0.5, -1.0);
        assert!((v - 50.0).abs() < 0.01, "first segment mid: expected ~50.0, got {v}");
    }

    #[test]
    fn three_keyframes_second_segment() {
        let kfs = [
            kf_linear(0.0, 0.0),
            kf_linear(1.0, 100.0),
            kf_linear(2.0, 50.0),
        ];
        let v = evaluate(&kfs, 1.5, -1.0);
        assert!((v - 75.0).abs() < 0.01, "second segment mid: expected ~75.0, got {v}");
    }

    #[test]
    fn three_keyframes_at_middle_keyframe() {
        let kfs = [
            kf_linear(0.0, 0.0),
            kf_linear(1.0, 100.0),
            kf_linear(2.0, 50.0),
        ];
        let v = evaluate(&kfs, 1.0, -1.0);
        assert!((v - 100.0).abs() < 0.01, "at middle kf: expected ~100.0, got {v}");
    }

    // ---- 8. Bezier interpolation ----

    #[test]
    fn bezier_no_tangents_hits_endpoints() {
        let kfs = [
            kf_bezier(0.0, 0.0, (0.0, 0.0), (0.0, 0.0)),
            kf_bezier(1.0, 100.0, (0.0, 0.0), (0.0, 0.0)),
        ];
        let v0 = evaluate(&kfs, 0.0, -1.0);
        let v1 = evaluate(&kfs, 1.0, -1.0);
        assert!((v0 - 0.0).abs() < 0.01, "start: expected ~0.0, got {v0}");
        assert!((v1 - 100.0).abs() < 0.01, "end: expected ~100.0, got {v1}");
    }

    #[test]
    fn bezier_with_time_tangents_smooth_curve() {
        // Ease-in-out curve with time handles
        let kfs = [
            kf_bezier(0.0, 0.0, (0.3, 0.0), (0.0, 0.0)),
            {
                let mut k = kf_bezier(1.0, 100.0, (0.0, 0.0), (-0.3, 0.0));
                k.interpolation = Interpolation::Linear; // irrelevant, first kf drives
                k
            },
        ];
        let v = evaluate(&kfs, 0.5, -1.0);
        assert!(v > 30.0 && v < 70.0, "bezier mid: expected 30..70, got {v}");
        assert!((evaluate(&kfs, 0.0, -1.0) - 0.0).abs() < 0.01);
        assert!((evaluate(&kfs, 1.0, -1.0) - 100.0).abs() < 0.01);
    }

    #[test]
    fn bezier_with_value_tangents() {
        // Zero time tangents, value tangents create overshoot shape
        // P0=0, P1=0+80=80, P2=100+(-80)=20, P3=100
        // At t=0.5: (1-t)^3*0 + 3*(1-t)^2*t*80 + 3*(1-t)*t^2*20 + t^3*100
        // = 0 + 30 + 7.5 + 12.5 = 50
        let kfs = [
            kf_bezier(0.0, 0.0, (0.0, 80.0), (0.0, 0.0)),
            kf_bezier(1.0, 100.0, (0.0, 0.0), (0.0, -80.0)),
        ];
        let v = evaluate(&kfs, 0.5, -1.0);
        assert!((v - 50.0).abs() < 1.0, "bezier value tangents mid: expected ~50, got {v}");
    }

    #[test]
    fn bezier_monotonic_with_ease() {
        let kfs = [
            kf_bezier(0.0, 0.0, (0.33, 0.0), (0.0, 0.0)),
            kf_bezier(1.0, 100.0, (0.0, 0.0), (-0.33, 0.0)),
        ];
        let mut prev = evaluate(&kfs, 0.0, -1.0);
        for i in 1..=20 {
            let t = i as f64 / 20.0;
            let v = evaluate(&kfs, t, -1.0);
            assert!(
                v >= prev - 0.1,
                "bezier should be monotonic: at t={t}, v={v} < prev={prev}"
            );
            prev = v;
        }
    }

    // ---- 9. Negative time ----

    #[test]
    fn negative_time_returns_first_keyframe() {
        let kfs = [kf_linear(0.0, 42.0), kf_linear(1.0, 100.0)];
        assert_eq!(evaluate(&kfs, -1.0, -1.0), 42.0);
        assert_eq!(evaluate(&kfs, -1000.0, -1.0), 42.0);
    }

    #[test]
    fn negative_time_empty_returns_default() {
        assert_eq!(evaluate(&[], -5.0, 99.0), 99.0);
    }

    // ---- 10. Zero-duration segment ----

    #[test]
    fn zero_duration_segment_returns_first_value() {
        let kfs = [kf_linear(1.0, 10.0), kf_linear(1.0, 90.0)];
        let v = evaluate(&kfs, 1.0, -1.0);
        assert_eq!(v, 10.0);
    }

    #[test]
    fn zero_duration_segment_before_and_after() {
        let kfs = [kf_linear(1.0, 10.0), kf_linear(1.0, 90.0)];
        assert_eq!(evaluate(&kfs, 0.5, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 1.5, -1.0), 90.0);
    }

    #[test]
    fn zero_duration_step() {
        let kfs = [kf_step(1.0, 10.0), kf_step(1.0, 90.0)];
        let v = evaluate(&kfs, 1.0, -1.0);
        assert_eq!(v, 10.0);
    }

    // ---- Edge cases ----

    #[test]
    fn very_large_time_values() {
        let kfs = [kf_linear(0.0, 1.0), kf_linear(1.0, 2.0)];
        assert_eq!(evaluate(&kfs, 1e12, -1.0), 2.0);
    }

    #[test]
    fn many_keyframes_correct_segment_lookup() {
        let kfs: Vec<Keyframe> = (0..10)
            .map(|i| kf_linear(i as f64, i as f32 * 10.0))
            .collect();
        let v = evaluate(&kfs, 4.5, -1.0);
        assert!((v - 45.0).abs() < 0.01, "expected ~45.0, got {v}");
    }

    #[test]
    fn default_interpolation_is_linear() {
        assert_eq!(Interpolation::default(), Interpolation::Linear);
    }

    // ═══════════════════════════════════════════════════════════════
    // NaN-safe sort regression.
    //
    // ~20+ sites in klipp-ui/src/app.rs sort keyframes, multicam_cuts,
    // and PCM regions with the closure
    //
    //   |a, b| a.time_secs.partial_cmp(&b.time_secs)
    //              .unwrap_or(std::cmp::Ordering::Equal)
    //
    // because OFX plugins, hand-edited .klipp project files, or a bad
    // preset can inject NaN time values; the bare `.unwrap()` form
    // would panic. These tests exercise that closure directly so any
    // future "cleanup" that drops `.unwrap_or(Equal)` back to bare
    // `.unwrap()` surfaces here instead of in a user crash.
    //
    // KNOWN LIMITATION (latent bug, not in U-13's scope): treating NaN
    // as Equal violates sort_by's strict-weak-ordering contract, so
    // when a NaN slips into the input the order of OTHER elements
    // around it is also unspecified. The contract these tests pin is
    // narrow: "no panic", not "sort is sane in the face of NaN".
    // Properly stripping or quarantining NaN-time keyframes is a
    // follow-up worth opening separately if one ever surfaces.
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn keyframe_sort_with_nan_time_does_not_panic() {
        let mut kfs = [kf_linear(2.0, 0.0),
            kf_linear(f64::NAN, 0.0),
            kf_linear(0.5, 0.0),
            kf_linear(1.0, 0.0)];
        // Same closure as the sort sites in klipp-ui/src/app.rs. The
        // assertion is implicit: control reaches the line below.
        kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(kfs.len(), 4, "sort must preserve length");
    }

    #[test]
    fn tuple_sort_by_first_f64_with_nan_does_not_panic() {
        // Mirrors clip.multicam_cuts (Vec<(f64, u8)>) and the PCM regions
        // sort in app.rs — same NaN-safe closure on the first tuple field.
        let mut cuts: Vec<(f64, u8)> = vec![
            (2.0, 1),
            (f64::NAN, 2),
            (0.5, 3),
            (1.0, 4),
        ];
        cuts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(cuts.len(), 4);
    }

    #[test]
    fn keyframe_sort_without_nan_is_ascending() {
        // Sanity: when no NaN is present the closure DOES sort
        // ascending, so the rest of the codebase's reliance on the
        // sort's correctness in the normal case is preserved.
        let mut kfs = [kf_linear(3.0, 0.0),
            kf_linear(1.0, 0.0),
            kf_linear(2.5, 0.0),
            kf_linear(0.5, 0.0)];
        kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
        let times: Vec<f64> = kfs.iter().map(|k| k.time_secs).collect();
        assert_eq!(times, vec![0.5, 1.0, 2.5, 3.0]);
    }
}



#[cfg(test)]
mod ease_tests {
    use super::*;

    fn track(ease: Ease) -> Vec<Keyframe> {
        let mut k = Vec::new();
        upsert_keyframe(&mut k, 0.0, 0.0);
        upsert_keyframe(&mut k, 2.0, 100.0);
        ease_into(&mut k, 1, ease);
        k
    }

    #[test]
    fn each_ease_has_the_shape_its_name_says() {
        let mid = |e| evaluate(&track(e), 1.0, 0.0);
        assert!((mid(Ease::Linear) - 50.0).abs() < 1e-3);
        assert!(mid(Ease::In) < 40.0, "in is still slow halfway: {}", mid(Ease::In));
        assert!(mid(Ease::Out) > 60.0, "out is nearly there halfway: {}", mid(Ease::Out));
        assert!((mid(Ease::InOut) - 50.0).abs() < 1.0, "in_out is symmetric: {}", mid(Ease::InOut));
        assert!(evaluate(&track(Ease::ExpoOut), 0.4, 0.0) > 70.0, "expo_out arrives fast");
        let peak = (1..40).map(|i| evaluate(&track(Ease::BackOut), i as f64 * 0.05, 0.0)).fold(0.0f32, f32::max);
        assert!(peak > 100.5, "back_out goes past and settles: {peak}");
        // Every ease still starts and ends where the keyframes are.
        for e in [Ease::In, Ease::Out, Ease::InOut, Ease::ExpoOut, Ease::BackOut] {
            assert!((evaluate(&track(e), 0.0, 0.0)).abs() < 1e-3);
            assert!((evaluate(&track(e), 2.0, 0.0) - 100.0).abs() < 1e-3);
        }
    }

    #[test]
    fn an_ease_reads_from_its_name() {
        let e: Ease = serde_json::from_str("\"expo_out\"").unwrap();
        assert_eq!(e, Ease::ExpoOut);
    }
}

#[cfg(test)]
mod precision_tests {
    use super::*;

    #[test]
    fn adjacent_frames_survive_mutation_and_round_trip() {
        for fps in [24.0, 30.0, 60.0, 120.0, 24000.0 / 1001.0, 60000.0 / 1001.0] {
            let mut keys = Vec::new();
            for frame in 0..4 {
                upsert_keyframe(&mut keys, frame as f64 / fps, frame as f32);
            }
            assert_eq!(keys.len(), 4, "insertion at {fps} fps");
            ease_into(&mut keys, 1, Ease::Out);
            let encoded = serde_json::to_string(&keys).unwrap();
            let mut decoded: Vec<Keyframe> = serde_json::from_str(&encoded).unwrap();
            assert_eq!(keys.len(), decoded.len());
            for (before, after) in keys.iter().zip(&decoded) {
                assert!((before.time_secs - after.time_secs).abs() < KEYFRAME_TIME_EPSILON);
                assert_eq!(before.value, after.value);
                assert_eq!(before.interpolation, after.interpolation);
                assert!((before.tangent_in.0 - after.tangent_in.0).abs() < KEYFRAME_TIME_EPSILON);
                assert_eq!(before.tangent_in.1, after.tangent_in.1);
            }
            for frame in 0..4 {
                assert_eq!(evaluate(&decoded, frame as f64 / fps, -1.0), frame as f32);
            }
            assert!(move_keyframe(&mut decoded, 3, 4.0 / fps, 4.0));
            assert_eq!(decoded.len(), 4);
            remove_keyframe_at(&mut decoded, 1.0 / fps);
            assert_eq!(decoded.len(), 3, "deletion at {fps} fps");
            assert_eq!(decoded[0].time_secs, 0.0);
            assert!((decoded[1].time_secs - 2.0 / fps).abs() < KEYFRAME_TIME_EPSILON);
            upsert_keyframe(&mut decoded, 2.0 / fps, 20.0);
            assert_eq!(decoded.len(), 3, "replacement at {fps} fps");
            assert_eq!(evaluate(&decoded, 2.0 / fps, -1.0), 20.0);
        }
    }

    #[test]
    fn floating_point_roundoff_still_identifies_the_same_time() {
        let mut keys = Vec::new();
        upsert_keyframe(&mut keys, 0.1 + 0.2, 1.0);
        upsert_keyframe(&mut keys, 0.3, 2.0);
        assert_eq!(keys.len(), 1);
        remove_keyframe_at(&mut keys, 0.1 + 0.2);
        assert!(keys.is_empty());
    }
    #[test]
    fn value_updates_preserve_shape_and_new_keys_use_the_requested_default() {
        let mut keys = Vec::new();
        upsert_keyframe_with_interpolation(&mut keys, 0.7, 0.5, Interpolation::Bezier);
        keys[0].tangent_in = (-0.1, -0.2);
        keys[0].tangent_out = (0.3, 0.4);
        let mut expected = keys[0].clone();
        expected.value = 0.9;
        upsert_keyframe(&mut keys, 0.7 + KEYFRAME_TIME_EPSILON / 2.0, 0.9);
        assert_eq!(keys, vec![expected]);
        upsert_keyframe_with_interpolation(&mut keys, 1.0, 1.0, Interpolation::Step);
        assert_eq!(keys[1].interpolation, Interpolation::Step);
        upsert_keyframe(&mut keys, 2.0, 0.5);
        assert_eq!(keys[2].interpolation, Interpolation::Linear);
    }

}
