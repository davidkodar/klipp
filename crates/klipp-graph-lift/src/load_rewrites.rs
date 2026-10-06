//! Rewrites today's reader and writer make on a file's way through the app,
//! applied once by the conversion so a converted file's first save changes
//! nothing in its pictures (#1089).
//!
//! The lift reproduces the rewrites that depend only on the file (the Color
//! Warper migration, the default pipeline). This one also depends on the
//! machine, so it is a pass on the old file before the lift, with the rule
//! handed in by the caller: the conversion knows where this machine's LUT
//! library is, and this crate touches no file.
//!
//! **A grade node's LUT path.** Today's reader resolves a stored `lut_path`
//! against this machine's LUT library (`bundled:<name>`, or an absolute
//! path into some install's or checkout's `assets/luts/` holding the same
//! file), and its writer stores a file in the library as `bundled:<name>`.
//! So an old absolute path to a bundled look is rewritten on the first save
//! after loading. The same LUT is used before and after; only the stored
//! name changes, so this is a kept rewrite and is reported, not refused.

use serde_json::Value;

/// One LUT path the pass rewrote: where, what it was, what it became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LutPathRewrite {
    pub at: String,
    pub from: String,
    pub to: String,
}

/// Rewrite every grade node's `lut_path` in `value` (a whole project, or a
/// Look, a still or a preset) by `rule`: today's resolve-then-store, as
/// the caller computes it for this machine. Only the string values of
/// `lut_path` keys inside a node's `params` change.
pub fn portable_lut_paths(value: &mut Value, rule: &dyn Fn(&str) -> String) -> Vec<LutPathRewrite> {
    let mut out = Vec::new();
    walk(value, "", false, rule, &mut out);
    out
}

fn walk(v: &mut Value, at: &str, in_params: bool, rule: &dyn Fn(&str) -> String, out: &mut Vec<LutPathRewrite>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                let here = format!("{at}/{k}");
                if in_params && k == "lut_path" {
                    if let Value::String(s) = x {
                        let to = rule(s);
                        if to != *s {
                            out.push(LutPathRewrite { at: here, from: s.clone(), to: to.clone() });
                            *s = to;
                        }
                    }
                    continue;
                }
                walk(x, &here, k == "params", rule, out);
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter_mut().enumerate() {
                walk(x, &format!("{at}/{i}"), false, rule, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(s: &str) -> String {
        match s.split_once("/assets/luts/") {
            Some((_, rel)) => format!("bundled:{rel}"),
            None => s.to_string(),
        }
    }

    #[test]
    fn only_a_nodes_lut_path_is_rewritten() {
        let mut v = json!({
            "sequences": [{ "tracks": [{ "clips": [{
                "node_graph": { "nodes": [
                    { "kind": "Lut3D", "params": { "lut_path": "/old/Klipp/assets/luts/fuji/a.cube", "lut_opacity": 0.4 } },
                    { "kind": "Lut3D", "params": { "lut_path": "bundled:fuji/b.cube" } },
                    { "kind": "Lut3D", "params": { "lut_path": "/home/me/my.cube" } }
                ] },
                // Not a node's params: left alone.
                "lut_path": "/old/Klipp/assets/luts/fuji/c.cube"
            }] }] }]
        });
        let done = portable_lut_paths(&mut v, &rule);
        assert_eq!(done.len(), 1, "{done:?}");
        assert_eq!(done[0].from, "/old/Klipp/assets/luts/fuji/a.cube");
        assert_eq!(done[0].to, "bundled:fuji/a.cube");
        let nodes = &v["sequences"][0]["tracks"][0]["clips"][0]["node_graph"]["nodes"];
        assert_eq!(nodes[0]["params"]["lut_path"], "bundled:fuji/a.cube");
        assert_eq!(nodes[0]["params"]["lut_opacity"], 0.4);
        assert_eq!(nodes[1]["params"]["lut_path"], "bundled:fuji/b.cube");
        assert_eq!(nodes[2]["params"]["lut_path"], "/home/me/my.cube");
        assert_eq!(v["sequences"][0]["tracks"][0]["clips"][0]["lut_path"], "/old/Klipp/assets/luts/fuji/c.cube");
        assert!(portable_lut_paths(&mut v, &rule).is_empty(), "a second pass changes nothing");
    }
}
