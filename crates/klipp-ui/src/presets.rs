use klipp_nodes::{NodeGraph, NodeKind, NodeParams};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Tool-scoped preset types ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToolScope {
    Wheels,
    Curves,
    HueSat,
    Warper,
    Color,
    FullGrade,
}

impl ToolScope {
    pub fn label(&self) -> &'static str {
        match self {
            ToolScope::Wheels => "Wheels",
            ToolScope::Curves => "Curves",
            ToolScope::HueSat => "Hue/Sat",
            ToolScope::Warper => "Warper",
            ToolScope::Color => "Colour",
            ToolScope::FullGrade => "Full Grade",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPreset {
    pub name: String,
    pub tool: ToolScope,
    pub params: NodeParams,
}

// ── Directory helpers ──

/// Get the user presets directory, creating it if needed.
pub fn user_presets_dir() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("klipp").join("presets").join("color");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).ok()?;
    }
    Some(dir)
}

pub fn tool_presets_dir(tool: &ToolScope) -> Option<PathBuf> {
    Some(user_presets_dir()?.join(tool_dir_name(tool)))
}

/// The folder under `presets/color` a tool's presets are kept in.
fn tool_dir_name(tool: &ToolScope) -> &'static str {
    match tool {
        ToolScope::Wheels => "wheels",
        ToolScope::Curves => "curves",
        ToolScope::HueSat => "huesat",
        ToolScope::Warper => "warper",
        ToolScope::Color => "color_basic",
        ToolScope::FullGrade => "full",
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
        .collect::<String>()
        .trim()
        .replace(' ', "_")
}

/// Whether the tool preset file `text` is one this build reads in full:
/// the test `load_tool_presets` applies before offering it.
fn tool_preset_reads_in_full(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text).is_ok_and(|v| {
        v.get("params").map(klipp_nodes::json_check::params_losses).unwrap_or_default().is_empty()
            && serde_json::from_value::<ToolPreset>(v).is_ok()
    })
}

/// Whether the colour preset file `text` is one this build reads in full:
/// the test `load_preset_file` applies before offering it.
/// A colour preset file (`<file_name>`, extension included) as this Klipp
/// reads it (#1089): a revision-2 preset brought back through `unlift`, and
/// only one this build reads in full (#993). A preset from before revision
/// 2 is refused, naming the converter.
fn read_colour_preset(file_name: &str, text: &str) -> Result<NodeGraph, String> {
    let v2: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    if let Some(why) = klipp_project::side_store_revision_refusal(&v2) {
        return Err(why);
    }
    let v1 = serde_json::to_string(&klipp_graph_lift::unlift::colour_preset(file_name, &v2)?).map_err(|e| e.to_string())?;
    if !klipp_nodes::json_check::reads_in_full(&v1) {
        return Err("saved by a newer Klipp".into());
    }
    serde_json::from_str(&v1).map_err(|e| format!("corrupt: {e}"))
}

/// Refuses to write over `path` when it holds a preset this build cannot
/// read in full. Loading leaves such a preset out, so the user cannot see
/// it, and a save under the same name would replace it unseen (#1050).
fn refuse_unreadable(path: &Path, reads_in_full: impl Fn(&str) -> bool) -> Result<(), String> {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(bytes) if std::str::from_utf8(&bytes).is_ok_and(&reads_in_full) => Ok(()),
        _ => Err(format!(
            "A preset named {:?} is already saved, by a newer Klipp or in a form this one cannot read, and is kept. Choose another name.",
            path.file_stem().unwrap_or_default()
        )),
    }
}

pub fn save_tool_preset(preset: &ToolPreset) -> Result<(), String> {
    save_tool_preset_in(&tool_presets_dir(&preset.tool).ok_or("Cannot find config directory")?, preset)
}

fn save_tool_preset_in(dir: &Path, preset: &ToolPreset) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {dir:?}: {e}"))?;
    let path = dir.join(format!("{}.json", sanitize_filename(&preset.name)));
    refuse_unreadable(&path, tool_preset_reads_in_full)?;
    let json = serde_json::to_string_pretty(preset).map_err(|e| format!("Serialize error: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("Write error: {e}"))
}

pub fn load_tool_presets(tool: &ToolScope) -> Vec<ToolPreset> {
    let Some(dir) = tool_presets_dir(tool) else { return vec![] };
    load_tool_presets_in(&dir)
}

fn load_tool_presets_in(dir: &Path) -> Vec<ToolPreset> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut presets: Vec<ToolPreset> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            // A preset holding settings this build would drop is not
            // offered: applied, they would be lost (#993). Left as it is.
            let value: serde_json::Value = serde_json::from_str(&text).ok()?;
            let lost = value.get("params").map(klipp_nodes::json_check::params_losses).unwrap_or_default();
            if !lost.is_empty() {
                tracing::warn!("Leaving tool preset {:?} as it is: saved by a newer Klipp ({})", e.path(), lost.join(", "));
                return None;
            }
            serde_json::from_value(value).ok()
        })
        .collect();
    presets.sort_by(|a, b| a.name.cmp(&b.name));
    presets
}

