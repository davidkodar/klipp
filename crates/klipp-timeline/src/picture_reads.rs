//! A clip's plug-ins and built-in effects as its picture holds them (M4
//! step 12a): what the renderer reads for the nodes steps 3, 5 and 9 made
//! (an OFX plug-in, a frei0r plug-in, an analysis effect), rather than
//! the v1 lists, so those nodes run from the graph. The lists stay until
//! step 12f; a picture-truth clip's lists are a view of its picture, so
//! for it the two can differ only where the view can't hold what the
//! picture does.

use klipp_effects::BuiltinEffect;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::{types, Node};
use klipp_graph_lift::effect_params;

use crate::Clip;

/// A built-in effect node of a clip's picture.
#[derive(Debug, Clone, PartialEq)]
pub struct PictureEffect {
    pub node: uuid::Uuid,
    pub enabled: bool,
    /// The effect its values make.
    pub effect: BuiltinEffect,
}

/// An OFX plug-in node of a clip's picture, as the entry the host runs.
#[derive(Debug, Clone)]
pub struct PictureOfx {
    pub node: uuid::Uuid,
    /// Its plug-in, values, context and whether it runs.
    pub entry: klipp_plugin::AppliedEffect,
}

/// Where the node sits in its list.
fn slot(n: &Node) -> u32 {
    n.legacy.as_ref().and_then(|l| l.slot).unwrap_or(u32::MAX)
}

impl Clip {
    /// The clip's built-in effect nodes, in list order, each with the
    /// effect its values make: the plain effects (an analysis effect, a
    /// frei0r plug-in, a blur). The kinds the clip places itself (its
    /// placement, opacity, speed ramp and temporal noise reduction) are
    /// nodes of their own types and aren't among them.
    pub fn picture_effects(&self) -> Result<Vec<PictureEffect>, String> {
        let lifted = self.picture_v2()?;
        let mut out = Vec::new();
        for n in &lifted.picture.graph.nodes {
            if !matches!(n.type_id.as_str(), types::LEGACY_EFFECT | lt::FREI0R) {
                continue;
            }
            // Its values in its map, as the renderer reads them; a stored
            // frei0r node is the effect node it stands for.
            let mut held = n.clone();
            effect_params::into_memory(&mut held)?;
            let effect = effect_params::effect_of(&held)?;
            out.push((slot(n), PictureEffect { node: n.id, enabled: n.enabled, effect }));
        }
        out.sort_by_key(|(s, _)| *s);
        Ok(out.into_iter().map(|(_, e)| e).collect())
    }

    /// The clip's OFX plug-in nodes, in list order, as the entries the
    /// host runs: each with its own `enabled`, which says whether it runs.
    pub fn picture_ofx(&self) -> Result<Vec<PictureOfx>, String> {
        let lifted = self.picture_v2()?;
        let mut out = Vec::new();
        for n in &lifted.picture.graph.nodes {
            if !matches!(n.type_id.as_str(), types::LEGACY_OFX | lt::OFX) {
                continue;
            }
            // A stored `klipp.ofx` node as the entry it holds.
            let mut held = n.clone();
            effect_params::into_memory(&mut held)?;
            let payload = held.legacy.as_ref().map(|l| l.payload.clone()).ok_or_else(|| format!("OFX node {} holds no entry", n.id))?;
            let entry: crate::picture_v1::EffectJson = serde_json::from_value(payload).map_err(|e| format!("OFX node {}: {e}", n.id))?;
            out.push((slot(n), PictureOfx { node: n.id, entry: entry.to_effect() }));
        }
        out.sort_by_key(|(s, _)| *s);
        Ok(out.into_iter().map(|(_, e)| e).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_effects::BuiltinEffect as E;

    fn clip() -> Clip {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0))
    }

    /// What the picture holds is what's read: the built-in effects in
    /// order with their flags, the OFX entries with theirs.
    #[test]
    fn the_picture_reads_back_as_the_clip_holds_it() {
        let c = crate::test_clip::TestClip::of(clip())
            .effect(E::GaussianBlur { radius: 2.0 })
            .effect(E::Deflicker { strength: 1.0, window_secs: 0.4, gain: 1.0 })
            .off()
            .plugin(klipp_plugin::AppliedEffect { plugin_name: "org.example.A".into(), enabled: true, ..Default::default() })
            .plugin(klipp_plugin::AppliedEffect { plugin_name: "org.example.B".into(), enabled: false, ..Default::default() })
            .build();
        let effects = c.picture_effects().unwrap();
        assert_eq!(effects.iter().map(|e| (e.effect.clone(), e.enabled)).collect::<Vec<_>>(), vec![(E::GaussianBlur { radius: 2.0 }, true), (E::Deflicker { strength: 1.0, window_secs: 0.4, gain: 1.0 }, false)]);
        let ofx = c.picture_ofx().unwrap();
        assert_eq!(ofx.iter().map(|o| (o.entry.plugin_name.as_str(), o.entry.enabled)).collect::<Vec<_>>(), vec![("org.example.A", true), ("org.example.B", false)]);
    }

    /// A clip whose lists the test set apart from its picture reads its
    /// picture here (the divergence the renderer's tests build).
    #[test]
    fn a_diverged_clip_reads_its_picture() {
        let mut c = crate::test_clip::TestClip::of(clip()).effect(E::GaussianBlur { radius: 2.0 }).build();
        c.diverge_fields_for_test(|effects, enabled, ofx| {
            effects.clear();
            enabled.clear();
            ofx.push(klipp_plugin::AppliedEffect { plugin_name: "org.example.Only".into(), enabled: true, ..Default::default() });
        });
        assert!(c.builtin_effects().is_empty() && c.ofx_effects().len() == 1, "the lists as set");
        assert_eq!(c.picture_effects().unwrap().len(), 1, "the picture as it was");
        assert!(c.picture_ofx().unwrap().is_empty());
    }
}
