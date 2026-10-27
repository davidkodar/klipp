use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum TemplateValue {
    Text(String),
    Color([f32; 4]),
    Number(f64),
    Bool(bool),
    Enum(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateInstance {
    pub template_id: Uuid,
    /// Resolved parameter-to-property paths copied from the template
    /// definition at insertion time. Keeping this with the instance lets the
    /// renderer apply values without depending on the UI catalogue.
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, TemplateValue>,
    #[serde(default)]
    pub media_slots: BTreeMap<String, String>,
    /// A control's links to what it drives (#2757): each its own range
    /// and curve, and the user's offset on top or the value it was left
    /// at when unlinked. Copied from the template at insertion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<TemplateLink>,
}

/// How a control's value goes over to what it drives (#2757).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkCurve {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl LinkCurve {
    /// `t` (0..1) shaped by the curve.
    pub fn at(self, t: f64) -> f64 {
        let t = t.clamp(0.0, 1.0);
        match self {
            LinkCurve::Linear => t,
            LinkCurve::EaseIn => t * t,
            LinkCurve::EaseOut => 1.0 - (1.0 - t) * (1.0 - t),
            LinkCurve::EaseInOut => if t < 0.5 { 2.0 * t * t } else { 1.0 - 2.0 * (1.0 - t) * (1.0 - t) },
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LinkCurve::Linear => "linear",
            LinkCurve::EaseIn => "ease_in",
            LinkCurve::EaseOut => "ease_out",
            LinkCurve::EaseInOut => "ease_in_out",
        }
    }
}

/// One link of a control (macro, #2757): the control `parameter` drives
/// `property`, its value from `from` mapped onto `to` along `curve`. A
/// manual change is an `offset` on top; unlinked, the property keeps
/// `frozen` and follows the control no more.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateLink {
    pub parameter: String,
    pub property: String,
    pub from: [f64; 2],
    pub to: [f64; 2],
    #[serde(default)]
    pub curve: LinkCurve,
    #[serde(default)]
    pub offset: f64,
    #[serde(default = "linked")]
    pub linked: bool,
    #[serde(default)]
    pub frozen: f64,
}

fn linked() -> bool {
    true
}

impl TemplateLink {
    /// What the control at `control` makes the property, before the
    /// offset.
    pub fn mapped(&self, control: f64) -> f64 {
        let span = self.from[1] - self.from[0];
        let t = if span.abs() < 1e-12 { 0.0 } else { (control - self.from[0]) / span };
        self.to[0] + (self.to[1] - self.to[0]) * self.curve.at(t)
    }

    /// The property's value with the control at `control`: mapped plus
    /// the offset while linked, the value it was left at when not.
    pub fn effective(&self, control: f64) -> f64 {
        if self.linked { self.mapped(control) + self.offset } else { self.frozen }
    }

    /// Stops following the control, keeping the value it has now.
    pub fn unlink(&mut self, control: f64) {
        if self.linked {
            self.frozen = self.effective(control);
            self.linked = false;
        }
    }

    /// Follows the control again from the value it has now: the offset
    /// is what keeps it there, so nothing jumps.
    pub fn relink(&mut self, control: f64) {
        if !self.linked {
            self.offset = self.frozen - self.mapped(control);
            self.linked = true;
        }
    }

    /// Snaps to the control's value.
    pub fn reset_offset(&mut self) {
        self.offset = 0.0;
    }

    /// A value set by hand: while linked, the offset that gives it; when
    /// not, the value kept.
    pub fn set_value(&mut self, control: f64, value: f64) {
        if self.linked {
            self.offset = value - self.mapped(control);
        } else {
            self.frozen = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(curve: LinkCurve) -> TemplateLink {
        TemplateLink { parameter: "water".into(), property: "object:0.size".into(), from: [0.0, 1.0], to: [40.0, 120.0], curve, offset: 0.0, linked: true, frozen: 0.0 }
    }

    /// #2757: a link maps its control along its curve; a value set by hand
    /// is an offset the control keeps adding to; unlinked it stays where
    /// it was; relinked nothing jumps; reset snaps to the control.
    #[test]
    fn a_link_maps_offsets_unlinks_and_relinks_without_a_jump() {
        assert_eq!(link(LinkCurve::Linear).effective(0.5), 80.0);
        assert_eq!(link(LinkCurve::EaseIn).effective(0.5), 60.0);
        assert_eq!(link(LinkCurve::EaseOut).effective(0.5), 100.0);
        assert_eq!(link(LinkCurve::EaseInOut).effective(0.25), 50.0);
        let mut l = link(LinkCurve::Linear);
        l.set_value(0.5, 90.0);
        assert_eq!(l.offset, 10.0);
        assert_eq!(l.effective(1.0), 130.0, "the control still drives it, the offset on top");
        l.unlink(1.0);
        assert_eq!(l.effective(0.0), 130.0, "unlinked: where it was");
        l.relink(0.25);
        assert_eq!(l.effective(0.25), 130.0, "relinked: no jump");
        assert!((l.offset - 70.0).abs() < 1e-9);
        l.reset_offset();
        assert_eq!(l.effective(0.25), 60.0);
    }
}
