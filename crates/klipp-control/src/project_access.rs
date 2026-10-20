//! Recent-project lookup is bounded to the UI's published preferences.
//! Names are literal strings, never paths, glob patterns or shell input.
use klipp_core::{ProjectStatus, RecentProjectSummary};
use std::path::PathBuf;

pub(crate) fn recent(project: &ProjectStatus) -> Vec<RecentProjectSummary> {
    let mut seen = std::collections::HashSet::new();
    project
        .recent_projects
        .iter()
        .filter(|p| {
            p.path.is_absolute()
                && seen.insert(p.path.clone())
                && (p.path.is_dir() && p.path.join("project.json").is_file()
                    || p.path.is_file()
                        && (p.path.extension().is_some_and(|e| e == "klipp")
                            || p.path.file_name().is_some_and(|n| n == "project.json")))
        })
        .take(20)
        .cloned()
        .collect()
}

pub(crate) fn status_recents(project: &ProjectStatus) -> Vec<serde_json::Value> {
    if project.path.is_some() {
        return Vec::new();
    }
    recent(project)
        .into_iter()
        .take(5)
        .map(|p| serde_json::json!({"name":p.name,"path":p.path}))
        .collect()
}

pub(crate) fn resolve(
    project: &ProjectStatus,
    path: Option<&str>,
    name: Option<&str>,
) -> Result<PathBuf, String> {
    match (path, name) {
        (Some(path), None) => {
            let p = PathBuf::from(path);
            if !p.is_absolute() {
                return Err("project path must be absolute".into());
            }
            if !p.exists() {
                return Err(format!("no project at {path:?}"));
            }
            Ok(p)
        }
        (None, Some(name)) => {
            let name = name.trim();
            if name.is_empty() || name.len() > 512 {
                return Err(
                    "project name must contain 1–512 bytes; use recent_projects to choose one"
                        .into(),
                );
            }
            let needle = name.to_lowercase();
            let recent = recent(project);
            let exact: Vec<_> = recent
                .iter()
                .filter(|p| p.name.to_lowercase() == needle)
                .collect();
            let matches = if exact.is_empty() {
                recent
                    .iter()
                    .filter(|p| p.name.to_lowercase().contains(&needle))
                    .collect()
            } else {
                exact
            };
            match matches.as_slice() {
                [one] => Ok(one.path.clone()),
                [] => Err("no existing recent project matches that name; use recent_projects to see available projects".into()),
                _ => Err(format!("ambiguous project name; nothing opened. Choose a path from these candidates: {}", serde_json::to_string(&matches.iter().map(|p|serde_json::json!({"name":p.name,"path":p.path})).collect::<Vec<_>>()).unwrap())),
            }
        }
        _ => Err("give exactly one of path or name".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(root: &std::path::Path, name: &str) -> RecentProjectSummary {
        let path = root.join(format!("{name}.klipp"));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("project.json"), "{}").unwrap();
        RecentProjectSummary {
            path,
            name: name.into(),
            last_saved: None,
            duration_secs: None,
            fps: None,
            resolution: None,
            missing_media: None,
        }
    }
    #[test]
    fn project_access_matches_only_existing_recent_names() {
        let root = std::env::temp_dir().join(format!("klipp-recents-{}", uuid::Uuid::new_v4()));
        let mut p = ProjectStatus::default();
        p.recent_projects = vec![
            entry(&root, "Film"),
            entry(&root, "Film final"),
            entry(&root, "Interview"),
        ];
        assert_eq!(
            resolve(&p, None, Some("fILm")).unwrap(),
            p.recent_projects[0].path
        );
        assert_eq!(
            resolve(&p, None, Some("terview")).unwrap(),
            p.recent_projects[2].path
        );
        let error = resolve(&p, None, Some("fil")).unwrap_err();
        assert!(
            error.contains("ambiguous") && error.contains("Film final") && error.contains(".klipp")
        );
        for name in ["unknown", "../Film", "*", "$(touch /tmp/unwanted)", ""] {
            assert!(resolve(&p, None, Some(name)).is_err());
        }
        assert!(resolve(&p, Some("/tmp/anything"), Some("Film")).is_err());
        std::fs::remove_dir_all(&p.recent_projects[2].path).unwrap();
        assert!(resolve(&p, None, Some("Interview")).is_err());
        // A real project outside the recorded list is never discovered.
        entry(&root, "Unlisted");
        assert!(resolve(&p, None, Some("Unlisted")).is_err());
        assert_eq!(recent(&p).len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn project_access_status_recents_are_bounded_and_only_on_project_page() {
        let root = std::env::temp_dir().join(format!("klipp-recents-{}", uuid::Uuid::new_v4()));
        let mut p = ProjectStatus::default();
        p.recent_projects = (0..7).map(|i| entry(&root, &format!("Film{i}"))).collect();
        let shown = status_recents(&p);
        assert_eq!(shown.len(), 5);
        assert_eq!(shown[0]["name"], "Film0");
        assert_eq!(shown[0].as_object().unwrap().len(), 2);
        p.path = Some(p.recent_projects[0].path.clone());
        assert!(status_recents(&p).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
