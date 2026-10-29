//! Stable ids (#2888): what templates, recipes and packs made by other
//! people address an effect, a node or a transition by, and its
//! parameters and ports, apart from the names shown for them.
//!
//! An effect is `klipp.fx.<tag>` and a grade node `klipp.grade.<kind>`, as
//! their registries declare them; a transition is
//! `klipp.transition.<key>`, its key in snake case. A parameter is its
//! descriptor's key and a port its key. Projects keep saving effects by
//! their serde tag and nodes by their kind, which these map to one for one.
//!
//! An id is renamed or removed only with an entry in [`MIGRATIONS`]: a
//! file naming the old id reads as the new one, or is refused saying why.
//! `tests/registry_ids.txt` holds every id declared so far, and a test
//! fails when one of them goes without an entry.

/// What an id names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject<'a> {
    /// An effect, node or transition.
    Type(&'a str),
    /// A parameter of the type.
    Param(&'a str, &'a str),
    /// A port of the type.
    Port(&'a str, &'a str),
}

/// What became of an old id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Read as this id now: a type's id, or a parameter's or port's key
    /// on the same type.
    Renamed(&'static str),
    /// Gone, and why.
    Removed(&'static str),
}

/// One old id and what became of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    pub old: Subject<'static>,
    pub change: Change,
}

/// Every renamed or removed id, oldest first. None yet.
pub const MIGRATIONS: &[Migration] = &[];

/// The id prefix of a transition.
pub const TRANSITION: &str = "klipp.transition.";

/// One declared type: its id, its parameters' keys and its ports' keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub id: String,
    pub params: Vec<String>,
    pub ports: Vec<String>,
}

/// Every effect, grade node and transition this Klipp declares.
pub fn declared() -> Vec<Declared> {
    let of = |d: &klipp_effects::registry::NodeTypeDesc| Declared {
        id: d.id.to_string(),
        params: d.params.iter().map(|p| p.key.to_string()).collect(),
        ports: d.inputs.iter().chain(d.outputs).map(|p| p.key.to_string()).collect(),
    };
    let effects = klipp_effects::registry::effect_types().map(|t| of(&t.desc));
    let nodes = klipp_nodes::registry::node_types().map(|t| of(&t.desc));
    let transitions = klipp_core::transition_registry::TRANSITION_SPECS.iter().map(|s| Declared {
        id: format!("{TRANSITION}{}", snake(s.string_key)),
        params: serde_json::to_value(s.default_params).ok().and_then(|v| v.as_object().map(|o| o.keys().cloned().collect())).unwrap_or_default(),
        ports: Vec::new(),
    });
    effects.chain(nodes).chain(transitions).collect()
}

/// `CrossDissolve` as `cross_dissolve`.
pub fn snake(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for (i, c) in key.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// The declared ids as lines, one an id, sorted: `type <id>`,
/// `param <id> <key>`, `port <id> <key>`. What `tests/registry_ids.txt`
/// holds.
pub fn snapshot() -> Vec<String> {
    let mut lines: Vec<String> = declared()
        .into_iter()
        .flat_map(|d| {
            let params = d.params.iter().map(|k| format!("param {} {k}", d.id)).collect::<Vec<_>>();
            let ports = d.ports.iter().map(|k| format!("port {} {k}", d.id)).collect::<Vec<_>>();
            std::iter::once(format!("type {}", d.id)).chain(params).chain(ports)
        })
        .collect();
    lines.sort();
    lines.dedup();
    lines
}

/// What an id reads as now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A current id (the one asked for, or what it was renamed to).
    Current(String),
    /// Removed, and why.
    Removed(&'static str),
    /// Neither declared nor in the table.
    Unknown,
    /// The table's renames run in a loop or past its length.
    Loops,
}

/// `subject` as it reads now through `table`: its renames followed, at
/// most once each, to a current id. `current` says whether an id is
/// declared now.
pub fn resolve_in(table: &[Migration], subject: Subject<'_>, current: &dyn Fn(Subject<'_>) -> bool) -> Resolved {
    let mut at: (String, Option<String>, bool) = match subject {
        Subject::Type(id) => (id.to_string(), None, false),
        Subject::Param(id, key) => (id.to_string(), Some(key.to_string()), false),
        Subject::Port(id, key) => (id.to_string(), Some(key.to_string()), true),
    };
    // Each rename is taken at most once: a longer chain is a loop.
    for _ in 0..=table.len() {
        let here = match (&at.1, at.2) {
            (None, _) => Subject::Type(&at.0),
            (Some(k), false) => Subject::Param(&at.0, k),
            (Some(k), true) => Subject::Port(&at.0, k),
        };
        if current(here) {
            return Resolved::Current(at.1.clone().unwrap_or_else(|| at.0.clone()));
        }
        match table.iter().find(|m| m.old == here).map(|m| m.change) {
            Some(Change::Renamed(to)) => match &mut at.1 {
                None => at.0 = to.to_string(),
                Some(k) => *k = to.to_string(),
            },
            Some(Change::Removed(why)) => return Resolved::Removed(why),
            None => return Resolved::Unknown,
        }
    }
    Resolved::Loops
}