pub fn delete_tool_preset(tool: &ToolScope, name: &str) -> std::io::Result<()> {
    let dir = tool_presets_dir(tool)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Cannot find config directory"))?;
    let path = dir.join(format!("{}.json", sanitize_filename(name)));
    std::fs::remove_file(path)
}

// ── Extract / Apply helpers ──

pub fn extract_tool_params(graph: &NodeGraph, tool: &ToolScope) -> NodeParams {
    let mut params = NodeParams::default();
    match tool {
        ToolScope::Wheels => {
            if let Some(node) = graph.nodes.iter().find(|n| n.kind == NodeKind::ColorWheels) {
                let p = &node.params;
                params.lift_x = p.lift_x;
                params.lift_y = p.lift_y;
                params.lift_master = p.lift_master;
                params.gamma_x = p.gamma_x;
                params.gamma_y = p.gamma_y;
                params.gamma_master = p.gamma_master;
                params.gain_x = p.gain_x;
                params.gain_y = p.gain_y;
                params.gain_master = p.gain_master;
                params.offset_x = p.offset_x;
                params.offset_y = p.offset_y;
                params.offset_master = p.offset_master;
                params.wheels_log_mode = p.wheels_log_mode;
                // Log wheels
                params.log_low_x = p.log_low_x;
                params.log_low_y = p.log_low_y;
                params.log_low_master = p.log_low_master;
                params.log_mid_x = p.log_mid_x;
                params.log_mid_y = p.log_mid_y;
                params.log_mid_master = p.log_mid_master;
                params.log_high_x = p.log_high_x;
                params.log_high_y = p.log_high_y;
                params.log_high_master = p.log_high_master;
                // Where the log bands sit and the luma mix (#900)
                params.log_low_range = p.log_low_range;
                params.log_high_range = p.log_high_range;
                params.luma_mix = p.luma_mix;
            }
            if let Some(node) = graph.nodes.iter().find(|n| n.kind == NodeKind::WheelSatBri) {
                let p = &node.params;
                params.lift_sat = p.lift_sat;
                params.lift_bri = p.lift_bri;
                params.gamma_sat = p.gamma_sat;
                params.gamma_bri = p.gamma_bri;
                params.gain_sat = p.gain_sat;
                params.gain_bri = p.gain_bri;
                params.offset_sat = p.offset_sat;
                params.offset_bri = p.offset_bri;
            }
        }
        ToolScope::Curves => {
            if let Some(node) = graph.nodes.iter().find(|n| n.kind == NodeKind::RGBCurves) {
                params.curves = node.params.curves.clone();
                params.curves_preserve_luma = node.params.curves_preserve_luma;
                params.curves_mix = node.params.curves_mix;
            }
        }
        ToolScope::HueSat => {
            if let Some(node) = graph.nodes.iter().find(|n| n.kind == NodeKind::HueSatCurves) {
                params.huesat = node.params.huesat.clone();
                params.huesat_mix = node.params.huesat_mix;
            }
        }
        ToolScope::Warper => {
            if let Some(node) = graph.nodes.iter().find(|n| n.kind == NodeKind::ColorWarper) {
                params.warper_params = node.params.warper_params.clone();
            }
        }
        ToolScope::Color => {
            for node in &graph.nodes {
                match node.kind {
                    NodeKind::Exposure => {
                        params.exposure = node.params.exposure;
                    }
                    NodeKind::Contrast => {
                        params.contrast = node.params.contrast;
                        params.clarity = node.params.clarity;
                    }
                    NodeKind::Saturation => {
                        params.saturation = node.params.saturation;
                        params.vibrance = node.params.vibrance;
                    }
                    NodeKind::ToneControls => {
                        params.highlights = node.params.highlights;
                        params.shadows = node.params.shadows;
                        params.whites = node.params.whites;
                        params.blacks = node.params.blacks;
                    }
                    _ => {}
                }
            }
        }
        ToolScope::FullGrade => {
            // Full grade handled separately via existing NodeGraph serialization
        }
    }
    params
}

