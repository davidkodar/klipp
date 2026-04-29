//! Quick probe — load an ONNX file, print its inputs + outputs.
//! Run with: `ORT_DYLIB_PATH=... cargo run --features ort \
//!     -p klipp-ml-ort --example probe_outputs <path-to-onnx>`

#[cfg(feature = "ort")]
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: probe_outputs <path-to-onnx>");
    let session = ort::session::Session::builder()
        .expect("session builder")
        .commit_from_file(&path)
        .expect("load model");

    println!("INPUTS:");
    for (i, input) in session.inputs().iter().enumerate() {
        println!("  [{i}] {:?}", input);
    }

    println!("OUTPUTS:");
    for (i, output) in session.outputs().iter().enumerate() {
        println!("  [{i}] {:?}", output);
    }
}

#[cfg(not(feature = "ort"))]
fn main() {
    eprintln!("rebuild with --features ort");
}
