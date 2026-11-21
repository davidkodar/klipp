//! What a frame of the Veylvik tab's playback costs, part by part, on a
//! real composition and real footage: Klipp decoding the clip's frame,
//! sending it up, and Veylvik rendering the composition. Run by hand:
//! KLIPP_TAB_COST=<project folder> cargo test -p klipp-veylvik --release
//! --test tab_playback_cost -- --ignored --nocapture

use std::sync::Arc;
use std::time::Instant;

use klipp_core::WorkingColorspace;
use klipp_veylvik::editor_footage::{EditorFootage, TabHost};
use klipp_veylvik::{ClipSource, Composition, HostMedia, Renderer, Time};

fn summary(name: &str, ms: &mut [f64]) {
    ms.sort_by(f64::total_cmp);
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;
    eprintln!("{name:<34} mean {mean:7.1} ms   median {:7.1}   p90 {:7.1}   max {:7.1}", ms[ms.len() / 2], ms[ms.len() * 9 / 10], ms[ms.len() - 1]);
}

#[test]
#[ignore = "a measurement, run by hand"]
fn what_a_frame_of_the_tabs_playback_costs() {
    let Some(project) = std::env::var_os("KLIPP_TAB_COST").map(std::path::PathBuf::from) else { return eprintln!("set KLIPP_TAB_COST") };
    let ctx = klipp_effects::gpu_context::GpuContext::shared().expect("a GPU");
    let folder = std::fs::read_dir(project.join("media/veylvik")).unwrap().next().unwrap().unwrap().path();
    let composition: Composition = serde_json::from_slice(&std::fs::read(folder.join("composition.veylvik")).unwrap()).unwrap();
    let footage = project.parent().unwrap().join("footage/Handheld.mp4");
    let (w, h) = (composition.width, composition.height);
    eprintln!("composition {w}x{h}, footage {}", footage.display());
    let fps = 24000.0 / 1001.0;
    let device = ctx.device();
    eprintln!("limits: max_buffer {} MB, 4K float frame {} MB", device.limits().max_buffer_size >> 20, (3840u64 * 2160 * 16) >> 20);
    let mut renderer = Renderer::from_device((*device).clone(), (*ctx.queue()).clone(), ctx.adapter_info().unwrap().clone()).unwrap();
    veylvik_io::runtime::RenderLimits::from_env_with_default(None).unwrap().apply(&mut renderer);
    let renderer = Arc::new(renderer);
    let frames = 96;

    // Klipp's part alone: decode + send up, frame after frame as playback asks.
    let mut item = klipp_media::MediaItem::synthetic_for_test(footage.clone(), None);
    item.resolution = klipp_core::Resolution { width: 3840, height: 2160 };
    item.fps = klipp_core::Fps::new(24000, 1001);
    item.duration = klipp_core::RationalTime::from_secs_f64(13.0, 24000);
    let media = EditorFootage::new(&item, WorkingColorspace::Rec709Linear, 7, Arc::clone(&device));
    let mut decode = Vec::new();
    for i in 0..frames {
        let t = Instant::now();
        let f = media.frame(Time { ticks: i * 1001, rate: 24000 }, None).unwrap();
        decode.push(t.elapsed().as_secs_f64() * 1000.0);
        drop(f);
        // Let the card free what was dropped, as the app's frames do.
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    }
    summary("Klipp: decode + send up (sequential)", &mut decode);

    // The whole frame as the tab plays it: Veylvik asking Klipp's footage.
    let host = Arc::new(TabHost::default());
    host.set(Some(EditorFootage::new(&item, WorkingColorspace::Rec709Linear, 8, Arc::clone(&device))));
    let source = ClipSource::new(Arc::clone(&renderer), composition, folder.clone(), Some(host as Arc<dyn klipp_veylvik::Host>)).unwrap();
    let t = Instant::now();
    source.compile().unwrap();
    eprintln!("compile: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    let out = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: u64::from(w) * u64::from(h) * 16, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let pixels = klipp_veylvik::working_pixels(WorkingColorspace::Rec709Linear);
    let mut whole = Vec::new();
    for i in 0..frames {
        let t = Instant::now();
        source.render(Time { ticks: (100 + i) * 1001, rate: 24000 }, [w, h], &out, &pixels).unwrap();
        // Waited for on the card: what the frame really took.
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        whole.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    summary("whole frame (decode + Veylvik)", &mut whole);
    eprintln!("budget at 23.976 fps: {:.1} ms", 1000.0 / fps);
}