pub fn apply_tool_preset(graph: &mut NodeGraph, preset: &ToolPreset) {
    let p = &preset.params;
    match &preset.tool {
        ToolScope::Wheels => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::ColorWheels) {
                let np = &mut node.params;
                if p.lift_x.is_some() { np.lift_x = p.lift_x; }
                if p.lift_y.is_some() { np.lift_y = p.lift_y; }
                if p.lift_master.is_some() { np.lift_master = p.lift_master; }
                if p.gamma_x.is_some() { np.gamma_x = p.gamma_x; }
                if p.gamma_y.is_some() { np.gamma_y = p.gamma_y; }
                if p.gamma_master.is_some() { np.gamma_master = p.gamma_master; }
                if p.gain_x.is_some() { np.gain_x = p.gain_x; }
                if p.gain_y.is_some() { np.gain_y = p.gain_y; }
                if p.gain_master.is_some() { np.gain_master = p.gain_master; }
                if p.offset_x.is_some() { np.offset_x = p.offset_x; }
                if p.offset_y.is_some() { np.offset_y = p.offset_y; }
                if p.offset_master.is_some() { np.offset_master = p.offset_master; }
                if p.wheels_log_mode.is_some() { np.wheels_log_mode = p.wheels_log_mode; }
                if p.log_low_x.is_some() { np.log_low_x = p.log_low_x; }
                if p.log_low_y.is_some() { np.log_low_y = p.log_low_y; }
                if p.log_low_master.is_some() { np.log_low_master = p.log_low_master; }
                if p.log_mid_x.is_some() { np.log_mid_x = p.log_mid_x; }
                if p.log_mid_y.is_some() { np.log_mid_y = p.log_mid_y; }
                if p.log_mid_master.is_some() { np.log_mid_master = p.log_mid_master; }
                if p.log_high_x.is_some() { np.log_high_x = p.log_high_x; }
                if p.log_high_y.is_some() { np.log_high_y = p.log_high_y; }
                if p.log_high_master.is_some() { np.log_high_master = p.log_high_master; }
                if p.log_low_range.is_some() { np.log_low_range = p.log_low_range; }
                if p.log_high_range.is_some() { np.log_high_range = p.log_high_range; }
                if p.luma_mix.is_some() { np.luma_mix = p.luma_mix; }
            }
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::WheelSatBri) {
                let np = &mut node.params;
                if p.lift_sat.is_some() { np.lift_sat = p.lift_sat; }
                if p.lift_bri.is_some() { np.lift_bri = p.lift_bri; }
                if p.gamma_sat.is_some() { np.gamma_sat = p.gamma_sat; }
                if p.gamma_bri.is_some() { np.gamma_bri = p.gamma_bri; }
                if p.gain_sat.is_some() { np.gain_sat = p.gain_sat; }
                if p.gain_bri.is_some() { np.gain_bri = p.gain_bri; }
                if p.offset_sat.is_some() { np.offset_sat = p.offset_sat; }
                if p.offset_bri.is_some() { np.offset_bri = p.offset_bri; }
            }
        }
        ToolScope::Curves => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::RGBCurves) {
                if p.curves.is_some() { node.params.curves = p.curves.clone(); }
                if p.curves_preserve_luma.is_some() { node.params.curves_preserve_luma = p.curves_preserve_luma; }
                if p.curves_mix.is_some() { node.params.curves_mix = p.curves_mix; }
            }
        }
        ToolScope::HueSat => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::HueSatCurves) {
                if p.huesat.is_some() { node.params.huesat = p.huesat.clone(); }
                if p.huesat_mix.is_some() { node.params.huesat_mix = p.huesat_mix; }
            }
        }
        ToolScope::Warper => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.kind == NodeKind::ColorWarper) {
                if p.warper_params.is_some() { node.params.warper_params = p.warper_params.clone(); }
            }
        }
        ToolScope::Color => {
            for node in graph.nodes.iter_mut() {
                match node.kind {
                    NodeKind::Exposure => {
                        if p.exposure.is_some() { node.params.exposure = p.exposure; }
                    }
                    NodeKind::Contrast => {
                        if p.contrast.is_some() { node.params.contrast = p.contrast; }
                        if p.clarity.is_some() { node.params.clarity = p.clarity; }
                    }
                    NodeKind::Saturation => {
                        if p.saturation.is_some() { node.params.saturation = p.saturation; }
                        if p.vibrance.is_some() { node.params.vibrance = p.vibrance; }
                    }
                    NodeKind::ToneControls => {
                        if p.highlights.is_some() { node.params.highlights = p.highlights; }
                        if p.shadows.is_some() { node.params.shadows = p.shadows; }
                        if p.whites.is_some() { node.params.whites = p.whites; }
                        if p.blacks.is_some() { node.params.blacks = p.blacks; }
                    }
                    _ => {}
                }
            }
        }
        ToolScope::FullGrade => {}
    }
}

