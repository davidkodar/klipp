//! Read-only canvas models. A grade adapter borrows its graph; the painter
//! needs no grade parameters or graph mutation access. Commands are mapped
//! by the model so another root can use the same interactions.

use std::collections::HashMap;
use klipp_core::AppCommand;
use klipp_effects::registry::{Category, PortDesc, PortType};
use klipp_nodes::{Edge, Node, NodeFrame, NodeGraph, NodeId, NodeKind, NodePos, NodeStickyNote, PortRole};

#[derive(Clone, Copy)]
pub(super) enum Palette { Grade, Matte, Composite, Look, Structural, Source, Output }

pub(super) trait CanvasKind: Copy {
    fn is_source(self) -> bool;
    fn is_output(self) -> bool;
    fn is_reroute(self) -> bool;
    fn is_group(self) -> bool;
    fn is_external_matte(self) -> bool;
    fn is_variadic(self) -> bool;
    fn is_terminal(self) -> bool { self.is_source() || self.is_output() }
    fn palette(self) -> Palette;
    fn glyph(self) -> &'static str;
    fn display_name(&self) -> &str;
    fn inputs(self) -> &'static [PortDesc];
    fn outputs(self) -> &'static [PortDesc];
    fn output_port_count(&self) -> u8;
    fn input_port_role(&self, port: u8) -> PortRole;
    fn output_port_role(&self, port: u8) -> PortRole;
    fn input_port_label(&self, port: u8) -> &str;
    fn output_port_label(&self, port: u8) -> &str;
}

pub(super) trait CanvasNode {
    type Kind<'a>: CanvasKind where Self: 'a;
    fn id(&self) -> NodeId;
    fn kind(&self) -> Self::Kind<'_>;
    fn label(&self) -> &str;
    fn enabled(&self) -> bool;
    fn effective_enabled(&self, time: f64) -> bool;
    fn input_count(&self) -> u8;
    fn modified(&self) -> bool;
    fn animated(&self) -> bool;
    fn child_count(&self) -> usize;
    fn matte_image(&self) -> Option<&std::path::Path>;
    fn output_link(&self) -> Option<&str>;
    fn input_links(&self) -> &[Option<String>];
}

pub(super) type MenuCategory = (&'static str, Vec<(&'static str, &'static str)>);

pub(super) trait CanvasGraph {
    type Node: CanvasNode;
    fn nodes(&self) -> &[Self::Node];
    fn edges(&self) -> &[Edge];
    fn positions(&self) -> &HashMap<NodeId, NodePos>;
    fn frames(&self) -> &[NodeFrame];
    fn sticky_notes(&self) -> &[NodeStickyNote];
    fn signal_role(&self, node: NodeId, port: u8) -> PortRole;
    fn add_menu(&self) -> &[MenuCategory];
    fn command(&self, command: AppCommand) -> Option<AppCommand>;
    /// Why `command` can't be made here, when it can't and the person
    /// asked for it (#1743): shown on the canvas, so a pick never does
    /// nothing in silence.
    fn refusal(&self, _command: &AppCommand) -> Option<String> { None }
    fn thumbnail_output(&self, clip: klipp_core::ClipId, node: NodeId, descent: &[NodeId]) -> Option<klipp_compose::InspectAt> {
        let mut at = klipp_compose::inspect::grade_node(clip, node);
        at.path.nodes.splice(1..1, descent.iter().copied());
        Some(at)
    }
    fn grade_tools(&self) -> bool { true }
    fn keyboard_enabled(&self) -> bool { true }
    /// Whether `node` takes an outside node (#898): a correction a key
    /// gates, on the chain. Only a grade's.
    fn takes_outside_node(&self, _node: NodeId) -> bool { false }
}

/// The grade's graph on the canvas, and whether the canvas has the keys
/// (#1742): it has them only while it is the focused panel, so Delete in
/// the timeline does not take the selected nodes with it.
pub(super) struct GradeCanvas<'a>(pub &'a NodeGraph, pub bool);

impl CanvasGraph for GradeCanvas<'_> {
    type Node = Node;
    fn nodes(&self) -> &[Node] { &self.0.nodes }
    fn edges(&self) -> &[Edge] { &self.0.edges }
    fn positions(&self) -> &HashMap<NodeId, NodePos> { &self.0.node_positions }
    fn frames(&self) -> &[NodeFrame] { &self.0.frames }
    fn sticky_notes(&self) -> &[NodeStickyNote] { &self.0.sticky_notes }
    fn signal_role(&self, node: NodeId, port: u8) -> PortRole { klipp_nodes::effective_signal_role_at(self.0, node, port) }
    fn add_menu(&self) -> &[MenuCategory] { grade_menu() }
    fn command(&self, command: AppCommand) -> Option<AppCommand> { Some(command) }
    fn keyboard_enabled(&self) -> bool { self.1 }
    fn takes_outside_node(&self, node: NodeId) -> bool { self.0.clone().add_outside_node(node).is_some() }
}

