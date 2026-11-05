//! Before/After on a parked frame: the frame is made again at once, with its
//! ungraded half. It was only made by whatever came next, so the split had
//! nothing to show and both sides of it were graded.

mod common;

use klipp_core::AppCommand;

#[test]
fn the_split_has_its_ungraded_half_without_another_edit() {
    let Some(src) = common::source("before-after", 320, 180, "") else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start_with(src, 320, 180, |_| {});
    parked.settle();
    assert!(original(&parked).is_empty(), "no ungraded half before it's asked for");
    parked.send(AppCommand::ToggleBeforeAfter);
    parked.settle();
    assert!(!original(&parked).is_empty(), "the ungraded half is there");
}

fn original(parked: &common::Parked) -> Vec<u8> {
    parked.frame_buffer().lock().unwrap().original_data.clone()
}
