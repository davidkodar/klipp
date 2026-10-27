//! A title's published fields (#1139): what a motion-graphics template
//! keeps editable once applied. A template is a styled title saved with a
//! few fields named on it (a layer's text, a colour, how fast it moves),
//! and the inspector shows those first, the rest folded away, so a title
//! applied in one drag is changed with a box and a slider.
//!
//! Each field names its target by the layer's id, which a template keeps.
//! A field whose layer has gone (deleted since) or no longer holds what
//! the field reads is not live: shown nowhere, set refused, never a panic.

use crate::{TitleBackground, TitleData, TitleObjectContent, TitleObjectId, TitlePosition};
use klipp_core::TitleFieldValue;
use serde::{Deserialize, Serialize};

/// How fast a title's motion may run against its template's.
pub const SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.25..=4.0;

/// How big a title may be against its template's (#1724).
pub const SIZE_RANGE: std::ops::RangeInclusive<f32> = 0.5..=2.0;

/// One field a template keeps editable, and what the inspector calls it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedField {
    pub label: String,
    pub kind: FieldKind,
}

/// What a published field reads and writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldKind {
    /// A text layer's text.
    Text { object: TitleObjectId },
    /// A layer's colour: a text's, or a shape's fill.
    Colour { object: TitleObjectId },
    /// The colour of the title's background bar.
    BackgroundColour,
    /// Every motion's timing, as the template made it divided by
    /// `factor`: the timings kept here are the template's, so the factor
    /// never compounds and 1.0 is always as made.
    Speed { factor: f32, base: Vec<LayerTiming> },
    /// The whole title's size, as the template made it times `factor`
    /// (#1724): each layer's place grown from `anchor` (a point of the
    /// frame, 0..1), its box and its type grown with it. As with Speed,
    /// the template's own places and sizes are kept here, so a factor
    /// never compounds and 1.0 is always as made.
    Size { factor: f32, anchor: TitlePosition, base: Vec<LayerSize> },
    /// A text layer's font family (#2757).
    Font { object: TitleObjectId },
    /// An image layer's file (#2757).
    Picture { object: TitleObjectId },
    /// How long the motion is laid out over, in seconds (#2757): the
    /// template's timings and animator keys, made over `made_secs`,
    /// stretched to `secs`. As with Speed, the template's own are kept
    /// here, so a change never compounds.
    Duration { made_secs: f32, secs: f32, base: Vec<LayerTiming>, animators: Vec<LayerAnimators> },
}

/// A layer's animators as the template made them (#2757).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerAnimators {
    pub object: TitleObjectId,
    pub animators: Vec<klipp_core::title_motion::Animator>,
}

/// The shortest and longest a Duration field lays motion over, seconds.
pub const DURATION_RANGE: std::ops::RangeInclusive<f32> = 0.2..=3600.0;

/// A layer's place, box and type size, as the template made them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LayerSize {
    pub object: TitleObjectId,
    pub center: TitlePosition,
    pub size: TitlePosition,
    pub font_size: Option<f32>,
}

/// A layer's entrance and exit timing, as the template made them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LayerTiming {
    pub object: TitleObjectId,
    pub enter: Option<Timing>,
    pub exit: Option<Timing>,
}

/// One motion's timing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Timing {
    pub duration_secs: f32,
    pub delay_secs: f32,
    pub stagger_secs: f32,
}

impl Timing {
    fn of(m: &klipp_core::title_motion::Motion) -> Self {
        Self { duration_secs: m.duration_secs, delay_secs: m.delay_secs, stagger_secs: m.stagger_secs }
    }

    fn put(&self, m: &mut klipp_core::title_motion::Motion, factor: f32) {
        m.duration_secs = (self.duration_secs / factor).min(klipp_core::title_motion::MAX_DURATION_SECS);
        m.delay_secs = self.delay_secs / factor;
        m.stagger_secs = self.stagger_secs / factor;
    }
}

