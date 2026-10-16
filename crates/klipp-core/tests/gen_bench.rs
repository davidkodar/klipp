//! What a generator costs the CPU, for the two paths that exist.
//!
//! Printed rather than asserted on a threshold — a timing assertion is
//! a flake waiting to happen — but the shape is the point: the half
//! float path must not cost materially more than the byte path, since
//! both now leave the sRGB decode to the GPU.
#[test]
fn what_a_generator_costs() {
    use klipp_core::generator::*;
    let (w, h) = (1920u32, 1080u32);
    let (a, b) = ([4u8, 3, 12, 255], [18u8, 12, 48, 255]);
    let time = |mut f: Box<dyn FnMut(f64)>| {
        f(0.0);
        let t = std::time::Instant::now();
        for i in 0..10 { f(i as f64); }
        t.elapsed().as_secs_f64() * 100.0
    };
    let u8_ms = time(Box::new(move |s| { let _ = render_living_gradient(a, b, 25.0, 1.2, 0.12, w, h, s); }));
    let f16_ms = time(Box::new(move |s| { let _ = render_living_gradient_f16(a, b, 25.0, 1.2, 0.12, klipp_core::DEFAULT_GENERATOR_GRAIN, w, h, s); }));
    println!("1080p living gradient: u8 {u8_ms:.1} ms/frame, sRGB f16 {f16_ms:.1} ms/frame");
}
