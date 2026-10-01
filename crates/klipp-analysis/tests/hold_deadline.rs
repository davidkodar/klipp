//! A child that does not finish in time is killed and said to be cut
//! short (#519): ffmpeg's hardware decode can wait forever on a seek.
//! Its own test binary, since the editor-busy hook is process-wide.

use std::time::{Duration, Instant};

#[test]
fn a_child_past_its_deadline_is_killed() {
    let t = Instant::now();
    let cut = klipp_analysis::hold::output_within(std::process::Command::new("sleep").arg("30"), Some(Duration::from_millis(300))).unwrap();
    assert!(cut.is_none(), "cut short");
    assert!(t.elapsed() < Duration::from_secs(5), "not waited out: {:?}", t.elapsed());
    let done = klipp_analysis::hold::output_within(std::process::Command::new("echo").arg("hi"), Some(Duration::from_secs(10))).unwrap().expect("in time");
    assert_eq!(String::from_utf8_lossy(&done.stdout).trim(), "hi");
}