impl TitleData {
    /// The fields a template keeps editable unless told otherwise: each
    /// text layer's text and colour, the background bar's colour when it
    /// has one, and the speed of its motion when anything moves.
    pub fn default_fields(&self) -> Vec<PublishedField> {
        let mut out = Vec::new();
        let mut n = 0;
        for o in &self.objects {
            if let TitleObjectContent::TextBox(_) = &o.content {
                n += 1;
                let name = if o.name.trim().is_empty() { format!("Text {n}") } else { o.name.trim().to_string() };
                out.push(PublishedField { label: name.clone(), kind: FieldKind::Text { object: o.id } });
                out.push(PublishedField { label: format!("{name} colour"), kind: FieldKind::Colour { object: o.id } });
            }
        }
        if !matches!(self.background, TitleBackground::None) {
            out.push(PublishedField { label: "Bar colour".into(), kind: FieldKind::BackgroundColour });
        }
        let base = self.timings();
        if !base.is_empty() {
            out.push(PublishedField { label: "Speed".into(), kind: FieldKind::Speed { factor: 1.0, base } });
        }
        out
    }

    /// A Size field over every layer as it is now, grown from `anchor`
    /// (#1724): what a template publishes to be made bigger or smaller
    /// with one slider.
    pub fn size_field(&self, anchor: TitlePosition) -> FieldKind {
        let base = self
            .objects
            .iter()
            .map(|o| LayerSize {
                object: o.id,
                center: o.transform.center,
                size: o.transform.size,
                font_size: match &o.content {
                    TitleObjectContent::TextBox(tb) => Some(tb.font_size),
                    _ => None,
                },
            })
            .collect();
        FieldKind::Size { factor: 1.0, anchor, base }
    }

    /// A Duration field for a title made `made_secs` long (#2757): its
    /// motion and animators as they are now; `None` when nothing in it
    /// moves over time.
    pub fn duration_field(&self, made_secs: f32) -> Option<FieldKind> {
        let base = self.timings();
        let animators: Vec<LayerAnimators> = self
            .objects
            .iter()
            .filter(|o| !o.fx().animators.is_empty())
            .map(|o| LayerAnimators { object: o.id, animators: o.fx().animators.clone() })
            .collect();
        let made = made_secs.clamp(*DURATION_RANGE.start(), *DURATION_RANGE.end());
        (!base.is_empty() || !animators.is_empty()).then_some(FieldKind::Duration { made_secs: made, secs: made, base, animators })
    }

    /// The fields a template may also keep editable, offered unticked
    /// beside the defaults (#2757): each text's font and each picture,
    /// and the motion's length for a title made `made_secs` long.
    pub fn optional_fields(&self, made_secs: f32) -> Vec<PublishedField> {
        let mut out = Vec::new();
        let (mut texts, mut pictures) = (0, 0);
        for o in &self.objects {
            match &o.content {
                TitleObjectContent::TextBox(_) => {
                    texts += 1;
                    let name = if o.name.trim().is_empty() { format!("Text {texts}") } else { o.name.trim().to_string() };
                    out.push(PublishedField { label: format!("{name} font"), kind: FieldKind::Font { object: o.id } });
                }
                TitleObjectContent::Image(_) => {
                    pictures += 1;
                    let name = if o.name.trim().is_empty() { format!("Picture {pictures}") } else { o.name.trim().to_string() };
                    out.push(PublishedField { label: name, kind: FieldKind::Picture { object: o.id } });
                }
                _ => {}
            }
        }
        if let Some(kind) = self.duration_field(made_secs) {
            out.push(PublishedField { label: "Duration".into(), kind });
        }
        out
    }

    /// Each layer's motion timing as it is now.
    fn timings(&self) -> Vec<LayerTiming> {
        self.objects
            .iter()
            .filter_map(|o| {
                let m = &o.fx().motion;
                let (enter, exit) = (m.enter.as_ref().map(Timing::of), m.exit.as_ref().map(Timing::of));
                (enter.is_some() || exit.is_some()).then_some(LayerTiming { object: o.id, enter, exit })
            })
            .collect()
    }