impl CanvasNode for Node {
    type Kind<'a> = NodeKind;
    fn id(&self) -> NodeId { self.id }
    fn kind(&self) -> NodeKind { self.kind }
    fn label(&self) -> &str { &self.label }
    fn enabled(&self) -> bool { self.enabled }
    fn effective_enabled(&self, time: f64) -> bool { klipp_nodes::effective_enabled(self, time) }
    fn input_count(&self) -> u8 { self.dynamic_input_port_count() }
    fn modified(&self) -> bool { !self.params.is_default() }
    fn animated(&self) -> bool { !self.keyframes.is_empty() }
    fn child_count(&self) -> usize { self.params.compound_subgraph.as_ref().map_or(0, |g| g.nodes.iter().filter(|n| !n.kind.is_terminal()).count()) }
    fn matte_image(&self) -> Option<&std::path::Path> { self.params.matte_image_path.as_deref() }
    fn output_link(&self) -> Option<&str> { self.output_link.as_deref() }
    fn input_links(&self) -> &[Option<String>] { &self.input_links }
}

impl CanvasKind for NodeKind {
    fn is_source(self) -> bool { self == NodeKind::Source }
    fn is_output(self) -> bool { self == NodeKind::Output }
    fn is_reroute(self) -> bool { self == NodeKind::Reroute }
    fn is_group(self) -> bool { self == NodeKind::Compound }
    fn is_external_matte(self) -> bool { self == NodeKind::ExternalMatte }
    fn is_variadic(self) -> bool { matches!(self, NodeKind::LayerMixer | NodeKind::KeyMixer) }
    fn palette(self) -> Palette { grade_palette(&self) }
    fn glyph(self) -> &'static str { grade_glyph(&self) }
    fn display_name(&self) -> &'static str { NodeKind::display_name(self) }
    fn inputs(self) -> &'static [PortDesc] { klipp_nodes::registry::node_type(self).map_or(&[], |t| t.desc.inputs) }
    fn outputs(self) -> &'static [PortDesc] { klipp_nodes::registry::node_type(self).map_or(&[], |t| t.desc.outputs) }
    fn output_port_count(&self) -> u8 { klipp_nodes::registry::node_type(*self).map_or_else(|| NodeKind::output_port_count(self), |_| self.outputs().len() as u8) }
    fn input_port_role(&self, port: u8) -> PortRole {
        // Grade gate sockets outside the kind's counted inputs keep their
        // existing routing colour; the catalog does not add visible sockets.
        match self.inputs().get(port as usize).filter(|_| port < NodeKind::input_port_count(self)).map(|p| p.ty) {
            Some(PortType::Matte) => PortRole::Matte,
            _ => NodeKind::input_port_role(self, port),
        }
    }
    fn output_port_role(&self, port: u8) -> PortRole {
        match self.outputs().get(port as usize).map(|p| p.ty) {
            Some(PortType::Matte) => PortRole::Matte,
            _ => NodeKind::output_port_role(self, port),
        }
    }
    fn input_port_label(&self, port: u8) -> &'static str { NodeKind::input_port_label(self, port) }
    fn output_port_label(&self, port: u8) -> &'static str { NodeKind::output_port_label(self, port) }
}

/// The other words an add menu entry is searched by, as its label
/// (#2576): what a colourist calls it, or the kind of key it is.
const SEARCH_ALIASES: &[(&str, &[&str])] = &[
    ("Qualifier", &["qualifier", "hsl", "hue", "key by colour", "key by color", "colour key", "color key", "secondary"]),
];

/// Whether the add menu's search `q`, lowercase and trimmed, finds the
/// entry `label`: in its label, or in one of its other words.
pub(super) fn menu_entry_matches(label: &str, q: &str) -> bool {
    label.to_lowercase().contains(q) || SEARCH_ALIASES.iter().any(|(l, words)| label.starts_with(l) && words.iter().any(|w| w.contains(q)))
}

/// The grade add menu: each node type its registry entry offers in the
/// menu, under its group, by its label and rank (#2888); the entry's
/// name is the kind's add name.
pub(super) fn grade_menu() -> &'static [MenuCategory] {
    use klipp_nodes::registry::{node_types, GradeMenuGroup, Offered};
    static MENU: std::sync::OnceLock<Vec<MenuCategory>> = std::sync::OnceLock::new();
    MENU.get_or_init(|| {
        GradeMenuGroup::ALL
            .into_iter()
            .map(|group| {
                let mut entries: Vec<(u8, &'static str, &'static str)> = node_types()
                    .filter_map(|t| match t.offered {
                        Offered::Menu { group: g, label, rank } if g == group => {
                            let name = klipp_nodes::kinds::addable_node_kinds().iter().find(|(_, k)| *k == t.kind).map(|(n, _)| *n)?;
                            Some((rank, label, name))
                        }
                        _ => None,
                    })
                    .collect();
                entries.sort_by_key(|e| e.0);
                (group.label(), entries.into_iter().map(|(_, label, name)| (label, name)).collect())
            })
            .filter(|(_, entries): &MenuCategory| !entries.is_empty())
            .collect()
    })
}


