//! The workspace parses JSON floats exactly (serde_json's
//! `float_roundtrip`): a value printed and read back is the same value, so
//! a file can be written, read and written again without its numbers
//! drifting by a unit in the last place each time.

/// A float the default parser reads back one unit off.
#[test]
fn a_printed_float_reads_back_as_itself() {
    for x in [0.20399999618530273f64, 0.1 + 0.2, 1e-7, 123_456.789_012_345, f64::from(0.204f32), f64::from(1.0f32 / 3.0)] {
        let v = serde_json::json!({ "x": x });
        let text = serde_json::to_string(&v).unwrap();
        let back: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, v, "{x} came back as {}", back["x"]);
        assert_eq!(serde_json::to_string(&back).unwrap(), text, "and prints the same");
    }
}

/// How long a project takes to load, for the record when the JSON parser
/// changes (`KLIPP_LOAD_BENCH` names a project folder: a copy, never the
/// real one).
#[test]
#[ignore = "a timing, run by hand with KLIPP_LOAD_BENCH"]
fn load_time() {
    let Some(dir) = std::env::var_os("KLIPP_LOAD_BENCH") else { return };
    let dir = std::path::PathBuf::from(dir);
    let mut times: Vec<f64> = (0..30)
        .map(|_| {
            let t = std::time::Instant::now();
            klipp_project::load_project(&dir).expect("the project loads");
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    eprintln!("load: median {:.2} ms, min {:.2} ms over {} loads", times[times.len() / 2], times[0], times.len());
}
