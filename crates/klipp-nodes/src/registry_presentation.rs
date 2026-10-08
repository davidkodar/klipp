//! How the inspector shows a grade node's parameters (M3 step 0a), with
//! the effects' schema ([`klipp_effects::registry::presentation`]): this
//! catalog's table, by `(type id, key)`, filled by M3 step 0c. A parameter
//! the table names nothing for is shown as its description implies
//! ([`Presentation::derived`]).

use crate::registry::NodeType;
use klipp_effects::registry::presentation::{ParamEntry, Presentation, TypeEntry, TypePresentation};
use klipp_effects::registry::ParamDesc;

/// The grade nodes' entries (M3 step 0c).
pub static PARAMS: &[ParamEntry] = &[];
pub static TYPES: &[TypeEntry] = &[];

impl NodeType {
    /// How parameter `p` of this node type is shown.
    pub fn presentation(&self, p: &ParamDesc) -> Presentation {
        Presentation::of(PARAMS, self.desc.id, p)
    }

    /// How this node type is shown, beyond its rows.
    pub fn type_presentation(&self) -> TypePresentation {
        TypePresentation::of(TYPES, self.desc.id)
    }
}
