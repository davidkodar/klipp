//! Find by Picture (#2212): the search run off the UI thread, and what it
//! found handed to the window.

use super::*;

/// Stretches across `files` (each a media item and its stored picture
/// samples) that look like `wanted`, best first, at most `max`.
pub(crate) fn search_files(files: &[(klipp_core::MediaId, f64, klipp_analysis::visual::Visual)], wanted: &[f32], min: f32, max: usize) -> Vec<(klipp_core::MediaId, f64, f64, f64, f32)> {
    let mut hits: Vec<(klipp_core::MediaId, f64, f64, f64, f32)> = files
        .iter()
        .flat_map(|(id, length, v)| v.search(wanted, min).into_iter().map(move |h| (*id, h.start_secs, if *length > 0.0 { h.end_secs.min(*length) } else { h.end_secs }, h.best_secs, h.score)))
        .collect();
    hits.sort_by(|a, b| b.4.total_cmp(&a.4));
    hits.truncate(max);
    hits
}

impl KlippApp {
    pub(super) fn handle_visual_search_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::OpenVisualSearch => {
                if self.state.visual_search.is_none() {
                    self.state.visual_search = Some(Default::default());
                }
                true
            }
            AppCommand::RunVisualSearch { query } => {
                let files: Vec<(klipp_core::MediaId, std::path::PathBuf, f64)> = self.state.media_pool.all().map(|m| (m.id, m.path.clone(), m.duration.to_secs_f64())).collect();
                let query = query.clone();
                crate::file_dialog::spawn(move || {
                    let hits = klipp_analysis::visual::query(&query).and_then(|wanted| {
                        let stored: Vec<_> = files.into_iter().filter_map(|(id, path, length)| klipp_analysis::visual::load(&path).map(|v| (id, length, v))).collect();
                        if stored.is_empty() {
                            return Err("No footage has been looked at for picture search yet: download the Picture Search model in AI Models, then analyze the media.".into());
                        }
                        Ok(search_files(&stored, &wanted, 0.2, 40))
                    });
                    vec![AppCommand::VisualSearchFound { query, hits }]
                });
                true
            }
            AppCommand::VisualSearchFound { query, hits } => {
                crate::visual_search_dialog::take_result(&mut self.state, query, hits.clone());
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_analysis::visual::{quantise, Sample, Visual};

    /// Hits across files come best first, cut to the most asked for, and
    /// never run past their file's end.
    #[test]
    fn hits_across_files_come_best_first() {
        let (a, b) = (klipp_core::MediaId::new(), klipp_core::MediaId::new());
        let at = |secs: f64, e: &[f32]| Sample { secs, e: quantise(e) };
        let near = [0.98, 0.2, 0.0];
        let exact = [1.0, 0.0, 0.0];
        let files = vec![
            (a, 10.0, Visual { every_secs: 2.0, samples: vec![at(1.0, &near), at(9.0, &exact)] }),
            (b, 0.0, Visual { every_secs: 2.0, samples: vec![at(3.0, &near)] }),
        ];
        let hits = search_files(&files, &[1.0, 0.0, 0.0], 0.5, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].0, hits[0].1, hits[0].2), (a, 9.0, 10.0), "the best, held inside the file");
        assert!(hits[0].4 > hits[1].4);
    }
}