// ── Legacy full-graph presets (existing API) ──

/// Load all presets: built-in + user directory.
pub fn load_all_presets() -> Vec<(String, NodeGraph)> {
    let mut presets = Vec::new();
    presets.extend(load_user_presets());
    presets
}

/// Save a grade preset to the user directory.
pub fn save_preset(name: &str, graph: &NodeGraph) -> Result<(), String> {
    save_preset_in(&user_presets_dir().ok_or("Cannot find config directory")?, name, graph)
}

pub(crate) fn save_preset_in(dir: &Path, name: &str, graph: &NodeGraph) -> Result<(), String> {
    let file_name = format!("{}.json", preset_stem(name));
    let path = dir.join(&file_name);
    refuse_unreadable(&path, |t| read_colour_preset(&file_name, t).is_ok())?;
    // Revision 2 (#1089), through text so an f32 keeps today's digits.
    let text = serde_json::to_string(graph).map_err(|e| format!("Serialize error: {e}"))?;
    let v1: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Serialize error: {e}"))?;
    let (v2, notes) = klipp_graph_lift::revision2::colour_preset_noted(&file_name, &v1).map_err(|e| format!("Revision 2 error: {e}"))?;
    if let Some(n) = notes.iter().find(|n| klipp_graph_lift::revision2::is_lost(n)) {
        return Err(format!("Saving the preset would lose part of it ({n:?}), so it was not saved"));
    }
    let json = serde_json::to_string_pretty(&v2).map_err(|e| format!("Serialize error: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Write error: {e}"))?;
    Ok(())
}

/// The file stem a whole-grade preset named `name` is saved under.
pub(crate) fn preset_stem(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// The whole-grade presets in `dir`, by name.
pub(crate) fn load_presets_in(dir: &Path) -> Vec<(String, NodeGraph)> {
    let mut presets = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return presets };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Some(preset) = load_preset_file(&path) {
                presets.push(preset);
            }
        }
    }
    presets.sort_by(|a, b| a.0.cmp(&b.0));
    presets
}

/// Load presets from user directory.
fn load_user_presets() -> Vec<(String, NodeGraph)> {
    user_presets_dir().map(|dir| load_presets_in(&dir)).unwrap_or_default()
}

fn load_preset_file(path: &Path) -> Option<(String, NodeGraph)> {
    let text = std::fs::read_to_string(path).ok()?;
    // A preset this build can't read in full, or from before revision 2, is
    // not offered and is left as it is (#993, #1089).
    let file_name = path.file_name()?.to_string_lossy().to_string();
    let graph = match read_colour_preset(&file_name, &text) {
        Ok(g) => g,
        Err(why) => {
            tracing::warn!("Leaving colour preset {:?} as it is: {why}", path);
            return None;
        }
    };
    let name = path.file_stem()?.to_string_lossy().to_string();
    Some((name, graph))
}

