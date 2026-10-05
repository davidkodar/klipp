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
    let subdir = match tool {
        ToolScope::Wheels => "wheels",
        ToolScope::Curves => "curves",
        ToolScope::HueSat => "huesat",
        ToolScope::Warper => "warper",
        ToolScope::Color => "color_basic",
        ToolScope::FullGrade => "full",
    };
    Some(user_presets_dir()?.join(subdir))
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
        .collect::<String>()
        .trim()
        .replace(' ', "_")
}

pub fn save_tool_preset(preset: &ToolPreset) -> std::io::Result<()> {
    let dir = tool_presets_dir(&preset.tool)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "Cannot find config directory"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", sanitize_filename(&preset.name)));
    let json = serde_json::to_string_pretty(preset)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

pub fn load_tool_presets(tool: &ToolScope) -> Vec<ToolPreset> {
    let Some(dir) = tool_presets_dir(tool) else { return vec![] };
    let Ok(entries) = std::fs::read_dir(&dir) else { return vec![] };
    let mut presets: Vec<ToolPreset> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            serde_json::from_str(&text).ok()
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
    let dir = user_presets_dir().ok_or("Cannot find config directory")?;
    let safe_name: String = name.chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let path = dir.join(format!("{safe_name}.json"));
    let json = serde_json::to_string_pretty(graph)
        .map_err(|e| format!("Serialize error: {e}"))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Write error: {e}"))?;
    Ok(())
}

/// Load presets from user directory.
fn load_user_presets() -> Vec<(String, NodeGraph)> {
    let Some(dir) = user_presets_dir() else { return Vec::new() };
    let mut presets = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return presets };
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

fn load_preset_file(path: &Path) -> Option<(String, NodeGraph)> {
    let text = std::fs::read_to_string(path).ok()?;
    let graph: NodeGraph = serde_json::from_str(&text).ok()?;
    let name = path.file_stem()?.to_string_lossy().to_string();
    Some((name, graph))
}
