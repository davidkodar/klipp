//! The stable ids (#2888): every id ever declared stays readable. An id in
//! `registry_ids.txt` that is no longer declared needs an entry in
//! `registry_ids::MIGRATIONS`; an entry has to lead to an id that is
//! declared now, or say why its id was removed. New ids are added to the
//! file with `KLIPP_BLESS_REGISTRY_IDS=1`.

use klipp_timeline::registry_ids::{self, Change, Resolved, Subject};

const FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/registry_ids.txt");

/// A snapshot line as the id it names.
fn subject(line: &str) -> Subject<'_> {
    match line.split(' ').collect::<Vec<_>>()[..] {
        ["type", id] => Subject::Type(id),
        ["param", id, key] => Subject::Param(id, key),
        ["port", id, key] => Subject::Port(id, key),
        _ => panic!("not a line of the id snapshot: {line:?}"),
    }
}

#[test]
fn every_id_ever_declared_still_reads() {
    let now = registry_ids::snapshot();
    let kept = std::fs::read_to_string(FILE).unwrap_or_default();
    let kept: Vec<&str> = kept.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    for line in &kept {
        match registry_ids::resolve(subject(line)) {
            Resolved::Current(_) | Resolved::Removed(_) => {}
            other => panic!("{line}: {other:?}: a declared id went without an entry in registry_ids::MIGRATIONS"),
        }
    }
    let new: Vec<&String> = now.iter().filter(|l| !kept.contains(&l.as_str())).collect();
    if std::env::var_os("KLIPP_BLESS_REGISTRY_IDS").is_some() {
        let mut all: Vec<String> = kept.iter().map(|l| l.to_string()).chain(new.iter().map(|l| l.to_string())).collect();
        all.sort();
        all.dedup();
        let head = "# Every effect, node and transition id, parameter and port ever declared (#2888).\n# Added to with KLIPP_BLESS_REGISTRY_IDS=1; a line is never removed.\n";
        std::fs::write(FILE, format!("{head}{}\n", all.join("\n"))).unwrap();
        return;
    }
    assert!(new.is_empty(), "{} new ids, not yet in tests/registry_ids.txt (add them with KLIPP_BLESS_REGISTRY_IDS=1): {:?}", new.len(), &new[..new.len().min(10)]);
}

/// Each entry leads somewhere: a rename to an id declared now (the right
/// kind, the parameter or port on its type), a removal with a reason, and
/// no entry for an id that is still declared.
#[test]
fn every_migration_leads_to_a_current_id() {
    for m in registry_ids::MIGRATIONS {
        assert!(!registry_ids::is_current(m.old), "{:?} is still declared: its entry would never be read", m.old);
        match (m.change, registry_ids::resolve(m.old)) {
            (Change::Renamed(_), Resolved::Current(_)) => {}
            (Change::Removed(why), Resolved::Removed(_)) => assert!(!why.trim().is_empty(), "{:?}: removed without a reason", m.old),
            (_, other) => panic!("{:?}: {other:?}", m.old),
        }
    }
}

/// The file holds what is declared now, at the least: a change that
/// lost the declared list (an empty registry) can't pass as no change.
#[test]
fn the_snapshot_covers_every_kind() {
    let now = registry_ids::snapshot();
    for prefix in ["type klipp.fx.", "type klipp.grade.", "type klipp.transition.", "param klipp.fx.", "param klipp.grade.", "param klipp.transition.", "port klipp.fx."] {
        assert!(now.iter().any(|l| l.starts_with(prefix)), "no {prefix}");
    }
}
