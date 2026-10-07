//! A clip's stored picture naming a bundled LUT by an old absolute path is
//! kept on load (#1270): its fields read the path against this machine's
//! LUT library and a lift of them names it `bundled:<name>`, and the
//! stored picture is read the same way.

use klipp_core::{MediaId, RationalTime};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, GradeAccess, Sequence};

const LUT: &str = "fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube";

fn graded(lut: std::path::PathBuf) -> klipp_nodes::NodeGraph {
    let mut g = klipp_nodes::NodeGraph::default();
    let c = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == c).unwrap().params.lut_path = Some(lut);
    g
}

#[test]
fn a_stored_picture_with_an_old_absolute_lut_path_is_kept() {
    let Some(lib) = klipp_core::assets::bundled_luts_dir() else { return };
    let dir = std::env::temp_dir().join(format!("klipp_stored_lut_path_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let t = |n| RationalTime::from_frames(n, 24);
    let mut seq = Sequence::new_default();
    let mut clip = Clip::media(MediaId::new(), t(0), t(96), t(0), t(96));
    clip.set_grade(graded(lib.join(LUT)));
    clip.edit_grade_versions(|v| v.push(klipp_timeline::GradeVersion { name: "B".into(), node_graph: klipp_timeline::Grade::new(graded(lib.join(LUT))) }));
    seq.tracks[0].clips.push(clip);
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();

    // As a file saved before the name was portable holds it: the path into
    // another checkout's library, in the picture and the version's.
    let file = dir.join("project.json");
    let text = std::fs::read_to_string(&file).unwrap();
    let bundled = format!("\"bundled:{LUT}\"");
    assert!(text.matches(&bundled).count() >= 2, "the picture and the version name it bundled");
    std::fs::write(&file, text.replace(&bundled, &format!("\"/elsewhere/Klipp/assets/luts/{LUT}\""))).unwrap();

    // A debug build stops on a stored picture it can't keep.
    let (seqs, ..) = load_project(&dir).unwrap();
    let clip = &seqs[0].tracks[0].clips[0];
    assert!(clip.has_lifted_picture(), "the stored picture is kept");
    let _ = std::fs::remove_dir_all(&dir);
}
