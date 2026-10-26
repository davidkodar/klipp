//! Project revision 5 (#2754): a tracked surface's keys hold the map from
//! its reference corners to each frame, not the four corners alone, so a
//! layer can sit anywhere on the plane and each frame can say how sure
//! the tracker was. A revision-4 key's corners become the map that takes
//! the reference corners to them: the same corners, exactly, and no
//! confidence (the tracker that made them did not give one).
//!
//! This is the conversion on a project's JSON; `klipp-convert` step 5 is
//! its run over every project, once.

use serde_json::{json, Value};

/// The revision converted from, and to.
pub const FROM: u64 = 4;
pub const TO: u64 = 5;

/// How close a converted key's corners must come to its old ones, in
/// fractions of the frame.
const LOSSLESS: f64 = 1e-6;

type Quad = [(f64, f64); 4];

fn quad(v: &Value) -> Option<Quad> {
    let a = v.as_array().filter(|a| a.len() == 4)?;
    let mut q = [(0.0, 0.0); 4];
    for (i, c) in a.iter().enumerate() {
        let c = c.as_array().filter(|c| c.len() == 2)?;
        q[i] = (c[0].as_f64()?, c[1].as_f64()?);
    }
    Some(q)
}

/// The map taking `from`'s corners to `to`'s, row by row.
fn map_between(from: &Quad, to: &Quad) -> Option<[f64; 9]> {
    let h = klipp_effects::homography::Homography::from_4_points(from, to)?.h;
    Some([h[0][0], h[0][1], h[0][2], h[1][0], h[1][1], h[1][2], h[2][0], h[2][1], h[2][2]])
}

fn mapped(m: &[f64; 9], q: &Quad) -> Quad {
    let h = klipp_effects::homography::Homography { h: [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]] };
    q.map(|(x, y)| h.transform(x, y))
}

