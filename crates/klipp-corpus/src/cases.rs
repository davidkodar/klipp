//! Declared cases are exhaustive: a new name must choose its media and edits.
use klipp_effects::BuiltinEffect;
use klipp_nodes::{Edge, Node, NodeGraph, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, EffectEdit, GradeAccess, OfxEdit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Edit,
    Effects,
    Grade,
    Ofx,
    Frei0r,
    Temporal,
    Analysis,
}

pub const CASES: &[Case] = &[
    Case::Edit,
    Case::Effects,
    Case::Grade,
    Case::Ofx,
    Case::Frei0r,
    Case::Temporal,
    Case::Analysis,
];

impl Case {
    /// A control copy with this case's subject bypassed. The source and timing
    /// stay the same, so recording cannot bless a fixture that exercises nothing.
    pub fn bypass(self, sequence: &mut klipp_timeline::Sequence) -> Result<(), String> {
        for clip in &mut sequence.tracks[0].clips {
            match self {
                Self::Edit => {}
                Self::Grade => clip.set_grade(NodeGraph::default()),
                Self::Ofx => {
                    let n = clip.ofx_effects().len();
                    if n == 0 {
                        return Err("OFX witness has no plug-in".into());
                    }
                    clip.apply_ofx_edits(
                        &(0..n)
                            .map(|i| OfxEdit::SetEnabled(i, false))
                            .collect::<Vec<_>>(),
                    )?;
                }
                Self::Effects | Self::Frei0r | Self::Temporal | Self::Analysis => {
                    let n = clip.builtin_effects().len();
                    if n == 0 {
                        return Err(format!("{self}: witness has no effect"));
                    }
                    clip.apply_effect_edits(
                        &(0..n)
                            .map(|i| EffectEdit::SetEnabled(i, false))
                            .collect::<Vec<_>>(),
                    )
                    .ok_or("witness could not bypass the subject")??;
                }
            }
        }
        Ok(())
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Effects => "effects",
            Self::Grade => "grade",
            Self::Ofx => "ofx",
            Self::Frei0r => "frei0r",
            Self::Temporal => "temporal",
            Self::Analysis => "analysis",
        }
    }

    pub fn video(self) -> &'static str {
        match self {
            Self::Temporal => "noise.mkv",
            Self::Analysis => "flicker.mkv",
            Self::Edit | Self::Effects | Self::Grade | Self::Ofx | Self::Frei0r => "motion.mkv",
        }
    }

    /// Model edits before canonical IDs are assigned. These must leave the clip
    /// field-truth: its own serde intentionally does not preserve picture truth.
    pub fn before_ids(self, clip: &mut Clip, index: usize) -> Result<(), String> {
        match self {
            Self::Grade => clip.set_grade(grade(index)),
            Self::Ofx => {
                let entry = klipp_plugin::AppliedEffect {
                    plugin_name: "org.klipp.fixture.channels".into(),
                    enabled: true,
                    param_values: Default::default(),
                    context: klipp_plugin::OfxCategory::Filter,
                    transition_input: None,
                    transition_amount: 0.0,
                };
                clip.apply_ofx_edits(&[OfxEdit::Add(&entry)])?;
            }
            Self::Temporal => {
                let effect = BuiltinEffect::TemporalDenoise {
                    strength: 1.0,
                    threshold: 0.15,
                };
                clip.apply_effect_edits(&[EffectEdit::Add(&effect)])
                    .ok_or("temporal fixture refused picture edits")??;
            }
            Self::Edit | Self::Effects | Self::Frei0r | Self::Analysis => {}
        }
        if clip.is_picture_truth() {
            return Err(format!(
                "{}: canonicalizing this model would drop picture-only data",
                self.id()
            ));
        }
        Ok(())
    }

    /// Plain nodes added with explicit IDs after model canonicalization.
    pub fn effects(self, index: usize) -> Vec<(BuiltinEffect, bool)> {
        match self {
            Self::Effects => vec![
                (
                    BuiltinEffect::GaussianBlur {
                        radius: 2.0 + index as f32,
                    },
                    true,
                ),
                (BuiltinEffect::Sharpen { amount: 0.7 }, index == 0),
            ],
            Self::Grade => vec![(BuiltinEffect::GaussianBlur { radius: 1.5 }, true)],
            Self::Frei0r => vec![(
                BuiltinEffect::Frei0r {
                    // The corpus renderer runs with its generated root as cwd.
                    // Relative native-library paths keep the saved input portable.
                    type_id: "frei0r:plugins/colour.so:klipp-fixture-colour".into(),
                    params: vec![],
                },
                true,
            )],
            Self::Analysis => vec![(
                BuiltinEffect::Deflicker {
                    strength: 1.0,
                    window_secs: 0.4,
                    gain: 1.0,
                },
                true,
            )],
            Self::Edit | Self::Ofx | Self::Temporal => vec![],
        }
    }
}

impl std::fmt::Display for Case {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

fn grade(index: usize) -> NodeGraph {
    let node = |kind, params| Node {
        id: uuid::Uuid::new_v4(),
        kind,
        label: format!("{kind:?}"),
        enabled: true,
        params,
        keyframes: Default::default(),
        output_link: None,
        input_links: vec![],
    };
    let nodes = vec![
        node(NodeKind::Source, NodeParams::default()),
        node(
            NodeKind::Corrector,
            NodeParams {
                gain_master: Some(0.15 + index as f32 * 0.05),
                gamma_x: Some(0.07),
                saturation: Some(0.65),
                ..Default::default()
            },
        ),
        node(NodeKind::Output, NodeParams::default()),
    ];
    let edges = (0..2)
        .map(|i| Edge {
            from_node: nodes[i].id,
            from_port: 0,
            to_node: nodes[i + 1].id,
            to_port: 0,
        })
        .collect();
    let node_positions = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            (
                n.id,
                NodePos {
                    x: i as f32 * 160.0,
                    y: 0.0,
                },
            )
        })
        .collect();
    NodeGraph {
        output_node_id: nodes[2].id,
        nodes,
        edges,
        node_positions,
        working_space_override: None,
        frames: vec![],
        sticky_notes: vec![],
    }
}
