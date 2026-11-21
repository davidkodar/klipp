//! The Veylvik tab's footage of a clip is its media's frame at the time
//! the editor asks for, decoded by Klipp from the file: any time, not only
//! the one a render of Klipp's shows.

use std::sync::Arc;

use klipp_core::WorkingColorspace;
use klipp_veylvik::editor_footage::{EditorFootage, TabHost};
use klipp_veylvik::{ClipSource, Composition, Renderer, Time};

#[test]
fn the_tabs_footage_is_the_clips_frame_at_the_time_asked() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::shared() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    let dir = std::env::temp_dir().join(format!("klipp_veylvik_tab_footage_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("red-then-blue.mp4");
    // A second of red, then a second of blue, at 24 fps.
    let made = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i", "color=c=red:s=32x18:r=24:d=1", "-f", "lavfi", "-i", "color=c=blue:s=32x18:r=24:d=1"])
        .args(["-filter_complex", "[0][1]concat=n=2:v=1[v]", "-map", "[v]", "-pix_fmt", "yuv444p", "-c:v", "libx264", "-qp", "0"])
        .arg(&file)
        .status()
        .expect("ffmpeg");
    assert!(made.success());
    let device = ctx.device();
    let renderer = Arc::new(Renderer::from_device((*device).clone(), (*ctx.queue()).clone(), ctx.adapter_info().unwrap().clone()).unwrap());
    let host = Arc::new(TabHost::default());
    let mut item = klipp_media::MediaItem::synthetic_for_test(file.clone(), None);
    item.resolution = klipp_core::Resolution { width: 32, height: 18 };
    item.duration = klipp_core::RationalTime { value: 48, rate: 24 };
    host.set(Some(EditorFootage::new(&item, WorkingColorspace::Rec709Linear, 1, Arc::clone(&device), ctx.queue())));
    let c = Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [32, 18], Time { ticks: 24, rate: 1 }, 48).unwrap();
    let source = ClipSource::new(Arc::clone(&renderer), c, dir.clone(), Some(host as Arc<dyn klipp_veylvik::Host>)).unwrap();
    let out = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 32 * 18 * 16, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let pixels = klipp_veylvik::working_pixels(WorkingColorspace::Rec709Linear);
    let read = |frame: i64| {
        source.render(Time { ticks: frame, rate: 24 }, [32, 18], &out, &pixels).expect("rendered");
        let staging = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 32 * 18 * 16, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut e = device.create_command_encoder(&Default::default());
        e.copy_buffer_to_buffer(&out, 0, &staging, 0, 32 * 18 * 16);
        ctx.queue().submit([e.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
        klipp_effects::gpu::poll_until_mapped(&device, &rx).unwrap().unwrap();
        let px: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
        let at = (9 * 32 + 16) * 4;
        [px[at], px[at + 1], px[at + 2]]
    };
    for (frame, red) in [(6, true), (30, false), (3, true), (40, false)] {
        let [r, _, b] = read(frame);
        assert!(if red { r > 0.8 && b < 0.1 } else { b > 0.8 && r < 0.1 }, "frame {frame}: {:?}", read(frame));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