    /// The value of published field `index`, while it is live.
    pub fn field_value(&self, index: usize) -> Option<TitleFieldValue> {
        let field = self.published.get(index)?;
        let object = |id: TitleObjectId| self.objects.iter().find(|o| o.id == id);
        match &field.kind {
            FieldKind::Text { object: id } => match &object(*id)?.content {
                TitleObjectContent::TextBox(tb) => Some(TitleFieldValue::Text(tb.text.clone())),
                _ => None,
            },
            FieldKind::Colour { object: id } => match &object(*id)?.content {
                TitleObjectContent::TextBox(tb) => Some(TitleFieldValue::Colour(tb.color)),
                TitleObjectContent::Shape(s) => s.fill.map(TitleFieldValue::Colour),
                TitleObjectContent::Image(_) => None,
            },
            FieldKind::BackgroundColour => match &self.background {
                TitleBackground::Color { color, .. } | TitleBackground::FullFrame { color } => Some(TitleFieldValue::Colour(*color)),
                TitleBackground::None => None,
            },
            FieldKind::Speed { factor, base } => base.iter().any(|b| object(b.object).is_some()).then_some(TitleFieldValue::Speed(*factor)),
            FieldKind::Size { factor, base, .. } => base.iter().any(|b| object(b.object).is_some()).then_some(TitleFieldValue::Size(*factor)),
            FieldKind::Font { object: id } => match &object(*id)?.content {
                TitleObjectContent::TextBox(tb) => Some(TitleFieldValue::Font(tb.font_name.clone())),
                _ => None,
            },
            FieldKind::Picture { object: id } => match &object(*id)?.content {
                TitleObjectContent::Image(img) => Some(TitleFieldValue::Picture(img.path.clone())),
                _ => None,
            },
            FieldKind::Duration { secs, base, animators, .. } => {
                (base.iter().any(|b| object(b.object).is_some()) || animators.iter().any(|a| object(a.object).is_some())).then_some(TitleFieldValue::Duration(*secs))
            }
        }
    }

    /// The published fields still live, with their place in the list: the
    /// ones the inspector shows.
    pub fn live_fields(&self) -> Vec<(usize, &PublishedField, TitleFieldValue)> {
        self.published.iter().enumerate().filter_map(|(i, f)| self.field_value(i).map(|v| (i, f, v))).collect()
    }

    /// The live published field called `label`, in any case.
    pub fn field_named(&self, label: &str) -> Option<usize> {
        let label = label.trim();
        self.live_fields().into_iter().find(|(_, f, _)| f.label.eq_ignore_ascii_case(label)).map(|(i, ..)| i)
    }

    /// After `object`'s motion was set by hand (not through Speed), keep
    /// each Speed field's base the timing that, at its factor, is the one
    /// set: a later Speed move starts from the edit, not from the template.
    pub fn rebase_speed(&mut self, object: TitleObjectId) {
        let Some(motion) = self.objects.iter().find(|o| o.id == object).map(|o| o.fx().motion) else { return };
        let timing = |m: Option<&klipp_core::title_motion::Motion>, factor: f32| {
            m.map(|m| Timing { duration_secs: m.duration_secs * factor, delay_secs: m.delay_secs * factor, stagger_secs: m.stagger_secs * factor })
        };
        for field in &mut self.published {
            if let FieldKind::Speed { factor, base } = &mut field.kind {
                let entry = LayerTiming { object, enter: timing(motion.enter.as_ref(), *factor), exit: timing(motion.exit.as_ref(), *factor) };
                base.retain(|b| b.object != object);
                if entry.enter.is_some() || entry.exit.is_some() {
                    base.push(entry);
                }
            }
        }
    }

