//! The effect specialists' Pixels: what their workflows and editors
//! leave on the clip drawn by the real renderer (`parity_pixels`), on the
//! routes each declares: the frame changed, the CPU's within a level of
//! the GPU's.
//!
//! - An analysis workflow (Deflicker, Stabilisation): its measurement
//!   landing as the app's drain lands it, the file's analysis (M4 step 9),
//!   must change the unmeasured clip's frame.
//!   Stabilisation's layer is drawn on the CPU by the stack's temporal
//!   gate (M4 step 9), as in the numeric family.
//! - The frei0r plug-in editor: a real installed plug-in (`saturat0r`,
//!   from frei0r-plugins), its value typed through the editor and stored
//!   as typed. Its
//!   parameter's default (0, a grey picture) keeps the frame drawn, where
//!   a plug-in whose 0 is black would leave nothing to compare. Plug-ins run on the CPU by design until they become
//!   nodes (M4 step 5). frei0r refuses a frame whose sides aren't multiples
//!   of 8, so it draws at 320x176. A machine without the plug-in fails the
//!   case, named: a GPU-like hard requirement for these tests, never a skip.
//! - The 3D LUT effect's file editor (#1746): a bundled look picked through
//!   it changes the frame, the CPU's within a level of the GPU's.

use super::parity_manifest::{CoverageKey, Interaction, Manifest, Report, Surface};
use super::parity_numeric::routes_for;
use super::parity_pixels::{analysis_drawn_on, clip_edit_drawn_on, Routes, SMALL};
use super::parity_specialists::{effect_type, measure, number_right_of, type_into, Analysis, ANALYSES, VARIANT};
use super::parity_harness::fixture;
use klipp_effects::BuiltinEffect;
use klipp_timeline::test_clip;

/// Why the frei0r editor's Pixels are drawn on the CPU alone.
const FREI0R_ROUTE: &str = "M4 step 5 (frei0r as a node, with a local CPU segment and an instance owner): plug-ins run on the CPU";

/// A frame frei0r takes: both sides multiples of 8.
const FREI0R_SIZE: (u32, u32) = (320, 176);

fn analysis_key(a: &Analysis) -> CoverageKey {
    CoverageKey::new(effect_type(a.tag).desc.id, &format!("@workflow/{}", a.workflow), Surface::EffectsStack, VARIANT, Interaction::Pixels)
}

/// One analysis workflow's measurement, drawn: the measurement it starts
/// by itself landing as the app's drain lands it, the file's analysis as
/// the app holds it, drawn from that and nothing else, must change the
/// frame and draw it alike on the CPU and the GPU.
fn analysis_case(a: &Analysis, routes: Routes) -> Result<(), String> {
    let clip = fixture(&(effect_type(a.tag).sample)());
    let mut run = super::parity_harness::Run::new(clip.clone());
    // The worker's result, as the drain applies it, and the analysis it
    // kept, as the app's next look finds it.
    if !super::proxies::apply_analysis_result(&mut run.state, &(a.complete)(clip.id)) {
        return Err("the result wasn't applied".into());
    }
    measure(&mut run.state, &clip, a);
    run.frame(Vec::new(), egui::Modifiers::NONE);
    if !run.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", run.unapplied));
    }
    if run.state.analysis_of(run.clip(), a.kind).is_none() {
        return Err("the measurement isn't held".into());
    }
    analysis_drawn_on(routes, SMALL, run.clip(), &run.state)
}

/// A real frei0r plug-in, registered beside the others (the registry is
/// the process's; registering adds, so no other case loses its own).
pub(super) fn saturator() -> Result<klipp_frei0r::Frei0rPluginInfo, String> {
    static FOUND: std::sync::OnceLock<Option<klipp_frei0r::Frei0rPluginInfo>> = std::sync::OnceLock::new();
    FOUND
        .get_or_init(|| {
            // A GPU context before loading a plug-in: one that links the
            // driver's OpenGL can deadlock a Vulkan start (#1032).
            let _ = klipp_effects::gpu_context::GpuContext::shared();
            let home = std::env::var_os("HOME").map(std::path::PathBuf::from).map(|h| h.join(".frei0r-1/lib"));
            let dirs = home.into_iter().chain(["/usr/local/lib/frei0r-1", "/usr/lib/frei0r-1"].map(std::path::PathBuf::from));
            let info = dirs.map(|d| d.join("saturat0r.so")).filter(|p| p.exists()).find_map(|p| klipp_frei0r::host::scan_library(&p).into_iter().next())?;
            klipp_effects::frei0r_effect::_register_for_test(info.clone());
            Some(info)
        })
        .clone()
        .ok_or_else(|| "the frei0r saturat0r plug-in isn't installed: install frei0r-plugins (an optdepend) to run the frei0r parity case".into())
}

