//! A title's published fields (#1139): what a motion-graphics template
//! keeps editable once applied. A template is a styled title saved with a
//! few fields named on it (a layer's text, a colour, how fast it moves),
//! and the inspector shows those first, the rest folded away, so a title
//! applied in one drag is changed with a box and a slider.
//!
//! Each field names its target by the layer's id, which a template keeps.
//! A field whose layer has gone (deleted since) or no longer holds what
//! the field reads is not live: shown nowhere, set refused, never a panic.

use crate::{TitleBackground, TitleData, TitleObjectContent, TitleObjectId};
use klipp_core::TitleFieldValue;
use serde::{Deserialize, Serialize};

/// How fast a title's motion may run against its template's.
pub const SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.25..=4.0;

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
        }
    }

    /// The published fields still live, with their place in the list: the
    /// ones the inspector shows.
    pub fn live_fields(&self) -> Vec<(usize, &PublishedField, TitleFieldValue)> {
        self.published.iter().enumerate().filter_map(|(i, f)| self.field_value(i).map(|v| (i, f, v))).collect()
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
            (FieldKind::Speed { base, .. }, TitleFieldValue::Speed(f)) => {
                let factor = f.clamp(*SPEED_RANGE.start(), *SPEED_RANGE.end());
                for b in &base {
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
                if let FieldKind::Speed { factor: f, .. } = &mut self.published[index].kind {
                    *f = factor;
                }
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
}
