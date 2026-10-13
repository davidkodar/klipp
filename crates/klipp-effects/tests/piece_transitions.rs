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
            let u = PieceParams { count: 7, angle, stagger: 0.5, order, ease, assemble, alternate, gap, shear: angle < 0.0, ..Default::default() }.to_user(kind);
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

// ── Posed pieces (#985, phase 2) ──

use klipp_core::piece_transition::PieceMotion;
use klipp_effects::pieces;

const POSED: [PieceMotion; 4] = [PieceMotion::Tilt, PieceMotion::Turn, PieceMotion::Fall, PieceMotion::Shrink];

fn close(a: &[f32], b: &[f32], tol: f32) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < tol)
}

/// Every kind with every posed motion, and the shards sliding, leaving
/// and arriving, with a gap and shadows: the outgoing picture at the
/// start and the incoming at the end; in between both show.
#[test]
fn posed_pieces_run_from_the_outgoing_picture_to_the_incoming() {
    let (a, b) = (ramp(), blue());
    let mut cases: Vec<(TransitionKind, PieceMotion)> = KINDS.iter().flat_map(|k| POSED.map(|m| (*k, m))).collect();
    cases.extend([TransitionKind::Shatter].iter().flat_map(|k| [PieceMotion::Slide, PieceMotion::Fall, PieceMotion::Tilt].map(|m| (*k, m))));
    for (kind, motion) in cases {
        for assemble in [false, true] {
            let u = PieceParams { motion, assemble, gap: 4, shadow: true, angle: 30.0, ..PieceParams::read(kind, &spec_for_kind(kind).default_params) }.to_user(kind);
            assert!(pieces::is_posed(spec_for_kind(kind).kind_id as u32, u.param3), "{kind:?} {motion:?}");
            let what = format!("{kind:?} {motion:?} assemble {assemble}");
            assert!(close(&render(kind, u, &a, &b, 0.0), &a, 2e-3), "{what}: the outgoing picture at the start");
            assert!(close(&render(kind, u, &a, &b, 1.0), &b, 2e-3), "{what}: the incoming picture at the end");
            // Half way, neither picture whole: pieces (lit by how they
            // face) over what shows behind them.
            let mid = render(kind, u, &a, &b, 0.5);
            let unlike = |pic: &[f32]| mid.chunks_exact(4).zip(pic.chunks_exact(4)).filter(|(m, p)| m.iter().zip(*p).any(|(x, y)| (x - y).abs() > 1e-3)).count();
            let n = (W * H) as usize;
            let (not_a, not_b) = (unlike(&a), unlike(&b));
            assert!(not_a > 0 && not_b > 0 && (not_a < n || not_b < n), "{what}: half way, {not_a} pixels are not the outgoing picture and {not_b} not the incoming");
        }
    }
}

/// A shrinking panel keeps its picture at its middle while its corners
/// open onto the incoming picture; shadows darken only what is behind,
/// near a moving piece; tilted away, a piece is darker than its picture.
#[test]
fn posed_pieces_carry_their_picture_light_and_shadow() {
    let (a, b) = (ramp(), blue());
    // One row: two panels, shrinking together, linear.
    let shrink = PieceParams { count: 1, stagger: 0.0, order: PieceOrder::AllAtOnce, ease: PieceEase::Linear, alternate: false, motion: PieceMotion::Shrink, ..Default::default() };
    let mid = render(TransitionKind::SlidePanels, shrink.to_user(TransitionKind::SlidePanels), &a, &b, 0.5);
    let centre = px(&mid, 24, 27);
    assert!((centre[0] - px(&a, 24, 27)[0]).abs() < 0.02 && (centre[1] - px(&a, 24, 27)[1]).abs() < 0.02, "the middle keeps its picture: {centre:?}");
    assert!(is_blue(px(&mid, 2, 2)) && is_blue(px(&mid, 93, 51)), "the corners are open");

    // Shadows: on, part of what is behind is darkened well below the
    // incoming picture (a piece's soft corner over it only ever takes a
    // little); off, none is.
    let shaded = |shadow: bool| {
        let u = PieceParams { shadow, ..shrink }.to_user(TransitionKind::SlidePanels);
        render(TransitionKind::SlidePanels, u, &a, &b, 0.5).chunks_exact(4).filter(|p| p[0] < 0.01 && p[1] < 0.01 && p[2] > 0.3 && p[2] < 0.8).count()
    };
    assert!(shaded(true) > 20, "a shadow falls on the incoming picture: {} pixels", shaded(true));
    assert_eq!(shaded(false), 0, "none without");

    // Tilted most of the way, a panel is darker than its own picture.
    let tilt = PieceParams { motion: PieceMotion::Tilt, ..shrink };
    let mid = render(TransitionKind::SlidePanels, tilt.to_user(TransitionKind::SlidePanels), &a, &b, 0.6);
    let lit: Vec<[f32; 4]> = mid.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).filter(|p| !is_blue(*p) && p[2] < 0.01).collect();
    assert!(!lit.is_empty(), "something of the panels is left");
    let brightest_green = lit.iter().map(|p| p[1]).fold(0.0f32, f32::max);
    assert!(brightest_green < 0.95, "turned away from the light: {brightest_green}");
}

