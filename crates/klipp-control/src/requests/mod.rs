//! The request handler's arms, split by area; `handle` in lib.rs routes
//! each request to its area's `respond` (#826).

pub(crate) mod session;
pub(crate) mod analysis;
pub(crate) mod colour;
pub(crate) mod review;
pub(crate) mod media;
pub(crate) mod editing;
pub(crate) mod cutting;
pub(crate) mod planning;
pub(crate) mod output;

#[cfg(test)]
mod routing_tests {
    use std::collections::BTreeSet;

    const AREAS: [(&str, &str); 9] = [
        ("session", include_str!("session.rs")),
        ("analysis", include_str!("analysis.rs")),
        ("colour", include_str!("colour.rs")),
        ("review", include_str!("review.rs")),
        ("media", include_str!("media.rs")),
        ("editing", include_str!("editing.rs")),
        ("cutting", include_str!("cutting.rs")),
        ("planning", include_str!("planning.rs")),
        ("output", include_str!("output.rs")),
    ];

    /// The variants named in `text` as `ControlCommand::X` patterns.
    fn variants(text: &str) -> BTreeSet<String> {
        text.split("ControlCommand::").skip(1)
            .map(|rest| rest.chars().take_while(|c| c.is_alphanumeric()).collect())
            .collect()
    }

    /// The variants an area's arms answer: each arm starts at the arm
    /// indent, with `ControlCommand::` or a binding `name @ (`.
    fn answered(area: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for line in area.lines() {
            let Some(arm) = line.strip_prefix("        ") else { continue };
            if arm.starts_with("ControlCommand::") || (arm.contains(" @ (") && arm.contains("ControlCommand::")) {
                out.extend(variants(arm.split("=>").next().unwrap_or(arm)));
            }
        }
        out
    }

    // `handle` routes each request to the area that answers it: a request
    // routed elsewhere would fall to the area's "wrong area" error.
    #[test]
    fn every_request_goes_to_the_area_that_answers_it() {
        let lib = include_str!("../lib.rs");
        let start = lib.find("\nfn handle(").expect("handle in lib.rs");
        let handle = &lib[start..start + lib[start..].find("\n}\n").expect("end of handle")];
        let mut routed_all = BTreeSet::new();
        for (name, text) in AREAS {
            let call = format!(") => requests::{name}::respond(ctx, cmd, collect),");
            let at = handle.find(&call).unwrap_or_else(|| panic!("no route to {name}"));
            let open = handle[..at].rfind("cmd @ (").expect("route pattern");
            let routed = variants(&handle[open..at]);
            assert_eq!(routed, answered(text), "{name}: routed vs answered");
            assert!(routed_all.is_disjoint(&routed), "{name} shares a variant");
            routed_all.extend(routed);
        }
        assert_eq!(routed_all.len(), 106);
    }
}