/// Each surface in `v`, wherever it is (a clip on any track, in any
/// sequence or compound), handed to `f` with where it is.
fn each_surface(v: &mut Value, at: &str, f: &mut dyn FnMut(&mut Value, &str) -> Result<(), String>) -> Result<(), String> {
    match v {
        Value::Object(o) => {
            for (k, child) in o.iter_mut() {
                let here = format!("{at}/{k}");
                if k == "surfaces" {
                    if let Value::Array(surfaces) = child {
                        for (i, s) in surfaces.iter_mut().enumerate() {
                            f(s, &format!("{here}/{i}"))?;
                        }
                    }
                } else {
                    each_surface(child, &here, f)?;
                }
            }
            Ok(())
        }
        Value::Array(a) => {
            for (i, child) in a.iter_mut().enumerate() {
                each_surface(child, &format!("{at}/{i}"), f)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// One revision-4 surface's keys as revision 5's: each key's corners as
/// the map from the surface's reference corners. A key whose corners
/// can't be reached by a map (a degenerate quad) is refused by name.
fn surface(s: &mut Value, at: &str) -> Result<(), String> {
    let reference = s.get("corners").and_then(quad).ok_or_else(|| format!("{at}: a surface without four reference corners"))?;
    let Some(keys) = s.get_mut("keys").and_then(Value::as_array_mut) else { return Ok(()) };
    for (i, k) in keys.iter_mut().enumerate() {
        let o = k.as_object_mut().ok_or_else(|| format!("{at}/keys/{i}: not a key"))?;
        let corners = o.get("corners").and_then(quad).ok_or_else(|| format!("{at}/keys/{i}: a key without four corners"))?;
        let map = map_between(&reference, &corners).ok_or_else(|| format!("{at}/keys/{i}: its corners or the reference's make no shape to map"))?;
        o.remove("corners");
        o.insert("map".into(), json!(map));
    }
    Ok(())
}

/// What a revision-4 project holds that this step changes: its surfaces
/// and their keys.
pub fn check(raw: &Value) -> Result<(usize, usize), String> {
    match raw.get("format_revision").and_then(Value::as_u64) {
        Some(FROM) => {}
        other => return Err(format!("a project at revision {other:?}, not {FROM}")),
    }
    let (mut surfaces, mut keys) = (0, 0);
    let mut copy = raw.clone();
    each_surface(&mut copy, "", &mut |s, at| {
        surfaces += 1;
        keys += s.get("keys").and_then(Value::as_array).map_or(0, Vec::len);
        surface(s, at)
    })?;
    Ok((surfaces, keys))
}

/// `raw`, a revision-4 project, at revision 5.
pub fn project(raw: &Value) -> Result<Value, String> {
    check(raw)?;
    let mut out = raw.clone();
    each_surface(&mut out, "", &mut surface)?;
    out.as_object_mut().ok_or("not a project")?.insert("format_revision".into(), json!(TO));
    Ok(out)
}

/// [`project`] on a project's text, written as the app writes it.
pub fn project_text(text: &str) -> Result<String, String> {
    let raw: Value = serde_json::from_str(text).map_err(|e| format!("doesn't parse: {e}"))?;
    serde_json::to_string_pretty(&project(&raw)?).map_err(|e| e.to_string())
}

/// A written revision-5 text held to its revision-4 original: the
/// original's conversion again, the same document, at revision 5, and
/// every key's map taking the reference corners to the key's old corners.
pub fn verify(original: &str, written: &str) -> Result<(), String> {
    let old: Value = serde_json::from_str(original).map_err(|e| format!("the original doesn't parse: {e}"))?;
    let new: Value = serde_json::from_str(written).map_err(|e| format!("the written file doesn't parse: {e}"))?;
    if new.get("format_revision").and_then(Value::as_u64) != Some(TO) {
        return Err(format!("written at revision {:?}", new.get("format_revision")));
    }
    if crate::canon::sorted(project(&old)?) != crate::canon::sorted(new.clone()) {
        return Err("the written file isn't the original's conversion".into());
    }
    // Lossless: each key's corners again from its map, against the old.
    let mut before: Vec<(String, Quad, Vec<Quad>)> = Vec::new();
    each_surface(&mut old.clone(), "", &mut |s, at| {
        let reference = s.get("corners").and_then(quad).ok_or("no reference corners")?;
        let keys = s.get("keys").and_then(Value::as_array).map(|k| k.iter().filter_map(|k| k.get("corners").and_then(quad)).collect()).unwrap_or_default();
        before.push((at.to_owned(), reference, keys));
        Ok(())
    })?;
    let mut after: Vec<Vec<[f64; 9]>> = Vec::new();
    each_surface(&mut new.clone(), "", &mut |s, _| {
        let maps = s.get("keys").and_then(Value::as_array).map(|k| k.iter().filter_map(|k| serde_json::from_value::<[f64; 9]>(k.get("map")?.clone()).ok()).collect()).unwrap_or_default();
        after.push(maps);
        Ok(())
    })?;
    if before.len() != after.len() {
        return Err(format!("{} surfaces before, {} after", before.len(), after.len()));
    }
    for ((at, reference, corners), maps) in before.iter().zip(&after) {
        if corners.len() != maps.len() {
            return Err(format!("{at}: {} keys before, {} after", corners.len(), maps.len()));
        }
        for (i, (c, m)) in corners.iter().zip(maps).enumerate() {
            let again = mapped(m, reference);
            let worst = c.iter().zip(&again).map(|(a, b)| (a.0 - b.0).abs().max((a.1 - b.1).abs())).fold(0.0, f64::max);
            if worst > LOSSLESS {
                return Err(format!("{at}/keys/{i}: its map misses its old corners by {worst:e}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project4() -> Value {
        json!({
            "format_revision": 4,
            "sequences": [{ "tracks": [{ "clips": [{
                "surfaces": [{
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "Screen",
                    "corners": [[0.3, 0.3], [0.6, 0.32], [0.62, 0.6], [0.28, 0.58]],
                    "reference_secs": 2.5,
                    "keys": [
                        { "source_secs": 2.5, "corners": [[0.3, 0.3], [0.6, 0.32], [0.62, 0.6], [0.28, 0.58]] },
                        { "source_secs": 2.54, "corners": [[0.31, 0.29], [0.62, 0.33], [0.6, 0.61], [0.27, 0.57]] }
                    ]
                }]
            }] }] }]
        })
    }

    /// Every key's corners become the map to them, exactly, the rest of
    /// the project untouched, at revision 5; the check proves it.
    #[test]
    fn a_surfaces_keys_become_maps_to_the_same_corners() {
        let old = project4();
        let new = project(&old).unwrap();
        assert_eq!(new["format_revision"], json!(5));
        let key = &new["sequences"][0]["tracks"][0]["clips"][0]["surfaces"][0]["keys"][1];
        assert!(key.get("corners").is_none() && key["map"].as_array().unwrap().len() == 9, "{key}");
        assert_eq!(new["sequences"][0]["tracks"][0]["clips"][0]["surfaces"][0]["corners"], old["sequences"][0]["tracks"][0]["clips"][0]["surfaces"][0]["corners"]);
        let (o, n) = (serde_json::to_string(&old).unwrap(), serde_json::to_string_pretty(&new).unwrap());
        verify(&o, &n).unwrap();
        assert_eq!(check(&old).unwrap(), (1, 2));
    }

    /// A tampered key is caught: its map no longer gives its corners.
    #[test]
    fn a_map_that_misses_its_corners_is_refused() {
        let old = project4();
        let mut new = project(&old).unwrap();
        new["sequences"][0]["tracks"][0]["clips"][0]["surfaces"][0]["keys"][1]["map"][2] = json!(0.5);
        let err = verify(&serde_json::to_string(&old).unwrap(), &serde_json::to_string(&new).unwrap()).unwrap_err();
        assert!(err.contains("isn't the original's conversion"), "{err}");
    }

    /// Only a revision-4 project is converted; a degenerate key is named.
    #[test]
    fn only_revision_four_and_shaped_keys_convert() {
        let mut old = project4();
        old["format_revision"] = json!(3);
        assert!(project(&old).unwrap_err().contains("revision Some(3)"));
        let mut flat = project4();
        flat["sequences"][0]["tracks"][0]["clips"][0]["surfaces"][0]["keys"][1]["corners"] = json!([[0.1, 0.1], [0.1, 0.1], [0.1, 0.1], [0.1, 0.1]]);
        assert!(project(&flat).unwrap_err().contains("keys/1"));
    }
}
