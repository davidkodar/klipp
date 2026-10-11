//! `Saved`'s helper: a sequence through a real save and a strict reload,
//! in a folder of its own, the clips as the reloaded project holds them.
//! A case compares what it authored on a clip with the clip it gets back.

use klipp_media::MediaPool;
use klipp_timeline::Sequence;

/// `sequence`, with its media in `pool`, saved (`save_project`) and loaded
/// back strictly (`load_project_strict`): the reloaded sequence.
pub fn round_trip(sequence: &Sequence, pool: &MediaPool) -> Result<Sequence, String> {
    let dir = std::env::temp_dir().join(format!("klipp-readiness-saved-{}-{}", std::process::id(), uuid_like()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let result = (|| {
        klipp_project::save_project(&dir, std::slice::from_ref(sequence), pool)?;
        let (sequences, ..) = klipp_project::load_project_strict(&dir)?;
        sequences.into_iter().next().ok_or_else(|| "the reloaded project has no sequence".to_owned())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A name no other round trip of this process uses.
fn uuid_like() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
