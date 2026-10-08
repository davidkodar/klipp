//! The specialist editors a row names by [`Widget::Custom`](klipp_effects::registry::presentation::Widget::Custom): curves, a
//! mask stack, a file, a plug-in. The editors themselves come in M3
//! steps 6 (a plug-in's) and 11 (the grade's); until one is registered,
//! its row draws read-only, and the value it would edit is kept byte for
//! byte.

use super::row::RowCtx;
use klipp_effects::registry::ParamDesc;

/// A specialist editor for one or more of a node's parameters.
pub trait CustomEditor: Sync {
    /// Draw the editor for `desc`, whose stored value is `stored`, and push
    /// the path commands an edit implies into `ctx`. Never writes the node.
    fn show(&self, ui: &mut egui::Ui, ctx: &mut RowCtx, desc: &ParamDesc, stored: &serde_json::Value);
}

/// The registered editors, by id; steps 6 and 11 add theirs.
static EDITORS: &[(&str, &dyn CustomEditor)] = &[("frei0r_plugin", &super::adapters::Frei0rEditor)];

/// The editor registered under `id`, if any: a table, not a node-kind
/// match, so a new editor is one line.
pub fn editor(id: &str) -> Option<&'static dyn CustomEditor> {
    EDITORS.iter().find(|(name, _)| *name == id).map(|(_, e)| *e)
}

#[cfg(test)]
mod tests {
    #[test]
    fn editors_are_found_by_name() {
        assert!(super::editor("frei0r_plugin").is_some());
        assert!(super::editor("curves").is_none(), "step 11's");
        assert!(super::editor("").is_none());
    }
}
