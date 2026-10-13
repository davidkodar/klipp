//! The piece transitions (#985): strips, panels and halves that slide
//! away to reveal the next shot, or arrive over the last. What they look
//! like on the path that renders them (`render_transition_linear`, the
//! shader's body), and that the GPU draws the same over the settings.

use klipp_core::piece_transition::{PieceEase, PieceOrder, PieceParams};
use klipp_core::{spec_for_kind, TransitionKind, TransitionUserParams};
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::{linear_f32_rgba_to_srgb_u8, render_transition_linear};

const W: u32 = 96;
const H: u32 = 54;
const KINDS: [TransitionKind; 3] = [TransitionKind::SlideStrips, TransitionKind::SlidePanels, TransitionKind::PartHalves];

/// A picture that shows where each of its pixels came from: red across,
/// green down.
fn ramp() -> Vec<f32> {
    let mut v = Vec::new();
    for y in 0..H {
        for x in 0..W {
            v.extend_from_slice(&[(x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32, 0.0, 1.0]);
        }
    }
    v
}

/// A flat blue picture: the incoming shot.
fn blue() -> Vec<f32> {
    [0.0, 0.0, 1.0, 1.0].repeat((W * H) as usize)
}

fn render(kind: TransitionKind, u: TransitionUserParams, from: &[f32], to: &[f32], p: f32) -> Vec<f32> {
    render_transition_linear(from, to, spec_for_kind(kind).kind_id as u32, p, W, H, u.param0, u.param1, u.param2, u.param3, u.color)
}

fn px(img: &[f32], x: u32, y: u32) -> [f32; 4] {
    let i = ((y * W + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

/// Whether a pixel is the incoming (blue) picture.
fn is_blue(p: [f32; 4]) -> bool {
    p[2] > 0.99 && p[0] < 0.01 && p[1] < 0.01
}

/// The share of the frame showing the incoming picture.
fn blue_share(img: &[f32]) -> f32 {
    img.chunks_exact(4).filter(|p| is_blue([p[0], p[1], p[2], p[3]])).count() as f32 / (W * H) as f32
}

/// Every kind, leaving and arriving, at any angle and order: the outgoing
/// picture at the start and the incoming at the end, exactly; in between,
/// more of the incoming picture as it goes.
#[test]
fn each_piece_transition_runs_from_the_outgoing_picture_to_the_incoming() {
    let (a, b) = (ramp(), blue());
    for kind in KINDS {
        for assemble in [false, true] {
            for angle in [0.0, 33.0, 90.0, 215.0] {
                for order in [PieceOrder::InSequence, PieceOrder::Random, PieceOrder::CentreOut] {
                    let u = PieceParams { angle, order, assemble, gap: 6, ease: PieceEase::Smooth, ..PieceParams::read(kind, &spec_for_kind(kind).default_params) }.to_user(kind);
                    let what = format!("{kind:?} assemble {assemble} at {angle} degrees, {order:?}");
                    let start = render(kind, u, &a, &b, 0.0);
                    let end = render(kind, u, &a, &b, 1.0);
                    assert!(start.iter().zip(&a).all(|(x, y)| (x - y).abs() < 1e-4), "{what}: the outgoing picture at the start");
                    assert!(end.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4), "{what}: the incoming picture at the end");
                    let shares: Vec<f32> = [0.2, 0.5, 0.8].iter().map(|p| blue_share(&render(kind, u, &a, &b, *p))).collect();
                    // Strips at the corners are short: gone early, or
                    // arriving late.
                    assert!(shares[0] <= shares[1] && shares[1] <= shares[2] && shares[0] < shares[2] && shares[1] > 0.0 && shares[1] < 1.0, "{what}: the incoming picture grows {shares:?}");
                }
            }
        }
    }
}

/// The pieces move the picture, not a mask over it: half way, a strip
/// shows the outgoing picture shifted along its length, neighbours the
/// opposite ways; a panel shows its own picture moved right; a half
/// shows its picture moved off across the line.
#[test]
fn the_pieces_carry_their_picture_as_they_move() {
    let (a, b) = (ramp(), blue());
    // Two horizontal strips sliding sideways, together, linear: half way,
    // the top one has moved a half frame right, the bottom one left.
    let strips = PieceParams { count: 2, angle: 0.0, stagger: 0.0, order: PieceOrder::AllAtOnce, ease: PieceEase::Linear, alternate: true, ..Default::default() };
    let mid = render(TransitionKind::SlideStrips, strips.to_user(TransitionKind::SlideStrips), &a, &b, 0.5);
    let top = px(&mid, 70, 10);
    assert!((top[0] - px(&a, 70 - 48, 10)[0]).abs() < 0.02, "the top strip moved right: {top:?}");
    assert!(is_blue(px(&mid, 10, 10)), "uncovering the incoming picture behind it");
    let bottom = px(&mid, 20, 40);
    assert!((bottom[0] - px(&a, 20 + 48, 40)[0]).abs() < 0.02, "the bottom strip moved left: {bottom:?}");
    assert!(is_blue(px(&mid, 80, 40)));

    // Panels to the right, all at once, linear: each carries its own
    // picture; nothing of the outgoing picture is left at the far left.
    let panels = PieceParams { count: 2, angle: 0.0, stagger: 0.0, order: PieceOrder::AllAtOnce, ease: PieceEase::Linear, alternate: false, ..Default::default() };
    let mid = render(TransitionKind::SlidePanels, panels.to_user(TransitionKind::SlidePanels), &a, &b, 0.25);
    let moved = px(&mid, 60, 20);
    assert!((moved[0] - px(&a, 60 - 24, 20)[0]).abs() < 0.02 && (moved[1] - px(&a, 60, 20)[1]).abs() < 0.02, "moved right along its row: {moved:?}");
    assert!(is_blue(px(&mid, 5, 20)));

    // Halves parting across a vertical line, all at once, linear.
    let halves = PieceParams { angle: 90.0, stagger: 0.0, order: PieceOrder::AllAtOnce, ease: PieceEase::Linear, shear: false, ..Default::default() };
    let mid = render(TransitionKind::PartHalves, halves.to_user(TransitionKind::PartHalves), &a, &b, 0.5);
    assert!(is_blue(px(&mid, 47, 27)) && is_blue(px(&mid, 49, 27)), "the middle is open");
    let left = px(&mid, 5, 27);
    assert!((left[0] - px(&a, 5 + 24, 27)[0]).abs() < 0.03, "the left half moved left: {left:?}");

    // Arriving: half way, the incoming strips are half in, over the
    // outgoing picture.
    let arriving = PieceParams { assemble: true, ..strips };
    let mid = render(TransitionKind::SlideStrips, arriving.to_user(TransitionKind::SlideStrips), &a, &b, 0.5);
    assert!(is_blue(px(&mid, 70, 10)) && !is_blue(px(&mid, 10, 10)), "the top strip is coming back in from the right");
}

/// The order and stagger say which pieces go first: in sequence, the
/// panel nearest the way out leaves first; reversed, the farthest; all
/// at once, both.
#[test]
fn the_order_says_which_pieces_go_first() {
    let (a, b) = (ramp(), blue());
    let base = PieceParams { count: 1, angle: 0.0, stagger: 0.8, ease: PieceEase::Linear, alternate: false, ..Default::default() };
    // One row of two panels moving right.
    let share_at = |order, p: f32, x0: u32, x1: u32| {
        let img = render(TransitionKind::SlidePanels, PieceParams { order, ..base }.to_user(TransitionKind::SlidePanels), &a, &b, p);
        let mut n = 0;
        for y in 0..H {
            for x in x0..x1 {
                n += is_blue(px(&img, x, y)) as u32;
            }
        }
        n as f32 / ((x1 - x0) * H) as f32
    };
    // The right panel's key is 0 and the left's 0.5, so with a stagger of
    // 0.8 the left one waits until 0.4.
    assert!(share_at(PieceOrder::InSequence, 0.15, 48, 96) > 0.0 && share_at(PieceOrder::InSequence, 0.15, 0, 48) == 0.0, "the right panel goes first");
    assert!(share_at(PieceOrder::Reversed, 0.45, 0, 48) > 0.0 && share_at(PieceOrder::Reversed, 0.45, 48, 96) == 0.0, "reversed, the left one does");
    assert!(share_at(PieceOrder::AllAtOnce, 0.15, 0, 48) > 0.0 && share_at(PieceOrder::AllAtOnce, 0.15, 48, 96) > 0.0, "together, both");
}

/// The GPU draws what the CPU draws, over the settings: every kind,
/// angle, order, easing, leaving and arriving, alternating and with a gap.
#[test]
fn piece_transitions_match_between_cpu_and_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    // A textured outgoing picture, so a pixel taken from the wrong place
    // shows.
    let mut from = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let check = if (x / 5 + y / 5) % 2 == 0 { 0.8 } else { 0.15 };
            from.extend_from_slice(&[check * x as f32 / W as f32, 0.3 + 0.4 * y as f32 / H as f32, check, 1.0]);
        }
    }
    let to: Vec<f32> = (0..W * H).flat_map(|i| [0.1, (i % 7) as f32 / 7.0, 0.5, 1.0]).collect();
    let mut failures = Vec::new();
    for kind in KINDS {
        for (angle, order, ease, assemble, alternate, gap) in [
            (0.0, PieceOrder::InSequence, PieceEase::Smooth, false, true, 0),
            (27.0, PieceOrder::Random, PieceEase::Overshoot, true, false, 4),
            (90.0, PieceOrder::CentreOut, PieceEase::Accelerate, false, true, 10),
            (200.0, PieceOrder::EdgesIn, PieceEase::Linear, true, true, 0),
            (-45.0, PieceOrder::Reversed, PieceEase::Smooth, false, false, 3),
        ] {
            let u = PieceParams { count: 7, angle, stagger: 0.5, order, ease, assemble, alternate, gap, shear: angle < 0.0 }.to_user(kind);
            for p in [0.2f32, 0.5, 0.8] {
                let id = spec_for_kind(kind).kind_id as u32;
                let cpu = render_transition_linear(&from, &to, id, p, W, H, u.param0, u.param1, u.param2, u.param3, u.color);
                let g = gpu.render_transition_linear(&from, &to, id, p, W, H, u.param0, u.param1, u.param2, u.param3, u.color);
                let (c, g) = (linear_f32_rgba_to_srgb_u8(&cpu), linear_f32_rgba_to_srgb_u8(&g));
                // A pixel whose centre lies on a piece's edge may fall on
                // either side between the two: a few, no more.
                let over = c.iter().zip(&g).enumerate().filter(|(i, (x, y))| i % 4 != 3 && (**x as i32 - **y as i32).abs() > 1).count();
                if over > 6 {
                    failures.push(format!("{kind:?} at {angle} degrees, {order:?}, {ease:?}, assemble {assemble}, p {p}: {over} values over 1"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