    /// The motion timed by the Speed and Duration fields together
    /// (#2757): the template's timings, divided by the speed and
    /// stretched from the length they were made over to the Duration's;
    /// its animator keys the same.
    fn retime(&mut self) {
        let mut speed = 1.0;
        let mut stretch = 1.0;
        let mut base: Option<Vec<LayerTiming>> = None;
        let mut animators: Vec<LayerAnimators> = Vec::new();
        for f in &self.published {
            match &f.kind {
                FieldKind::Speed { factor, base: b } => {
                    speed = *factor;
                    base = Some(b.clone());
                }
                FieldKind::Duration { made_secs, secs, base: b, animators: a } => {
                    stretch = secs / made_secs.max(1e-3);
                    base.get_or_insert_with(|| b.clone());
                    animators = a.clone();
                }
                _ => {}
            }
        }
        let factor = speed / stretch;
        for b in base.iter().flatten() {
            if let Some(o) = self.objects.iter_mut().find(|o| o.id == b.object) {
                let m = &mut o.fx_mut().motion;
                if let (Some(t), Some(enter)) = (b.enter, m.enter.as_mut()) {
                    t.put(enter, factor);
                }
                if let (Some(t), Some(exit)) = (b.exit, m.exit.as_mut()) {
                    t.put(exit, factor);
                }
            }
        }
        for a in &animators {
            if let Some(o) = self.objects.iter_mut().find(|o| o.id == a.object) {
                let mut timed = a.animators.clone();
                for an in &mut timed {
                    for selector in std::iter::once(&mut an.selector).chain(an.within.as_mut()) {
                        for k in selector.start_keyframes.iter_mut().chain(selector.end_keyframes.iter_mut()).chain(selector.offset_keyframes.iter_mut()) {
                            k.time_secs /= factor as f64;
                        }
                    }
                }
                o.fx_mut().animators = timed;
            }
        }
    }

