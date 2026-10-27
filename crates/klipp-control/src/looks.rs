//! Looks: the bundled LUTs, found and picked by name, and the knobs a grade
//! match turns (#826).

#[allow(unused_imports)]
use super::*;

/// Where the bundled LUT library sits: next to the executable, or in the
/// source tree at build time. The one lookup the app and projects share.
pub(crate) fn bundled_luts_dir() -> Option<std::path::PathBuf> {
    klipp_core::assets::bundled_luts_dir().map(|p| p.to_path_buf())
}

pub(crate) const LOOK_CATEGORIES: &[(&str, &str)] = &[
    ("film-bw", "Film B&W"),
    ("film-slide", "Film Slide"),
    ("film-negative", "Film Negative"),
    ("film-print", "Film Print"),
    ("film-instant", "Film Instant"),
    ("fuji-xtrans", "Fuji X-Trans"),
    // A camera's log or flat profile to normal: on normal footage they
    // lift its blacks (#2769). The folder keeps its old name, which
    // saved grades point into.
    ("cinematic", "Camera conversion (log footage)"),
];

/// One bundled look: its category, its display name (the file stem), path.
pub(crate) fn scan_looks() -> Vec<(String, String, std::path::PathBuf)> {
    let mut out = Vec::new();
    let Some(base) = bundled_luts_dir() else { return out };
    for (dir, _label) in LOOK_CATEGORIES {
        let cat = base.join(dir);
        let Ok(rd) = std::fs::read_dir(&cat) else { continue };
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "cube" || x == "3dl" || x == "csp"))
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let stem = e.path().file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            out.push((dir.to_string(), stem, e.path()));
        }
    }
    out
}

/// Resolve a look name (or an absolute .cube path) to a file and a
/// display name. A query matches when every whitespace token appears in
/// the category or the file name; an exact name wins a tie.
pub fn resolve_look(name: &str) -> Result<(std::path::PathBuf, String), String> {
    let q = name.trim();
    // An explicit path to a real LUT file is taken as-is.
    let as_path = std::path::Path::new(q);
    if as_path.is_absolute() && as_path.is_file() {
        let stem = as_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| q.to_string());
        return Ok((as_path.to_path_buf(), stem));
    }
    let looks = scan_looks();
    if looks.is_empty() {
        return Err("the bundled LUT library was not found next to the app; give an absolute .cube path instead".into());
    }
    pick_look(&looks, q)
}

/// The look in `looks` (category, name, file) that `q` names: words that
/// all appear in "category name", where a `/` is a word break too, so the
/// `category/name` that `looks` lists is taken as it is shown (#656).
/// Several matching: the one whose name, or category/name, is `q`
/// exactly; else a list to choose from.
pub(crate) fn pick_look(looks: &[(String, String, std::path::PathBuf)], q: &str) -> Result<(std::path::PathBuf, String), String> {
    let ql = q.trim().to_lowercase();
    let tokens: Vec<&str> = ql.split(|c: char| c.is_whitespace() || c == '/').filter(|t| !t.is_empty()).collect();
    let hay = |cat: &str, stem: &str| format!("{cat} {stem}").to_lowercase();
    let matches: Vec<&(String, String, std::path::PathBuf)> = looks.iter()
        .filter(|(cat, stem, _)| tokens.iter().all(|t| hay(cat, stem).contains(t)))
        .collect();
    match matches.len() {
        0 => {
            let sample: Vec<String> = looks.iter().take(8).map(|(c, s, _)| format!("{c}/{s}")).collect();
            Err(format!("no look matches \"{q}\"; call looks to see the library. A few: {}", sample.join(", ")))
        }
        1 => Ok((matches[0].2.clone(), matches[0].1.clone())),
        _ => {
            // An exact name, or category/name, settles it; otherwise ask
            // to be specific.
            if let Some(exact) = matches.iter().find(|(c, s, _)| s.to_lowercase() == ql || format!("{c}/{s}").to_lowercase() == ql) {
                return Ok((exact.2.clone(), exact.1.clone()));
            }
            let names: Vec<String> = matches.iter().take(10).map(|(c, s, _)| format!("{c}/{s}")).collect();
            Err(format!("\"{q}\" matches {} looks; be more specific: {}", matches.len(), names.join(", ")))
        }
    }
}

/// The grade knobs that move a shot toward a reference shot, from the
/// two frames' scope readings: lift from the black-point gap, gain from
/// the white-point gap, exposure from the mid gap, and temperature and
/// tint from the RGB balance. A first pass a colourist would refine, not
/// a final answer — verify with the colour check and nudge. A knob whose
/// gap is within tolerance is left alone (`None`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MatchKnobs {
    pub exposure: Option<f32>,
    pub lift: Option<f32>,
    pub gain: Option<f32>,
    pub temperature: Option<f32>,
    pub tint: Option<f32>,
}

