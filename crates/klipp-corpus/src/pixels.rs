//! Render generated saved projects through the shared checkpoint routes.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use klipp_analysis::assets::CacheStore;
use klipp_render::pixel_check::{render_sequence, settings, Options, Rendered};
use klipp_render::pre_analysis::{ensure, needs, Analyser};
use klipp_render::SequenceSnapshot;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    frames: usize,
    flat: usize,
    grade_on_gpu: bool,
    stack: usize,
    stack_declined: usize,
    export_gpu: usize,
    export_cpu: usize,
    export_letterboxed: usize,
    gpu_cpu_kernels: u32,
    plan_clips: u32,
    plan_fallbacks: u32,
    declines: Vec<String>,
}

impl From<Rendered> for Coverage {
    fn from(mut r: Rendered) -> Self {
        r.declines.sort();
        Self {
            frames: r.frames,
            flat: r.flat,
            grade_on_gpu: r.grade_on_gpu,
            stack: r.stack,
            stack_declined: r.stack_declined,
            export_gpu: r.export_gpu,
            export_cpu: r.export_cpu,
            export_letterboxed: r.export_letterboxed,
            gpu_cpu_kernels: r.gpu_cpu_kernels,
            plan_clips: r.plan_clips,
            plan_fallbacks: r.plan_fallbacks,
            declines: r.declines,
        }
    }
}

/// A separate process supplies a private working directory for relative native
/// fixture paths. No process-global cwd or plug-in search is changed in tests.
pub fn render(root: &Path) -> Result<(), String> {
    let status = std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
        .arg("render-worker")
        .arg(root)
        .current_dir(root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .status()
        .map_err(|e| format!("corpus render worker: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("corpus render worker exited {status}"))
    }
}

pub fn worker(root: &Path) -> Result<(), String> {
    if !root.join(".klipp-scratch").is_file()
        || std::env::current_dir().map_err(|e| e.to_string())? != root
    {
        return Err("render worker needs a generated corpus root as its working directory".into());
    }
    let out = root.join("pixels");
    std::fs::create_dir(&out).map_err(|e| e.to_string())?;
    // Register only our compiled fixture, using its project-stable relative
    // path. Merely placing a library on disk does not populate the registry.
    let plugins = klipp_frei0r::host::scan_library(Path::new("plugins/colour.so"));
    if plugins.len() != 1 || plugins[0].type_id() != "frei0r:plugins/colour.so:klipp-fixture-colour"
    {
        return Err("frei0r fixture did not register its expected identity".into());
    }
    klipp_effects::frei0r_effect::set_plugin_registry(plugins);
    let mut coverage = BTreeMap::new();
    let mut failures = Vec::new();
    for case in crate::cases::CASES {
        let result = (|| -> Result<(), String> {
            let project = root.join("projects").join(format!("{case}.klipp"));
            let (sequences, pool, colour, ..) = klipp_project::load_project_strict(&project)?;
            if pool.all().any(|m| m.offline || !m.path.is_file()) {
                return Err(format!("{case}: offline media"));
            }
            for (index, sequence) in sequences.iter().enumerate() {
                let key = format!("{case}/{index}");
                let mut snapshot = SequenceSnapshot::from_sequence_with_all(
                    sequence,
                    &sequences,
                    &pool,
                    &settings(),
                    colour
                        .global_lut_path
                        .clone()
                        .filter(|_| colour.global_lut_enabled),
                    false,
                    colour.working_colorspace,
                )
                .map_err(|e| format!("{key}: snapshot: {e}"))?;
                snapshot.grade_encoding = colour.grade_encoding;
                snapshot.video.global_lut_opacity = colour.global_lut_opacity;
                let analysis_root = root.join("analysis");
                let mut store = CacheStore::at(analysis_root.clone());
                let analyse: Analyser = Arc::new(klipp_engine::analyse::analyse_media);
                ensure(
                    &needs(&snapshot),
                    &mut store,
                    Some(&analyse),
                    &AtomicBool::new(false),
                    &|_| {},
                )
                .map_err(|e| format!("{key}: {e}"))?;
                let factory = |snapshot: &SequenceSnapshot, gpu| {
                    let mut host = klipp_plugin::OfxHost::new();
                    host.discover_plugins_in(&[root.join("plugins/ofx")]);
                    let mut renderer = klipp_render::export_frames::export_renderer_with_host(
                        snapshot, "corpus", host,
                    );
                    renderer.set_gpu_enabled(gpu);
                    renderer.set_asset_store(Box::new(CacheStore::at(analysis_root.clone())));
                    Ok(renderer)
                };
                // Keep the temporal media edge and a cross-second sample;
                // other families use one active interior frame to fit 2 MiB.
                let temporal = *case == crate::cases::Case::Temporal;
                let witness_frame = if temporal { 24 } else { 12 };
                let rendered = render_sequence(
                    &snapshot,
                    &out.join(&key),
                    &key,
                    &out.join("probe.mp4"),
                    &Options {
                        width: 256,
                        frames: Some(if temporal { vec![0, 24] } else { vec![12] }),
                    },
                    &factory,
                )
                .map_err(|e| format!("{key}: {e}"))?;
                if !rendered.covered()
                    || rendered.flat != 0
                    || rendered.plan_fallbacks != 0
                    || rendered.declines.iter().any(|line| {
                        // Denoise cannot read a neighbour before the media
                        // starts. Only this exact edge fallback is expected.
                        !(temporal
                            && ["cpu", "gpu"].iter().any(|route| {
                                line.starts_with(&format!("temporal/0/0-{route} clip "))
                            })
                            && line.ends_with(": Temporal Noise Reduction: no frame past the media's edge (fallback)"))
                    })
                {
                    return Err(format!(
                        "{key}: invalid picture coverage ({} flat frames, {} legacy fallbacks, {} declines)",
                        rendered.flat,
                        rendered.plan_fallbacks,
                        rendered.declines.len()
                    ));
                }
                if *case != crate::cases::Case::Edit {
                    let mut control = snapshot.clone();
                    case.bypass(&mut control.video.sequence)?;
                    let witness = root.join("witness").join(&key);
                    render_sequence(
                        &control,
                        &witness,
                        &key,
                        &out.join("probe.mp4"),
                        &Options {
                            width: 256,
                            frames: Some(vec![witness_frame]),
                        },
                        &factory,
                    )
                    .map_err(|e| format!("{key}: bypass control: {e}"))?;
                    let (changed, _) = klipp_render::pixel_check::differ(
                        &out.join(&key).join(format!("{witness_frame}-cpu.png")),
                        &witness.join(format!("{witness_frame}-cpu.png")),
                    )?;
                    if changed <= 1 {
                        return Err(format!(
                            "{key}: the subject does not change its input beyond one level"
                        ));
                    }
                    println!("Witness {key}: subject changes its input ({changed} levels)");
                }
                coverage.insert(key.clone(), Coverage::from(rendered));
                println!("Rendered {key}");
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("FAIL {case}: {error}");
            failures.push(format!("{case}: {error}"));
        }
    }
    if !failures.is_empty() {
        return Err(failures.join("\n"));
    }
    std::fs::write(
        out.join("coverage.json"),
        serde_json::to_vec_pretty(&coverage).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