/// Whether `subject` is declared now.
pub fn is_current(subject: Subject<'_>) -> bool {
    static DECLARED: std::sync::OnceLock<Vec<Declared>> = std::sync::OnceLock::new();
    let declared = DECLARED.get_or_init(declared);
    let find = |id: &str| declared.iter().find(|d| d.id == id);
    match subject {
        Subject::Type(id) => find(id).is_some(),
        Subject::Param(id, key) => find(id).is_some_and(|d| d.params.iter().any(|p| p == key)),
        Subject::Port(id, key) => find(id).is_some_and(|d| d.ports.iter().any(|p| p == key)),
    }
}

/// `subject` as it reads now, through [`MIGRATIONS`].
pub fn resolve(subject: Subject<'_>) -> Resolved {
    resolve_in(MIGRATIONS, subject, &is_current)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn renamed(old: Subject<'static>, to: &'static str) -> Migration {
        Migration { old, change: Change::Renamed(to) }
    }

    /// A made-up registry: `a.new` with `size`, its port `in`.
    fn current(s: Subject<'_>) -> bool {
        matches!(s, Subject::Type("a.new") | Subject::Param("a.new", "size") | Subject::Port("a.new", "in"))
    }

    #[test]
    fn renames_are_followed_and_removals_say_why() {
        let table = [
            renamed(Subject::Type("a.old"), "a.mid"),
            renamed(Subject::Type("a.mid"), "a.new"),
            renamed(Subject::Param("a.new", "radius"), "size"),
            renamed(Subject::Port("a.new", "src"), "in"),
            Migration { old: Subject::Type("a.gone"), change: Change::Removed("folded into a.new") },
        ];
        let r = |s| resolve_in(&table, s, &current);
        assert_eq!(r(Subject::Type("a.new")), Resolved::Current("a.new".into()));
        assert_eq!(r(Subject::Type("a.old")), Resolved::Current("a.new".into()), "two renames in a row");
        assert_eq!(r(Subject::Param("a.new", "radius")), Resolved::Current("size".into()));
        assert_eq!(r(Subject::Port("a.new", "src")), Resolved::Current("in".into()));
        assert_eq!(r(Subject::Type("a.gone")), Resolved::Removed("folded into a.new"));
        assert_eq!(r(Subject::Type("b.never")), Resolved::Unknown);
        assert_eq!(r(Subject::Param("a.new", "radiu")), Resolved::Unknown);
    }

    /// A table from someone else's build could loop: it is refused, never
    /// followed for ever.
    #[test]
    fn a_rename_loop_is_refused() {
        let table = [renamed(Subject::Type("x"), "y"), renamed(Subject::Type("y"), "x")];
        assert_eq!(resolve_in(&table, Subject::Type("x"), &current), Resolved::Loops);
        let selfish = [renamed(Subject::Param("a.new", "k"), "k")];
        assert_eq!(resolve_in(&selfish, Subject::Param("a.new", "k"), &current), Resolved::Loops);
    }

    #[test]
    fn transition_ids_are_their_keys_in_snake_case() {
        assert_eq!(snake("CrossDissolve"), "cross_dissolve");
        assert_eq!(snake("Iris"), "iris");
        assert!(is_current(Subject::Type("klipp.transition.cross_dissolve")));
        assert!(is_current(Subject::Type("klipp.fx.gaussian_blur")));
        assert!(is_current(Subject::Param("klipp.fx.gaussian_blur", "radius")));
        assert!(is_current(Subject::Type("klipp.grade.qualifier")));
    }
}
