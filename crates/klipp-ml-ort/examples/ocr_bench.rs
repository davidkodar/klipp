//! Where the OCR's CPU time goes: the recogniser on synthetic strips
//! across thread counts, batch sizes and widths; the detector at two
//! sizes. Run with:
//! `cargo run --release --features ort -p klipp-ml-ort --example ocr_bench -- <det.onnx> <rec.onnx>`

#[cfg(feature = "ort")]
fn main() {
    use ort::session::builder::GraphOptimizationLevel;
    let mut args = std::env::args().skip(1);
    let det_path = args.next().expect("det model path");
    let rec_path = args.next().expect("rec model path");
    klipp_ml_ort::runtime_resolve::ensure_runtime_loadable().expect("runtime");

    let open = |path: &str, threads: usize| {
        let mut b = ort::session::Session::builder().unwrap().with_optimization_level(GraphOptimizationLevel::Level3).unwrap();
        if threads > 0 {
            b = b.with_intra_threads(threads).unwrap();
        }
        b.commit_from_file(path).unwrap()
    };
    let bench = |session: &mut ort::session::Session, batch: usize, width: usize, reps: usize| -> f64 {
        let m = 48 * width;
        let data: Vec<f32> = (0..batch * 3 * m).map(|i| ((i % 97) as f32 / 97.0) - 0.5).collect();
        // warm up
        let input = ort::value::Value::from_array(([batch as i64, 3, 48, width as i64], data.clone())).unwrap();
        let _ = session.run(ort::inputs![input]).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..reps {
            let input = ort::value::Value::from_array(([batch as i64, 3, 48, width as i64], data.clone())).unwrap();
            let _ = session.run(ort::inputs![input]).unwrap();
        }
        t.elapsed().as_secs_f64() * 1000.0 / (reps * batch) as f64
    };

    println!("recogniser: ms per line (lower is better)");
    println!("{:>8} {:>6} {:>6} | {:>8}", "threads", "batch", "width", "ms/line");
    for threads in [0usize, 1, 2, 4, 8, 16] {
        let mut s = open(&rec_path, threads);
        for (batch, width) in [(1usize, 320usize), (1, 640), (16, 320), (16, 640)] {
            let reps = if batch == 1 { 40 } else { 4 };
            let ms = bench(&mut s, batch, width, reps);
            println!("{:>8} {:>6} {:>6} | {:>8.2}", if threads == 0 { "default".to_string() } else { threads.to_string() }, batch, width, ms);
        }
    }

    println!("\ndetector: ms per frame");
    for threads in [0usize, 4, 8, 16] {
        let mut s = open(&det_path, threads);
        for (w, h) in [(960usize, 544usize), (1600, 896)] {
            let n = w * h;
            let data: Vec<f32> = (0..3 * n).map(|i| ((i % 89) as f32 / 89.0) - 0.5).collect();
            let input = ort::value::Value::from_array(([1i64, 3, h as i64, w as i64], data.clone())).unwrap();
            let _ = s.run(ort::inputs![input]).unwrap();
            let t = std::time::Instant::now();
            for _ in 0..5 {
                let input = ort::value::Value::from_array(([1i64, 3, h as i64, w as i64], data.clone())).unwrap();
                let _ = s.run(ort::inputs![input]).unwrap();
            }
            println!("{:>8} {}x{} | {:>8.1} ms", if threads == 0 { "default".to_string() } else { threads.to_string() }, w, h, t.elapsed().as_secs_f64() * 1000.0 / 5.0);
        }
    }
}

#[cfg(not(feature = "ort"))]
fn main() {
    eprintln!("rebuild with --features ort");
}
