//! An aspect version still waiting to be reframed is still waiting when
//! the project is opened again (#2231), and a sequence that is not
//! writes nothing for it.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

#[test]
fn a_version_waiting_to_be_reframed_waits_again_when_reopened() {
    let dir = std::env::temp_dir().join(format!("klipp_awaiting_refit_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cut = Sequence::new_default();
    let mut version = Sequence::new_default();
    version.awaiting_refit = true;
    save_project(&dir, &[cut, version], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(written.matches("\"awaiting_refit\"").count(), 1, "only the waiting version writes it");
    let (sequences, ..) = load_project(&dir).unwrap();
    assert_eq!(sequences.iter().map(|s| s.awaiting_refit).collect::<Vec<_>>(), vec![false, true]);
    let _ = std::fs::remove_dir_all(&dir);
}