impl MatchKnobs {
    /// Nothing to do: every knob is within tolerance.
    pub fn is_empty(&self) -> bool {
        self.exposure.is_none() && self.lift.is_none() && self.gain.is_none() && self.temperature.is_none() && self.tint.is_none()
    }
}

pub fn match_grade_knobs(target: &serde_json::Value, reference: &serde_json::Value) -> MatchKnobs {
    let ire = |v: &serde_json::Value, k: &str| v.get("luma_ire").and_then(|x| x.get(k)).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let bal = |v: &serde_json::Value, k: &str| v.get("rgb_balance").and_then(|x| x.get(k)).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let some_if = |v: f32, thresh: f32| if v.abs() > thresh { Some(v) } else { None };

    // Black and white points: the gap in IRE, as a lift/gain offset.
    let lift = some_if(((ire(reference, "black") - ire(target, "black")) / 100.0).clamp(-0.3, 0.3) as f32, 0.015);
    let gain = some_if(((ire(reference, "white") - ire(target, "white")) / 100.0).clamp(-0.3, 0.3) as f32, 0.015);
    // Mid: the exposure in stops that brings the mid grey together.
    let (tm, rm) = (ire(target, "mid").max(0.5), ire(reference, "mid").max(0.5));
    let exposure = some_if((rm / tm).log2().clamp(-1.0, 1.0) as f32, 0.05);
    // Balance (#2878): the white balance, in the grade's mireds, that
    // moves the target's average colour by the Lab a*/b* gap to the
    // reference's, solved through the balance itself. The knobs had been
    // the RGB gap times four, under a mired: nothing visible moved.
    let linear = |v: &serde_json::Value| ["red", "green", "blue"].map(|k| klipp_color::transfer::srgb_to_linear(bal(v, k) as f32));
    let lab = |c: [f32; 3]| klipp_color::measure::linear_to_lab(c[0], c[1], c[2]);
    let (t, r) = (lab(linear(target)), lab(linear(reference)));
    let solve = |k: f32| klipp_color::white_balance::white_balance_for_lab_shift(linear(target), (r.a - t.a) * k, (r.b - t.b) * k, klipp_core::WorkingColorspace::Rec709Linear);
    // A gap past the grade's ±50 is closed as far as the range goes, in
    // its own direction: the shift scaled down, not one knob clamped.
    let (mut temp, mut tnt) = solve(1.0);
    let mut k = 1.0f32;
    for _ in 0..4 {
        let over = temp.abs().max(tnt.abs()) / 50.0;
        if over <= 1.0 {
            break;
        }
        k /= over * 1.02;
        (temp, tnt) = solve(k);
    }
    // A mired moves a grey about 0.2 b*: under one is not seen.
    let temperature = some_if(temp.clamp(-50.0, 50.0), 1.0);
    let tint = some_if(tnt.clamp(-50.0, 50.0), 1.0);

    MatchKnobs { exposure, lift, gain, temperature, tint }
}

/// Holds `knobs` short of clipping the target `m`, as grade_to_target
/// does (#2476): a picture already losing its highlights is not
/// brightened at all, and none is brightened past the headroom above
/// its 95th percentile. A match had taken a shot up a stop and left a
/// fifth of the frame at white. Returns why, when a knob was held.
pub fn hold_match_for_highlights(knobs: &mut MatchKnobs, m: &klipp_color::measure::FrameMeasure) -> Option<String> {
    let clipping = m.clip_hi_pct > 1.0 || m.gamut_clip_pct > klipp_color::measure::GAMUT_WARN;
    let room = klipp_color::measure::exposure_headroom(m);
    let wanted = knobs.exposure.unwrap_or(0.0);
    let why = if clipping && (wanted > 0.0 || knobs.gain.is_some_and(|g| g > 0.0)) {
        knobs.exposure = knobs.exposure.filter(|e| *e < 0.0);
        knobs.gain = knobs.gain.filter(|g| *g < 0.0);
        format!("not brightened: {:.1}% of the shot is already at white", m.clip_hi_pct.max(m.gamut_clip_pct))
    } else if wanted > room + 0.01 {
        // A hold this close to nothing is no move at all.
        knobs.exposure = (room > 0.05).then_some(room);
        knobs.gain = knobs.gain.filter(|g| *g < 0.0);
        format!("exposure held at {room:+.2} of the {wanted:+.2} the match wanted: more would clip the shot's highlights")
    } else {
        return None;
    };
    Some(why)
}
