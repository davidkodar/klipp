//! Where a title sits on the frame, estimated from its objects: what
//! the checks place titles clear of, and what the renderer keeps
//! captions clear of (#2567).

/// The box a title covers, as fractions of the frame: its text boxes
/// sized from their type and words, its shapes and images from their
/// transforms. `None` for a title with nothing visible.
pub fn title_box(td: &crate::TitleData, frame: klipp_core::Resolution) -> Option<[f32; 4]> {
    use crate::TitleObjectContent;
    let mut acc: Option<[f32; 4]> = None;
    for o in td.objects.iter().filter(|o| o.visible) {
        let (cx, cy) = (o.transform.center.x, o.transform.center.y);
        let (w, h) = match &o.content {
            TitleObjectContent::TextBox(tb) => text_extent(&tb.text, tb.font_size, frame),
            _ => (o.transform.size.x, o.transform.size.y),
        };
        let b = [cx - w / 2.0, cy - h / 2.0, w, h];
        acc = Some(match acc {
            None => b,
            Some(a) => {
                let (x0, y0) = (a[0].min(b[0]), a[1].min(b[1]));
                let (x1, y1) = ((a[0] + a[2]).max(b[0] + b[2]), (a[1] + a[3]).max(b[1] + b[3]));
                [x0, y0, x1 - x0, y1 - y0]
            }
        });
    }
    acc
}

/// About how much of a `frame` text of `size` covers: the longest line at
/// a bit over half an em a character, the lines at 1.3 em. `size` is
/// pixels at 1080 on the frame's short side, as the renderer draws it
/// (`klipp_title::frame_scale`), so a vertical frame's estimate is its own
/// and not a landscape one's.
pub fn text_extent(text: &str, size: f32, frame: klipp_core::Resolution) -> (f32, f32) {
    let longest = text.lines().map(|l| l.chars().count()).max().unwrap_or(0).max(1) as f32;
    let lines = text.lines().count().max(1) as f32;
    let (w, h) = (frame.width.max(1) as f32, frame.height.max(1) as f32);
    let px = size * w.min(h) / 1080.0;
    // Never wider than the renderer's text box, 0.9 of the frame: it wraps
    // inside that.
    ((longest * px * 0.55 / w).min(0.9), (lines * px * 1.3 / h).min(0.9))
}
