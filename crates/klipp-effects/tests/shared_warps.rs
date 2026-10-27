//! The warps the Warp effect and the bending transitions share (#2758):
//! one function each in `klipp_effects::warp`. The transitions that call
//! them (Ripple, Swirl, Wave Distortion, Glitch RGB Split) draw on the CPU
//! the very bits they drew before the warps were shared, held here by a
//! hash of each picture; and each shape moves pixels as it says.

use klipp_effects::render_transition_linear;
use klipp_effects::warp;

const W: u32 = 96;
const H: u32 = 54;

/// Each transition's picture before the warps were shared: kind, set of
/// parameters, progress, and the FNV-1a hash of its f32 bits.
const BEFORE: &[(u32, usize, f32, u64)] = &[
    (20, 0, 0.25, 0x6609019bfd55a5ce),
    (20, 0, 0.5, 0xfe43cc055aa60bca),
    (20, 0, 0.75, 0x8f55de144f0701cd),
    (20, 1, 0.25, 0x6fc6c06741114a57),
    (20, 1, 0.5, 0x2b395983c1b2305a),
    (20, 1, 0.75, 0x0d65c931cde90790),
    (20, 2, 0.25, 0x1ebcd77024df3b1c),
    (20, 2, 0.5, 0xa3d06380809403d3),
    (20, 2, 0.75, 0x7ae0dc538ddbacc8),
    (20, 3, 0.25, 0x4acf45b8ad9437ea),
    (20, 3, 0.5, 0xacc139a91323448c),
    (20, 3, 0.75, 0x9d216e7cd7b28dc2),
    (53, 0, 0.25, 0x80ad233bc20434f7),
    (53, 0, 0.5, 0xc2cd3439737e0a1a),
    (53, 0, 0.75, 0xe230c0ae2790fd41),
    (53, 1, 0.25, 0x405a3993c9cb9689),
    (53, 1, 0.5, 0x6d69fe210bdd0b91),
    (53, 1, 0.75, 0x224454b11c9f61bf),
    (53, 2, 0.25, 0x99c59ef1056f459e),
    (53, 2, 0.5, 0xdc60c7108aa3ab17),
    (53, 2, 0.75, 0xb1cb58edaca236e9),
    (53, 3, 0.25, 0xf63eebcb4dbf8fde),
    (53, 3, 0.5, 0x46caa41970e47af6),
    (53, 3, 0.75, 0x8d80c06ec0565f41),
    (54, 0, 0.25, 0x6d1f0fee3ff10207),
    (54, 0, 0.5, 0xb5eddcbd33053e1a),
    (54, 0, 0.75, 0x1ed7da516f077dfa),
    (54, 1, 0.25, 0xf54434afcec369c3),
    (54, 1, 0.5, 0xb19f0cf9ccf414c5),
    (54, 1, 0.75, 0xd7b03e7cf3026458),
    (54, 2, 0.25, 0x32981de998c6fdf1),
    (54, 2, 0.5, 0xbbc6d131bfd10532),
    (54, 2, 0.75, 0xf2094021ead8ec49),
    (54, 3, 0.25, 0x409e1af830c98ce6),
    (54, 3, 0.5, 0xcba7b4bb01331737),
    (54, 3, 0.75, 0xb32974a8f09241c9),
    (57, 0, 0.25, 0x195d8971b1877210),
    (57, 0, 0.5, 0xa434768c6ba74e08),
    (57, 0, 0.75, 0x42b6d381d6c81c79),
    (57, 1, 0.25, 0xfd2651a3d0a18c01),
    (57, 1, 0.5, 0xc371d2cd4bfb5ad9),
    (57, 1, 0.75, 0x22bdcae431c7dd2d),
    (57, 2, 0.25, 0x003aed576ddf0edb),
    (57, 2, 0.5, 0xdb5484ff14084802),
    (57, 2, 0.75, 0x7c12ccab9f373b85),
    (57, 3, 0.25, 0x64262a8639b90d95),
    (57, 3, 0.5, 0x7279026f4e952b11),
    (57, 3, 0.75, 0x36ef64835cfda135),
];

