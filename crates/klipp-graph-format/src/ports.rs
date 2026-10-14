//! Today's grade graphs number their ports; the graph format names them.
//! These turn a port number on a node of a given kind (its `NodeKind` name
//! as stored, such as `"Layer"`) into its key, and a key back into the
//! number. A number the kind doesn't have becomes `"p<n>"`, so an odd old
//! edge survives the trip.

use crate::PortKey;

/// The corrections whose port 1 is the matte that gates them, though their
/// own input count says one.
pub const GATEABLE: [&str; 8] = [
    "Corrector",
    "ColorBalance",
    "Exposure",
    "Contrast",
    "Saturation",
    "ColorWheels",
    "ToneControls",
    "RGBCurves",
];

fn named_input(kind: &str, port: u8) -> Option<String> {
    let names: &[&str] = match kind {
        "Source" | "ExternalMatte" => &[],
        "ApplyMatte" => &["in", "matte", "pregrade"],
        "Layer" => &["fg", "bg"],
        "Blend" | "CombineMattes" => &["a", "b"],
        "MaskCutout" => &["in", "matte"],
        // The grade's picture, and the matte it leaves with as its alpha
        // (#898).
        "Output" => &["in", "alpha"],
        "RGBCombiner" => &["r", "g", "b"],
        "ColorMatch" => &["in", "ref"],
        "LayerMixer" => return Some(format!("in{port}")),
        k if GATEABLE.contains(&k) => &["in", "mask"],
        _ => &["in"],
    };
    names.get(usize::from(port)).map(|s| (*s).to_owned())
}

fn named_output(kind: &str, port: u8) -> Option<String> {
    let names: &[&str] = match kind {
        "Output" => &[],
        "RGBSplitter" => &["r", "g", "b"],
        _ => &["out"],
    };
    names.get(usize::from(port)).map(|s| (*s).to_owned())
}

/// The key of input port `port` on a node of `kind`.
pub fn input_key(kind: &str, port: u8) -> PortKey {
    PortKey(named_input(kind, port).unwrap_or_else(|| format!("p{port}")))
}

/// The key of output port `port` on a node of `kind`.
pub fn output_key(kind: &str, port: u8) -> PortKey {
    PortKey(named_output(kind, port).unwrap_or_else(|| format!("p{port}")))
}

/// The input port number [`input_key`] gave `key`, or `None` for a key it
/// never gives on this kind.
pub fn input_index(kind: &str, key: &PortKey) -> Option<u8> {
    (0..=u8::MAX).find(|&n| input_key(kind, n) == *key)
}

/// The output port number [`output_key`] gave `key`, or `None`.
pub fn output_index(kind: &str, key: &PortKey) -> Option<u8> {
    (0..=u8::MAX).find(|&n| output_key(kind, n) == *key)
}
