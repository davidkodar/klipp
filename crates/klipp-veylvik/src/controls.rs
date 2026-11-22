//! A composition's published controls, changed from Klipp's inspector on
//! Edit: Veylvik's controls hand back its own commands, and these apply
//! them to the clip's composition as Veylvik's editor would, validated
//! and whole, giving the composition's new bytes and the step's name.

use std::sync::Arc;

use veylvik_document::{Command, Composition, Document};

/// The composition in `bytes` with `commands` applied, and the name of
/// the step; an error, and the composition as it was, when Veylvik
/// refuses them.
pub fn apply(bytes: &[u8], commands: &[Command]) -> Result<(Arc<[u8]>, String), String> {
    let before: Composition = serde_json::from_slice(bytes).map_err(|e| format!("the composition can't be read: {e}"))?;
    let label = veylvik_document::describe::step(commands, &before);
    let mut doc = Document::new(before).map_err(|e| e.to_string())?;
    doc.apply(0, commands).map_err(|e| e.to_string())?;
    let now: Arc<[u8]> = serde_json::to_vec(doc.snapshot()).map_err(|e| e.to_string())?.into();
    Ok((now, label))
}

/// One setting a composition shows on Edit, and its value now.
#[derive(Debug, Clone, PartialEq)]
pub struct Published {
    pub label: String,
    pub node: veylvik_document::NodeId,
    pub parameter: String,
    /// As the composition holds it: keys included when it is keyed.
    pub value: serde_json::Value,
}

/// The settings the composition in `bytes` shows on Edit, in its order:
/// its own, then its groups' controls.
pub fn published(bytes: &[u8]) -> Result<Vec<Published>, String> {
    let doc: Composition = serde_json::from_slice(bytes).map_err(|e| format!("the composition can't be read: {e}"))?;
    Ok(published_in(&doc))
}

fn published_in(doc: &Composition) -> Vec<Published> {
    veylvik_document::groups::published(doc)
        .into_iter()
        .map(|c| {
            let value = veylvik_document::inspection::node(doc, c.node, Some(&c.parameter)).map(|v| v["value"].clone()).unwrap_or_default();
            Published { label: c.label, node: c.node, parameter: c.parameter, value }
        })
        .collect()
}

/// The composition in `bytes` with each setting named in `set` (by its
/// label on Edit) given its value, all as one step; an error naming the
/// first label it doesn't show, or what Veylvik refused.
pub fn set_by_label(bytes: &[u8], set: &[(String, serde_json::Value)]) -> Result<(Arc<[u8]>, String), String> {
    let doc: Composition = serde_json::from_slice(bytes).map_err(|e| format!("the composition can't be read: {e}"))?;
    let shown = published_in(&doc);
    let commands = set
        .iter()
        .map(|(label, value)| {
            let c = shown.iter().find(|p| &p.label == label).ok_or_else(|| {
                let names: Vec<&str> = shown.iter().map(|p| p.label.as_str()).collect();
                format!("the composition shows no setting \"{label}\" on Edit; it shows: {}", names.join(", "))
            })?;
            Ok(Command::SetParameter { id: c.node, parameter: c.parameter.clone(), value: value.clone() })
        })
        .collect::<Result<Vec<_>, String>>()?;
    apply(bytes, &commands)
}