fn fnv(v: &[f32]) -> u64 {
    v.iter().flat_map(|f| f.to_bits().to_le_bytes()).fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

#[test]
fn the_bending_transitions_draw_what_they_drew() {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            a.extend_from_slice(&[0.8 * fx, 0.2 + 0.3 * fy, 0.1, 1.0]);
            let check = if (x / 6 + y / 6) % 2 == 0 { 0.7 } else { 0.2 };
            b.extend_from_slice(&[0.05, check * fy, check, 1.0]);
        }
    }
    let sets = [[4.0, 0.5, 0.5, 0.5], [30.0, 8.0, 2.0, 0.0], [180.0, 3.0, 1.0, 2.0], [0.8, 6.0, 0.0, 1.0]];
    for &(kind, set, p, want) in BEFORE {
        let s = sets[set];
        let got = render_transition_linear(&a, &b, kind, p, W, H, s[0], s[1], s[2], s[3], [0.8, 0.3, 0.1, 1.0]);
        assert_eq!(fnv(&got), want, "kind {kind}, parameters {s:?}, at {p}: not the picture it drew");
    }
}

#[test]
fn each_shape_moves_pixels_as_it_says() {
    macro_rules! at {
        ($e:expr) => {{
            let p = $e;
            (p.x, p.y)
        }};
    }
    let (w, h) = (200.0, 100.0);
    // A bulge takes a pixel from nearer its centre, a pinch from further
    // out; outside its reach, and at its edge, nothing moves.
    let (bx, _) = at!(warp::bulge(70.0, 50.0, 100.0, 50.0, 50.0, 0.5));
    assert!(bx > 70.0 && bx < 100.0, "a bulge reads nearer the centre: {bx}");
    let (px, _) = at!(warp::bulge(70.0, 50.0, 100.0, 50.0, 50.0, -0.5));
    assert!(px < 70.0, "a pinch reads further out: {px}");
    assert_eq!(at!(warp::bulge(10.0, 50.0, 100.0, 50.0, 50.0, 0.5)), (10.0, 50.0));
    // A twirl turns most at its centre and less away from it.
    let near = at!(warp::twirl(105.0, 50.0, w, h, 100.0, 50.0, 50.0, std::f32::consts::FRAC_PI_2, 3.0));
    let far = at!(warp::twirl(140.0, 50.0, w, h, 100.0, 50.0, 50.0, std::f32::consts::FRAC_PI_2, 3.0));
    let turn = |(_, y): (f32, f32), r: f32| ((y - 50.0) / r).asin().abs();
    assert!(turn(near, 5.0) > turn(far, 40.0), "the turn falls away from the centre");
    // Torn rows move along their row only, some rows not at all.
    let shifts: Vec<f32> = (0..20).map(|band| warp::band_shift(band as f32 * 5.0 + 1.0, 5.0, 3, 1.0, w)).collect();
    assert!(shifts.contains(&0.0) && shifts.iter().any(|s| *s > 0.0), "some rows torn, some left: {shifts:?}");
    // A wave moves along as its time does.
    let s = |time: f32| warp::Settings { mode: warp::WAVE, amplitude: 40.0, wavelength: 0.25, speed: 1.0, direction: 0, angle: 0.0, strength: 0.0, radius: 0.5, center_x: 0.5, center_y: 0.5, band: 0.04, time };
    assert_ne!(at!(warp::source(&s(0.0), 50.0, 30.0, w, h)), at!(warp::source(&s(0.3), 50.0, 30.0, w, h)), "it moves in time");
    assert!(warp::is_identity(&warp::Settings { amplitude: 0.0, ..s(0.0) }), "no amplitude, no wave");
}