    /// Set published field `index` to `value`. Refused, with nothing
    /// changed, for a field that isn't live or a value of another kind.
    pub fn set_field(&mut self, index: usize, value: &TitleFieldValue) -> Result<(), String> {
        if self.field_value(index).is_none() {
            return Err("that field's layer is gone".into());
        }
        let kind = self.published[index].kind.clone();
        match (kind, value) {
            (FieldKind::Text { object }, TitleFieldValue::Text(text)) => {
                if let Some(TitleObjectContent::TextBox(tb)) = self.objects.iter_mut().find(|o| o.id == object).map(|o| &mut o.content) {
                    tb.text = text.clone();
                    crate::adjust_style_runs_for_text_change(&mut tb.style_runs, tb.text.len());
                }
            }
            (FieldKind::Colour { object }, TitleFieldValue::Colour(c)) => match self.objects.iter_mut().find(|o| o.id == object).map(|o| &mut o.content) {
                Some(TitleObjectContent::TextBox(tb)) => tb.color = *c,
                Some(TitleObjectContent::Shape(s)) => s.fill = Some(*c),
                _ => {}
            },
            (FieldKind::BackgroundColour, TitleFieldValue::Colour(c)) => match &mut self.background {
                TitleBackground::Color { color, .. } | TitleBackground::FullFrame { color } => *color = *c,
                TitleBackground::None => {}
            },
            (FieldKind::Speed { .. }, TitleFieldValue::Speed(f)) => {
                if let FieldKind::Speed { factor, .. } = &mut self.published[index].kind {
                    *factor = f.clamp(*SPEED_RANGE.start(), *SPEED_RANGE.end());
                }
                self.retime();
            }
            (FieldKind::Size { anchor, base, .. }, TitleFieldValue::Size(f)) => {
                let factor = f.clamp(*SIZE_RANGE.start(), *SIZE_RANGE.end());
                for b in &base {
                    if let Some(o) = self.objects.iter_mut().find(|o| o.id == b.object) {
                        o.transform.center = TitlePosition {
                            x: anchor.x + (b.center.x - anchor.x) * factor,
                            y: anchor.y + (b.center.y - anchor.y) * factor,
                        };
                        o.transform.size = TitlePosition { x: b.size.x * factor, y: b.size.y * factor };
                        if let (Some(font), TitleObjectContent::TextBox(tb)) = (b.font_size, &mut o.content) {
                            tb.font_size = font * factor;
                        }
                    }
                }
                if let FieldKind::Size { factor: f, .. } = &mut self.published[index].kind {
                    *f = factor;
                }
            }
            (FieldKind::Font { object }, TitleFieldValue::Font(family)) => {
                let family = family.trim();
                if family.is_empty() {
                    return Err("a font needs a family's name".into());
                }
                if let Some(TitleObjectContent::TextBox(tb)) = self.objects.iter_mut().find(|o| o.id == object).map(|o| &mut o.content) {
                    tb.font_name = family.to_string();
                }
            }
            (FieldKind::Picture { object }, TitleFieldValue::Picture(path)) => {
                if path.trim().is_empty() {
                    return Err("a picture needs a file".into());
                }
                if let Some(TitleObjectContent::Image(img)) = self.objects.iter_mut().find(|o| o.id == object).map(|o| &mut o.content) {
                    img.path = path.trim().to_string();
                }
            }
            (FieldKind::Duration { .. }, TitleFieldValue::Duration(d)) => {
                if let FieldKind::Duration { secs, .. } = &mut self.published[index].kind {
                    *secs = d.clamp(*DURATION_RANGE.start(), *DURATION_RANGE.end());
                }
                self.retime();
            }
            _ => return Err("that value isn't of the field's kind".into()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TextBox;
    use klipp_core::title_motion::{Motion, MotionKind, StaggerUnit};

    /// A lower third: a name and a role, a bar behind them, the name rising
    /// in by word and fading out.
    fn lower_third() -> (TitleData, TitleObjectId, TitleObjectId) {
        let mut td = TitleData::single_text(TextBox { text: "Jane Doe".into(), ..Default::default() });
        let name = td.objects[0].id;
        let at = td.objects[0].transform.clone();
        let role = td.add_object(TitleObjectContent::TextBox(TextBox { text: "Director".into(), ..Default::default() }), at);
        td.background = TitleBackground::Color { color: [20, 20, 20, 200], padding: 0.2 };
        let m = &mut td.objects[0].fx_mut().motion;
        m.enter = Some(Motion::new(MotionKind::Rise).duration(0.6).delay(0.1).stagger(StaggerUnit::Word, 0.08));
        m.exit = Some(Motion::new(MotionKind::Fade).duration(0.4).as_exit());
        (td, name, role)
    }

    fn kinds(fields: &[PublishedField]) -> Vec<String> {
        fields.iter().map(|f| f.label.clone()).collect()
    }

    /// A template keeps each text's text and colour, the bar's colour and
    /// its motion's speed editable unless told otherwise.
    #[test]
    fn the_default_fields_are_each_text_and_colour_the_bar_and_the_speed() {
        let (td, ..) = lower_third();
        assert_eq!(kinds(&td.default_fields()), ["Text 1", "Text 1 colour", "Text 2", "Text 2 colour", "Bar colour", "Speed"]);
        let still = TitleData::single_text(TextBox { text: "Hi".into(), ..Default::default() });
        assert_eq!(kinds(&still.default_fields()), ["Text 1", "Text 1 colour"], "nothing moves, so no speed");
    }

    /// Each field writes its own target and nothing else; a value of
    /// another kind is refused.
    #[test]
    fn a_field_writes_its_own_target() {
        let (mut td, name, role) = lower_third();
        td.published = td.default_fields();
        let before = td.clone();
        td.set_field(2, &TitleFieldValue::Text("Producer".into())).unwrap();
        td.set_field(1, &TitleFieldValue::Colour([255, 200, 0, 255])).unwrap();
        td.set_field(4, &TitleFieldValue::Colour([0, 0, 120, 255])).unwrap();
        let text = |td: &TitleData, id| match &td.objects.iter().find(|o| o.id == id).unwrap().content {
            TitleObjectContent::TextBox(tb) => (tb.text.clone(), tb.color),
            _ => unreachable!(),
        };
        assert_eq!(text(&td, role).0, "Producer");
        assert_eq!(text(&td, name), ("Jane Doe".to_string(), [255, 200, 0, 255]));
        assert_eq!(td.background, TitleBackground::Color { color: [0, 0, 120, 255], padding: 0.2 });
        assert_eq!(td.objects[0].fx(), before.objects[0].fx(), "the motion untouched");
        assert!(td.set_field(0, &TitleFieldValue::Speed(2.0)).is_err(), "a text takes no speed");
    }

    /// Duration (#2757) lays the motion and the animator keys over the
    /// length asked: twice as long, every timing and key twice as late;
    /// with Speed too, both apply, and both back at as made is the
    /// template exactly.
    #[test]
    fn duration_stretches_the_motion_and_combines_with_speed() {
        let (mut td, ..) = lower_third();
        td.objects[0].fx_mut().animators.push(klipp_core::title_motion::Animator {
            selector: klipp_core::title_motion::RangeSelector { end_keyframes: vec![klipp_core::Keyframe { time_secs: 1.0, value: 1.0, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }], ..Default::default() },
            ..Default::default()
        });
        let made = td.clone();
        td.published = td.default_fields();
        td.published.extend(td.optional_fields(4.0).into_iter().filter(|f| f.label == "Duration"));
        let (speed, duration) = (td.field_named("Speed").unwrap(), td.field_named("Duration").unwrap());
        assert_eq!(td.field_value(duration), Some(TitleFieldValue::Duration(4.0)));
        td.set_field(duration, &TitleFieldValue::Duration(8.0)).unwrap();
        let enter = td.objects[0].fx().motion.enter.unwrap();
        assert!((enter.duration_secs - 1.2).abs() < 1e-6 && (enter.delay_secs - 0.2).abs() < 1e-6, "{enter:?}");
        assert!((td.objects[0].fx().animators[0].selector.end_keyframes[0].time_secs - 2.0).abs() < 1e-9, "the key twice as late");
        td.set_field(speed, &TitleFieldValue::Speed(2.0)).unwrap();
        let enter = td.objects[0].fx().motion.enter.unwrap();
        assert!((enter.duration_secs - 0.6).abs() < 1e-6, "twice as long and twice as fast: as made, {enter:?}");
        td.set_field(speed, &TitleFieldValue::Speed(1.0)).unwrap();
        td.set_field(duration, &TitleFieldValue::Duration(4.0)).unwrap();
        assert_eq!(td.objects[0].fx(), made.objects[0].fx(), "both as made: the template exactly");
    }

    /// Font and Picture (#2757) write their own layer.
    #[test]
    fn font_and_picture_fields_write_their_layer() {
        let (mut td, name, _) = lower_third();
        let at = td.objects[0].transform.clone();
        let pic = td.add_object(TitleObjectContent::Image(crate::ImageLayer { path: "a.png".into(), fit: Default::default() }), at);
        td.published = td.optional_fields(4.0);
        assert_eq!(kinds(&td.published), ["Text 1 font", "Text 2 font", "a", "Duration"], "a picture by its layer's name");
        td.set_field(0, &TitleFieldValue::Font("Anton".into())).unwrap();
        td.set_field(2, &TitleFieldValue::Picture("b.png".into())).unwrap();
        let o = |id| td.objects.iter().find(|o| o.id == id).unwrap().content.clone();
        assert!(matches!(o(name), TitleObjectContent::TextBox(tb) if tb.font_name == "Anton"));
        assert!(matches!(o(pic), TitleObjectContent::Image(img) if img.path == "b.png"));
        assert!(td.set_field(0, &TitleFieldValue::Font("  ".into())).is_err(), "a font needs a name");
    }

    /// Speed is the template's timing divided by the factor, never the
    /// last timing's: any run of moves back to 1.0 is the template exactly.
    #[test]
    fn speed_never_compounds_and_one_is_as_made() {
        let (mut td, ..) = lower_third();
        td.published = td.default_fields();
        let speed = td.published.iter().position(|f| matches!(f.kind, FieldKind::Speed { .. })).unwrap();
        let made = td.objects[0].fx().motion;
        for f in [2.0, 0.5, 3.7, 0.25, 4.0] {
            td.set_field(speed, &TitleFieldValue::Speed(f)).unwrap();
        }
        let enter = td.objects[0].fx().motion.enter.unwrap();
        assert!((enter.duration_secs - 0.6 / 4.0).abs() < 1e-6 && (enter.stagger_secs - 0.08 / 4.0).abs() < 1e-6, "{enter:?}");
        assert_eq!(td.field_value(speed), Some(TitleFieldValue::Speed(4.0)));
        td.set_field(speed, &TitleFieldValue::Speed(1.0)).unwrap();
        assert_eq!(td.objects[0].fx().motion, made, "1.0 is the template's own timing");
        td.set_field(speed, &TitleFieldValue::Speed(99.0)).unwrap();
        assert_eq!(td.field_value(speed), Some(TitleFieldValue::Speed(4.0)), "held to its range");
    }

    /// A layer's timing set by hand is where a later speed move starts: at
    /// 2× a 0.8 s entrance set by hand is the base 1.6 s, and back at 1.0
    /// the entrance is 1.6 s, not the template's 0.6 s.
    #[test]
    fn a_timing_set_by_hand_is_where_speed_starts() {
        let (mut td, name, _) = lower_third();
        td.published = td.default_fields();
        let speed = td.field_named("speed").expect("found in any case");
        td.set_field(speed, &TitleFieldValue::Speed(2.0)).unwrap();
        td.objects[0].fx_mut().motion.enter.as_mut().unwrap().duration_secs = 0.8;
        td.rebase_speed(name);
        td.set_field(speed, &TitleFieldValue::Speed(1.0)).unwrap();
        assert!((td.objects[0].fx().motion.enter.unwrap().duration_secs - 1.6).abs() < 1e-6);
        assert_eq!(td.field_named("No such"), None);
    }

    /// A field whose layer was deleted is not live: not listed, set
    /// refused with nothing changed. A layer duplicated beside it leaves the
    /// field on its own layer.
    #[test]
    fn a_field_whose_layer_is_gone_is_hidden_not_a_panic() {
        let (mut td, name, role) = lower_third();
        td.published = td.default_fields();
        // Duplicated: the copy has a new id, the field keeps its layer.
        let copy = td.objects.iter().find(|o| o.id == role).unwrap().content.clone();
        let at = td.objects[0].transform.clone();
        td.add_object(copy, at);
        assert_eq!(td.field_value(2), Some(TitleFieldValue::Text("Director".into())));
        // Deleted: the role's two fields go from the list.
        td.objects.retain(|o| o.id != role);
        let live: Vec<_> = td.live_fields().into_iter().map(|(i, ..)| i).collect();
        assert_eq!(live, [0, 1, 4, 5]);
        let before = td.clone();
        assert!(td.set_field(2, &TitleFieldValue::Text("x".into())).is_err());
        assert!(td.set_field(9, &TitleFieldValue::Text("x".into())).is_err(), "no such field");
        assert_eq!(td, before, "nothing changed");
        // Every layer that moves gone: no speed to set.
        td.objects.retain(|o| o.id != name);
        assert!(td.live_fields().iter().all(|(_, f, _)| !matches!(f.kind, FieldKind::Speed { .. })));
    }

    /// Size grows every layer from the anchor, its box and its type with
    /// it, and any run of moves back to 1.0 is the template exactly
    /// (#1724).
    #[test]
    fn size_grows_the_title_from_its_anchor_and_returns_exactly() {
        let (mut td, name, role) = lower_third();
        td.objects[1].transform.center = TitlePosition { x: 0.2, y: 0.9 };
        let made = td.clone();
        td.published = vec![PublishedField { label: "Size".into(), kind: td.size_field(TitlePosition { x: 0.1, y: 0.9 }) }];
        td.set_field(0, &TitleFieldValue::Size(2.0)).unwrap();
        let layer = |td: &TitleData, id| td.objects.iter().find(|o| o.id == id).unwrap().clone();
        let r = layer(&td, role);
        assert!((r.transform.center.x - 0.3).abs() < 1e-6 && (r.transform.center.y - 0.9).abs() < 1e-6, "{:?}", r.transform.center);
        let TitleObjectContent::TextBox(tb) = &layer(&td, name).content else { unreachable!() };
        assert_eq!(tb.font_size, 144.0);
        assert_eq!(td.field_value(0), Some(TitleFieldValue::Size(2.0)));
        for f in [0.7, 1.6, 9.0, 1.0] {
            td.set_field(0, &TitleFieldValue::Size(f)).unwrap();
        }
        let mut back = td.clone();
        back.published.clear();
        assert_eq!(back, made, "1.0 is the template as made");
        assert!(td.set_field(0, &TitleFieldValue::Speed(2.0)).is_err(), "a size takes no speed");
    }
}