/// The shutter blurs moving pieces: blending instants across it leaves
/// more pixels between the two pictures than one instant does, and none
/// at rest.
#[test]
fn an_open_shutter_blurs_moving_pieces() {
    let (a, b) = (ramp(), blue());
    let kind = TransitionKind::Shatter;
    let u = spec_for_kind(kind).default_params;
    let id = spec_for_kind(kind).kind_id as u32;
    let between = |img: &[f32]| img.chunks_exact(4).filter(|p| p[2] > 0.05 && p[2] < 0.95).count();
    let sharp = pieces::render(&a, &b, &pieces::plan(id, 0.5, W, H, [u.param0, u.param1, u.param2, u.param3], 0.0, 1));
    let blurred = pieces::render(&a, &b, &pieces::plan(id, 0.5, W, H, [u.param0, u.param1, u.param2, u.param3], 0.08, 6));
    assert!(between(&blurred) > between(&sharp) * 2, "blurred {} vs sharp {}", between(&blurred), between(&sharp));
    let at_rest = pieces::render(&a, &b, &pieces::plan(id, 0.0, W, H, [u.param0, u.param1, u.param2, u.param3], 0.08, 6));
    assert!(close(&at_rest, &a, 2e-3), "nothing has moved yet: nothing blurs");
}

/// The shards tile the frame: at rest every pixel is in one, and there
/// are about as many as asked for.
#[test]
fn the_shards_tile_the_frame() {
    for count in [6u32, 24, 60] {
        let u = PieceParams { count, ..PieceParams::read(TransitionKind::Shatter, &spec_for_kind(TransitionKind::Shatter).default_params) }.to_user(TransitionKind::Shatter);
        let plan = pieces::plan(71, 0.0, W, H, [u.param0, u.param1, u.param2, u.param3], 0.0, 1);
        assert!((plan.count as i64 - count as i64).abs() <= (count as i64) / 4 + 1, "{count} asked, {} cut", plan.count);
        let (a, b) = (ramp(), blue());
        assert_eq!(blue_share(&pieces::render(&a, &b, &plan)), 0.0, "{count}: no gaps between the shards at rest");
    }
}

/// The GPU draws the posed pieces as the CPU does: every kind and motion,
/// leaving and arriving, with shadows and a gap, and with the shutter open.
#[test]
fn posed_pieces_match_between_cpu_and_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let mut from = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let check = if (x / 5 + y / 5) % 2 == 0 { 0.8 } else { 0.15 };
            from.extend_from_slice(&[check * x as f32 / W as f32, 0.3 + 0.4 * y as f32 / H as f32, check, 1.0]);
        }
    }
    let to: Vec<f32> = (0..W * H).flat_map(|i| [0.1, (i % 7) as f32 / 7.0, 0.5, 1.0]).collect();
    let mut failures = Vec::new();
    for kind in [TransitionKind::SlideStrips, TransitionKind::SlidePanels, TransitionKind::PartHalves, TransitionKind::Shatter] {
        for (i, motion) in POSED.into_iter().enumerate() {
            for (assemble, shutter) in [(false, 0.0f32), (true, 0.06)] {
                let base = PieceParams::read(kind, &spec_for_kind(kind).default_params);
                let u = PieceParams { motion, assemble, shadow: true, gap: (i * 3) as u32, angle: 20.0 + 70.0 * i as f32, seed: i as u32 * 40, ..base }.to_user(kind);
                let id = spec_for_kind(kind).kind_id as u32;
                for p in [0.3f32, 0.7] {
                    let plan = pieces::plan(id, p, W, H, [u.param0, u.param1, u.param2, u.param3], shutter, 4);
                    let cpu = pieces::render(&from, &to, &plan);
                    let g = gpu.render_pieces_linear(&from, &to, &plan);
                    let (c, g) = (linear_f32_rgba_to_srgb_u8(&cpu), linear_f32_rgba_to_srgb_u8(&g));
                    let over = c.iter().zip(&g).enumerate().filter(|(i, (x, y))| i % 4 != 3 && (**x as i32 - **y as i32).abs() > 1).count();
                    if over > 6 {
                        failures.push(format!("{kind:?} {motion:?} assemble {assemble} shutter {shutter} p {p}: {over} values over 1"));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// What a posed frame costs at 4K on the GPU, plan and pass, for the
/// shatter's default and its most shards. Run by hand: timings say
/// nothing on a busy machine.
#[test]
#[ignore]
fn posed_pieces_cost_at_4k() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let (w, h) = (3840u32, 2160u32);
    let a = vec![0.4f32; (w * h * 4) as usize];
    let b = vec![0.7f32; (w * h * 4) as usize];
    // A dissolve moves the same frames in and out: the pieces' own cost
    // is what they take over it.
    let _ = gpu.render_transition_linear(&a, &b, 0, 0.5, w, h, 0.0, 0.0, 0.0, 0.0, [1.0; 4]);
    let t = std::time::Instant::now();
    let _ = gpu.render_transition_linear(&a, &b, 0, 0.5, w, h, 0.0, 0.0, 0.0, 0.0, [1.0; 4]);
    eprintln!("a dissolve, with upload and readback: {:?}", t.elapsed());
    for (count, samples) in [(24u32, 1u32), (96, 1), (24, 6)] {
        let u = PieceParams { count, ..PieceParams::read(TransitionKind::Shatter, &spec_for_kind(TransitionKind::Shatter).default_params) }.to_user(TransitionKind::Shatter);
        let p4 = [u.param0, u.param1, u.param2, u.param3];
        let _ = gpu.render_pieces_linear(&a, &b, &pieces::plan(71, 0.5, w, h, p4, 0.05, samples));
        let t = std::time::Instant::now();
        let plan = pieces::plan(71, 0.5, w, h, p4, if samples > 1 { 0.05 } else { 0.0 }, samples);
        let planned = t.elapsed();
        let _ = gpu.render_pieces_linear(&a, &b, &plan);
        eprintln!("{count} shards, {samples} instants: plan {planned:?}, frame (with upload and readback) {:?}", t.elapsed());
    }
}
