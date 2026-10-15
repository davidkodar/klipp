use klipp_title::{AlphaMaskProcessor, TitleRenderer};
struct Gpu(klipp_effects::gpu::GpuEffectsPipeline);
impl AlphaMaskProcessor for Gpu {
    fn blur(&mut self, m: &[u8], w: u32, h: u32, r: f32) -> Vec<u8> {
        self.0.blur_alpha_gpu(m, w, h, r)
    }
    fn dilate(&mut self, m: &[u8], w: u32, h: u32, r: f32) -> Vec<u8> {
        self.0.dilate_alpha_gpu(m, w, h, r)
    }
}
#[test]
fn revealed_stroke_halo_matches_cpu_and_gpu() {
    let Some(g) = klipp_effects::gpu::GpuEffectsPipeline::new() else {
        return;
    };
    let mut gpu = Gpu(g);
    let mut path = klipp_timeline::VectorPath::default();
    path.pose.fill = [0.0; 4];
    path.pose.stroke_width = 20.0;
    path.trim = Some(klipp_timeline::PathTrim {
        range: klipp_timeline::TrimRange {
            end: 0.6,
            ..Default::default()
        },
        ..Default::default()
    });
    for radius in [6.0, 42.0, 120.0] {
        path.pose.stroke_glow = Some(klipp_timeline::StrokeGlow {
            radius,
            strength: 0.8,
        });
        let mut title = klipp_timeline::TitleData::default();
        title.objects.clear();
        title.add_object(
            klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape {
                kind: klipp_timeline::ShapeKind::Path(path.clone()),
                fill: None,
                gradient: None,
                stroke: None,
                fit: None,
            }),
            klipp_timeline::ObjectTransform {
                size: klipp_timeline::TitlePosition { x: 0.8, y: 0.8 },
                ..klipp_timeline::default_text_transform()
            },
        );
        let cpu = TitleRenderer::new().render_with_time(&title, 320, 180, None, 0.0);
        let device = TitleRenderer::new().render_with_time(&title, 320, 180, Some(&mut gpu), 0.0);
        let delta = cpu
            .iter()
            .zip(&device)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(delta <= 1, "radius {radius}: {delta}");
    }
}

#[path = "../../klipp-title/tests/common/vector_case.rs"]
mod vector_case;

fn vector_sequence(w:u32,h:u32) -> klipp_timeline::Sequence {
    use klipp_timeline::{Clip,ClipKind,Track,TrackKind};
    let mut seq=klipp_timeline::Sequence::new_default();
    seq.resolution=klipp_core::Resolution{width:w,height:h};
    seq.fps=klipp_core::Fps::new(30,1);
    let tc=|s|klipp_core::RationalTime::from_secs_f64(s,seq.fps.rate());
    let mut title=Track::new("Vector",TrackKind::Text);
    title.clips.push(Clip{kind:ClipKind::Title(vector_case::ring()),source_in:tc(0.0),source_out:tc(7.0),..Clip::gap(tc(0.0),tc(7.0))});
    let mut bg=Track::new("Background",TrackKind::Video);
    bg.clips.push(Clip{kind:ClipKind::Generator(klipp_core::GeneratorData::LivingGradient{
        a:[12,23,32,255],b:[29,60,65,255],angle_deg:25.0,drift_deg_per_sec:1.5,glow:0.18,
    }),source_in:tc(0.0),source_out:tc(7.0),..Clip::gap(tc(0.0),tc(7.0))});
    seq.tracks=vec![title,bg];seq
}
struct NoMedia;
impl klipp_compose::FrameSource for NoMedia {
    fn frame(&mut self,_:&klipp_timeline::Clip,_:f64,_:u32,_:u32)->Option<klipp_compose::SourceFrame>{None}
}
fn renderer(gpu:bool)->klipp_compose::Renderer{
    let mut r=klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(),TitleRenderer::new());
    r.set_gpu_enabled(gpu);r
}
#[test]
fn animated_and_completed_vector_reveals_match_export_after_reload(){
    let seq=vector_sequence(320,180);
    let seq:klipp_timeline::Sequence=serde_json::from_str(&serde_json::to_string(&seq).unwrap()).unwrap();
    let mut gpu=renderer(true);let mut cpu=renderer(false);
    for t in [0.3,1.2,2.0,4.0,6.0,0.3]{
        let at=klipp_core::RationalTime::from_secs_f64(t,seq.fps.rate());
        let (linear,_)=cpu.render_inspected(&seq,at,320,180,&mut NoMedia,Default::default(),Default::default());
        let expected=klipp_effects::linear_f32_rgba_to_srgb_u8(&linear);
        let Some((actual,_))=gpu.render_display_u8_on_gpu(&seq,at,320,180,&mut NoMedia,Default::default()) else {
            assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(),Ok("1"),"GPU required");return;
        };
        let worst=actual.iter().zip(expected).map(|(a,b)|a.abs_diff(b)).max().unwrap();
        assert!(worst<=1,"time {t}: {worst} levels");
    }
}
#[test]
#[ignore="manual 4K compositor performance measurement"]
fn measure_4k_vector_composite(){
    let seq=vector_sequence(3840,2160);let mut r=renderer(true);
    for (label,start) in [("active",0.1),("held",3.0)]{
        let mut times=Vec::new();
        for n in 0..31 {
            let at=klipp_core::RationalTime::from_secs_f64(start+n as f64/30.0,seq.fps.rate());
            let begin=std::time::Instant::now();
            r.render_display_u8_on_gpu(&seq,at,3840,2160,&mut NoMedia,Default::default()).expect("GPU stack");
            if n>0{times.push(begin.elapsed().as_secs_f64()*1000.0);}
        }
        times.sort_by(f64::total_cmp);
        println!("4K {label} vector + gradient, display readback included: median {:.2} ms p95 {:.2} ms",times[15],times[28]);
    }
}
