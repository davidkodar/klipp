//! A smart bin: a name over a set of rules, each a field, a comparison
//! and a value, that gathers the clips matching all of them, or any.
//! Kept in the project with the bins. The rules are data here; what a
//! rule makes of a clip is decided beside the clip, in klipp-media.

use crate::Field;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleOp {
    Contains,
    NotContains,
    Is,
    IsNot,
    AtLeast,
    AtMost,
    Before,
    After,
    IsSet,
    IsEmpty,
}

impl RuleOp {
    pub const ALL: [RuleOp; 10] = [
        RuleOp::Contains, RuleOp::NotContains, RuleOp::Is, RuleOp::IsNot, RuleOp::AtLeast, RuleOp::AtMost,
        RuleOp::Before, RuleOp::After, RuleOp::IsSet, RuleOp::IsEmpty,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RuleOp::Contains => "contains",
            RuleOp::NotContains => "does not contain",
            RuleOp::Is => "is",
            RuleOp::IsNot => "is not",
            RuleOp::AtLeast => "is at least",
            RuleOp::AtMost => "is at most",
            RuleOp::Before => "is before",
            RuleOp::After => "is after",
            RuleOp::IsSet => "is set",
            RuleOp::IsEmpty => "is empty",
        }
    }

    /// Whether the comparison takes a value to compare with.
    pub fn takes_value(self) -> bool {
        !matches!(self, RuleOp::IsSet | RuleOp::IsEmpty)
    }

    /// The comparisons that make sense for a field: dates take before
    /// and after, numbers at least and at most, flags only set or empty.
    pub fn for_field(field: Field) -> Vec<RuleOp> {
        if field.is_flag() {
            vec![RuleOp::IsSet, RuleOp::IsEmpty]
        } else if field.is_date() {
            vec![RuleOp::Before, RuleOp::After, RuleOp::Is, RuleOp::IsSet, RuleOp::IsEmpty]
        } else if matches!(field, Field::Duration | Field::Fps | Field::FileSize | Field::Rating | Field::Resolution) {
            vec![RuleOp::Is, RuleOp::IsNot, RuleOp::AtLeast, RuleOp::AtMost, RuleOp::IsSet, RuleOp::IsEmpty]
        } else {
            vec![RuleOp::Contains, RuleOp::NotContains, RuleOp::Is, RuleOp::IsNot, RuleOp::AtLeast, RuleOp::AtMost, RuleOp::IsSet, RuleOp::IsEmpty]
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartRule {
    pub field: Field,
    pub op: RuleOp,
    #[serde(default)]
    pub value: String,
}

impl SmartRule {
    pub fn new(field: Field) -> Self {
        let op = RuleOp::for_field(field)[0];
        Self { field, op, value: String::new() }
    }

    /// "Scene is 12", "Rating is at least 4", "Good is set".
    pub fn describe(&self) -> String {
        if self.op.takes_value() {
            format!("{} {} {}", self.field.label(), self.op.label(), self.value)
        } else {
            format!("{} {}", self.field.label(), self.op.label())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmartBin {
    pub id: String,
    pub name: String,
    /// Every rule must match; otherwise any one is enough.
    pub match_all: bool,
    pub rules: Vec<SmartRule>,
}

impl Default for SmartBin {
    fn default() -> Self {
        Self { id: String::new(), name: "Smart Bin".to_string(), match_all: true, rules: vec![SmartRule::new(Field::Rating)] }
    }
}

impl SmartBin {
    pub fn new(id: String, name: &str) -> Self {
        Self { id, name: name.to_string(), ..Default::default() }
    }

    /// "all of: Scene is 12; Rating is at least 4".
    pub fn describe(&self) -> String {
        let rules: Vec<String> = self.rules.iter().map(SmartRule::describe).collect();
        format!("{} of: {}", if self.match_all { "all" } else { "any" }, rules.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_describes_itself_and_offers_the_comparisons_its_field_takes() {
        let r = SmartRule { field: Field::Rating, op: RuleOp::AtLeast, value: "4".into() };
        assert_eq!(r.describe(), "Rating is at least 4");
        assert_eq!(SmartRule { field: Field::GoodTake, op: RuleOp::IsSet, value: String::new() }.describe(), "Good is set");
        assert_eq!(RuleOp::for_field(Field::GoodTake), vec![RuleOp::IsSet, RuleOp::IsEmpty]);
        assert!(RuleOp::for_field(Field::ShootDate).contains(&RuleOp::Before));
        assert!(!RuleOp::for_field(Field::Rating).contains(&RuleOp::Contains));
        let bin = SmartBin { id: "x".into(), name: "Keepers".into(), match_all: true, rules: vec![r, SmartRule::new(Field::GoodTake)] };
        assert_eq!(bin.describe(), "all of: Rating is at least 4; Good is set");
        let json = serde_json::to_string(&bin).unwrap();
        assert_eq!(serde_json::from_str::<SmartBin>(&json).unwrap(), bin);
    }
}