/// Catalog categories supply the palette for new canvas roots. Grade's
/// historical sub-families below are presentation overrides: every grade
/// descriptor has Category::Grade, but its existing colours must stay put.
pub(super) fn category_palette(category: Category) -> Palette {
    match category {
        Category::Colour | Category::Grade => Palette::Grade,
        Category::Key => Palette::Matte,
        Category::Geometry => Palette::Structural,
        Category::BlurDetail | Category::LightGlow | Category::FilmTemporal | Category::PlugIn => Palette::Look,
        Category::Composite => Palette::Composite,
    }
}

fn grade_palette(kind: &NodeKind) -> Palette {
    match kind {
        NodeKind::Source => Palette::Source,
        NodeKind::Output => Palette::Output,

        // Secondary / matte family.
        NodeKind::Qualifier
        | NodeKind::LumaKey
        | NodeKind::Mask
        | NodeKind::CombineMattes
        | NodeKind::KeyMixer
        | NodeKind::ExternalMatte
        | NodeKind::DifferenceKey
        | NodeKind::MaskCutout => Palette::Matte,

        // Composite family.
        NodeKind::Layer
        | NodeKind::Blend
        | NodeKind::ApplyMatte
        | NodeKind::LayerMixer => Palette::Composite,

        // Look / finishing family.
        NodeKind::Lut3D
        | NodeKind::FilmGrain
        | NodeKind::Denoise
        | NodeKind::SoftClip => Palette::Look,

        // Structural family — routing + containers.
        NodeKind::Reroute
        | NodeKind::Compound
        | NodeKind::RGBSplitter
        | NodeKind::RGBCombiner
        | NodeKind::PrimariesMatrix
        | NodeKind::InputTransform => Palette::Structural,

        // Everything else falls into the primary-grade family —
        // Corrector, Exposure, Contrast, Saturation, ToneControls,
        // ColorWheels, WheelSatBri, RGBCurves, HueSatCurves,
        // ColorBalance, ColorMatch, ColorWarper, plus future
        // colour-pipeline kinds.
        _ => klipp_nodes::registry::node_type(*kind).map_or(Palette::Grade, |t| category_palette(t.desc.category)),
    }
}

fn grade_glyph(kind: &NodeKind) -> &'static str {
    match kind {
        // Source = play (input flowing IN); Output = eject (the
        // final composite leaving the graph). Distinct so the two
        // terminals are visually unambiguous on the thumbnail
        // badge.
        NodeKind::Source => "\u{25B6}",   // ▶
        NodeKind::Output => "\u{23CF}",   // ⏏
        NodeKind::Compound => "\u{229E}",                  // ⊞
        NodeKind::Reroute
        | NodeKind::RGBSplitter
        | NodeKind::RGBCombiner
        | NodeKind::PrimariesMatrix => "\u{21B9}",         // ↹
        NodeKind::Qualifier
        | NodeKind::LumaKey
        | NodeKind::Mask
        | NodeKind::CombineMattes
        | NodeKind::KeyMixer
        | NodeKind::ExternalMatte
        | NodeKind::DifferenceKey
        | NodeKind::MaskCutout => "M",
        NodeKind::Layer
        | NodeKind::Blend
        | NodeKind::ApplyMatte
        | NodeKind::LayerMixer => "L",
        NodeKind::Lut3D
        | NodeKind::FilmGrain
        | NodeKind::ColorMatch
        | NodeKind::ColorWarper
        | NodeKind::Denoise => "F",
        // Everything else is the primary-correction family.
        _ => "C",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ports_keep_the_grade_sockets_labels_and_colours() {
        for t in klipp_nodes::registry::node_types() {
            let kind = t.kind;
            assert_eq!(CanvasKind::output_port_count(&kind), kind.output_port_count(), "{}", t.desc.id);
            assert_eq!(kind.inputs(), t.desc.inputs);
            assert_eq!(kind.outputs(), t.desc.outputs);
            // Include the optional gate socket beyond the counted inputs.
            for port in 0..=kind.input_port_count() {
                assert_eq!(CanvasKind::input_port_role(&kind, port), kind.input_port_role(port), "{} input {port}", t.desc.id);
                assert_eq!(CanvasKind::input_port_label(&kind, port), kind.input_port_label(port));
            }
            for port in 0..kind.output_port_count() {
                assert_eq!(CanvasKind::output_port_role(&kind, port), kind.output_port_role(port), "{} output {port}", t.desc.id);
                assert_eq!(CanvasKind::output_port_label(&kind, port), kind.output_port_label(port));
            }
        }
    }
}