#[cfg(test)]
mod overwrite_tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-presets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// #1050: a preset this build cannot read is not offered, so a save
    /// under its name must not replace it unseen.
    #[test]
    fn a_save_leaves_an_unreadable_tool_preset_untouched() {
        let dir = temp_dir("tool");
        let saved = r#"{"name":"Warm","tool":"Wheels","params":{"exposure":0.5,"future_mix":0}}"#;
        let path = dir.join("Warm.json");
        std::fs::write(&path, saved).unwrap();
        let preset = ToolPreset { name: "Warm".into(), tool: ToolScope::Wheels, params: NodeParams::default() };
        assert!(save_tool_preset_in(&dir, &preset).is_err(), "the save went through");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved, "the preset was overwritten");
        // Under another name it saves.
        let other = ToolPreset { name: "Cool".into(), ..preset };
        assert!(save_tool_preset_in(&dir, &other).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_leaves_an_unreadable_colour_preset_untouched() {
        let dir = temp_dir("colour");
        let mut graph = serde_json::to_value(NodeGraph::default()).unwrap();
        graph["nodes"][1]["kind"] = serde_json::Value::from("HolographicKey");
        let saved = graph.to_string();
        let path = dir.join("Teal.json");
        std::fs::write(&path, &saved).unwrap();
        assert!(save_preset_in(&dir, "Teal", &NodeGraph::default()).is_err(), "the save went through");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), saved, "the preset was overwritten");
        // A readable (revision-2) one is replaced as before.
        std::fs::remove_file(&path).unwrap();
        assert!(save_preset_in(&dir, "Teal", &NodeGraph::default()).is_ok());
        assert!(save_preset_in(&dir, "Teal", &NodeGraph::default()).is_ok(), "a revision-2 preset is replaced");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1089: a colour preset is written as revision 2 and read back as the
    /// same graph.
    #[test]
    fn a_colour_preset_round_trips_through_revision_2() {
        let dir = temp_dir("colour-rt");
        let mut graph = NodeGraph::default();
        graph.nodes.iter_mut().filter(|n| n.kind == NodeKind::Corrector).for_each(|n| n.params.exposure = Some(0.4));
        save_preset_in(&dir, "Warm", &graph).unwrap();
        let path = dir.join("Warm.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["format_revision"], serde_json::json!(klipp_project::SIDE_STORE_REVISION), "written as revision 2");
        let (name, back) = load_preset_file(&path).expect("read back");
        assert_eq!(name, "Warm");
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&graph).unwrap(), "the graph comes back unchanged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1089: a colour preset from before revision 2 isn't offered, names
    /// the converter, and is left as it is.
    #[test]
    fn a_colour_preset_from_before_revision_2_is_left_alone() {
        let dir = temp_dir("colour-v1");
        let path = dir.join("Old.json");
        let v1 = serde_json::to_string(&NodeGraph::default()).unwrap();
        std::fs::write(&path, &v1).unwrap();
        assert!(load_preset_file(&path).is_none(), "an unconverted preset is not offered");
        let why = read_colour_preset("Old.json", &v1).unwrap_err();
        assert!(why.contains("klipp-convert run"), "{why}");
        assert!(save_preset_in(&dir, "Old", &NodeGraph::default()).is_err(), "and not written over");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), v1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod conversion_tests {
    use super::*;
    use crate::effect_presets::{preset_type_id, EffectPreset, PresetManager};

    fn ok_parity(_: &serde_json::Value, _: klipp_convert::StoreKind) -> Result<(), String> {
        Ok(())
    }

    /// Tool and single-effect presets saved by the app read back the same
    /// after the one-time conversion has run over the config folder: it
    /// leaves them at revision 2 as they are (#1126), beside a colour
    /// preset it converts.
    #[test]
    fn tool_and_effect_presets_read_the_same_after_the_conversion() {
        let root = std::env::temp_dir().join(format!("klipp-ui-presets-conversion-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let config = root.join("klipp");
        let colour = config.join("presets").join("color");
        let tool = ToolPreset { name: "Warm lift".into(), tool: ToolScope::Wheels, params: NodeParams { exposure: Some(0.3), ..Default::default() } };
        let tool_dir = colour.join(tool_dir_name(&ToolScope::Wheels));
        save_tool_preset_in(&tool_dir, &tool).unwrap();
        let effect = klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 };
        let key = preset_type_id(&effect).unwrap();
        let saved = EffectPreset { name: "Soft".into(), description: "a soft blur".into(), effect_key: key.into(), builtin: Some(effect), ofx_params: None, is_builtin: false };
        PresetManager::with_user_dir(config.join("effect_presets")).save_user_preset(saved.clone());
        let mut graph = NodeGraph::default();
        graph.ensure_corrector();
        std::fs::write(colour.join("Look.json"), serde_json::to_string(&graph).unwrap()).unwrap();
        let user = |m: &PresetManager| m.presets_for(key).into_iter().filter(|p| !p.is_builtin).map(|p| serde_json::to_value(p).unwrap()).collect::<Vec<_>>();
        let tool_before = serde_json::to_value(load_tool_presets_in(&tool_dir)).unwrap();
        let effect_before = user(&PresetManager::with_user_dir(config.join("effect_presets")));
        assert_eq!(effect_before, vec![serde_json::to_value(&saved).unwrap()]);

        let s = klipp_convert::run::Settings {
            projects: Vec::new(),
            excluded: Default::default(),
            config_dir: Some(config.clone()),
            backup_root: root.join("backup"),
            date: "2026-10-06".into(),
        };
        let report = klipp_convert::run::run(&s, &|| Ok(()), &ok_parity, &klipp_convert::run::Hooks::NONE).unwrap();
        assert_eq!((report.presets_converted, report.tool_presets_unchanged, report.effect_presets_unchanged), (1, 1, 1), "{report:?}");

        assert_eq!(serde_json::to_value(load_tool_presets_in(&tool_dir)).unwrap(), tool_before);
        assert_eq!(load_tool_presets_in(&tool_dir).len(), 1);
        assert_eq!(user(&PresetManager::with_user_dir(config.join("effect_presets"))), effect_before);
        let _ = std::fs::remove_dir_all(&root);
    }
}