/// The frei0r editor's value typed, stored as typed, drawn: it changes
/// the frame (on the CPU, the plug-ins' route by design).
fn frei0r_case() -> Result<(), String> {
    let info = saturator()?;
    let param = info.params.first().ok_or("saturat0r declares no parameter")?.name.clone();
    let params = info.params.iter().map(|p| klipp_frei0r::Frei0rValue::default_for(p.kind)).collect();
    let clip = fixture(&BuiltinEffect::Frei0r { type_id: info.type_id(), params });
    let mut run = super::parity_harness::Run::new(clip.clone());
    let at = number_right_of(&mut run, &param).ok_or_else(|| format!("no number for {param}"))?;
    type_into(&mut run, at, "0.85");
    if !run.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", run.unapplied));
    }
    match test_clip::effects(run.clip()).first() {
        Some(BuiltinEffect::Frei0r { params, .. }) if matches!(params.first(), Some(klipp_frei0r::Frei0rValue::Double(v)) if *v == 0.85) => {}
        other => return Err(format!("typed 0.85, stored {other:?}")),
    }
    clip_edit_drawn_on(Routes::CpuByDesign, FREI0R_SIZE, &clip, run.clip())
}

pub(super) fn run(report: &mut Report) {
    for a in ANALYSES {
        let name = format!("{} workflow / pixels", a.tag);
        let id = effect_type(a.tag).desc.id;
        match routes_for(id) {
            (Routes::CpuByDesign, Some(reason)) => report.case_cpu_by_design(&name, [analysis_key(a)], reason, || analysis_case(a, Routes::CpuByDesign)),
            _ => report.case_or_blocked(&name, [analysis_key(a)], || analysis_case(a, Routes::Both)),
        }
    }
    // The editor's one picture, earned on each entry it draws.
    let entries = super::parity_specialists::frei0r_entries();
    if entries.is_empty() {
        report.case("frei0r editor / pixels", [], || Err("no catalog entry draws the frei0r editor".into()));
    } else {
        let keys: Vec<_> = entries.iter().map(|e| CoverageKey::new(e.type_id, &e.parameter, Surface::EffectsStack, VARIANT, Interaction::Pixels)).collect();
        report.case_cpu_by_design("frei0r editor / pixels", keys, FREI0R_ROUTE, frei0r_case);
    }
    let keys: Vec<_> = super::parity_specialists::lut_file_entries().iter().map(|e| CoverageKey::new(e.type_id, &e.parameter, Surface::EffectsStack, VARIANT, Interaction::Pixels)).collect();
    report.case_or_blocked("3D LUT file editor / pixels", keys, lut_file_case);
}

/// A bundled look picked through the 3D LUT effect's file editor, drawn:
/// it changes the frame, the CPU's within a level of the GPU's.
fn lut_file_case() -> Result<(), String> {
    let (category, name, _) = super::parity_specialists::first_bundled_look().ok_or("no bundled look to pick")?;
    let clip = super::parity_specialists::lut_clip("");
    let mut run = super::parity_harness::Run::new(clip.clone());
    for text in ["Bundled", category.as_str(), name.as_str()] {
        if !run.click_last(text) {
            return Err(format!("{text:?} isn't drawn"));
        }
    }
    if !run.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", run.unapplied));
    }
    clip_edit_drawn_on(Routes::Both, SMALL, &clip, run.clip())
}

/// Every effect specialist's Pixels key is earned, on its declared route.
#[test]
fn effect_specialists_draw_their_edits_on_their_routes() {
    let mut manifest = Manifest::default();
    super::parity_specialists::require(&mut manifest);
    let mut report = Report::default();
    run(&mut report);
    for (key, why) in report.blocked.iter().chain(&report.cpu_by_design) {
        eprintln!("{key:?}: {why}");
    }
    report.family_result().unwrap();
    assert!(report.blocked.is_empty(), "a specialist left the native route: {:?}", report.blocked);
    let pixels: Vec<_> = manifest.required.iter().filter(|k| k.interaction == Interaction::Pixels).collect();
    let missing: Vec<_> = pixels.iter().filter(|k| !report.passed.contains(k)).collect();
    assert!(!pixels.is_empty() && missing.is_empty(), "Pixels required and not earned: {missing:#?}");
}

