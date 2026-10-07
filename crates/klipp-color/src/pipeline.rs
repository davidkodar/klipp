use crate::lut::{self, LutData};
use std::path::{Path, PathBuf};

/// The project-wide global LUT, applied after every clip's grade.
///
/// This is state, not a processing step: the engine owns one, the UI's
/// global-LUT commands write it, and the engine hands the loaded LUT to
/// `klipp_compose`, which applies it in the renderer's linear working
/// colourspace along with everything else.
///
/// Disabled by default — no LUT loaded, nothing applied.
pub struct ColorPipeline {
    pub enabled: bool,
    pub global_lut: Option<LutData>,
    pub global_lut_path: Option<PathBuf>,
    pub global_lut_opacity: f32,
    /// Use tetrahedral interpolation for LUT application (more accurate but slightly slower).
    pub use_tetrahedral: bool,
}

impl Default for ColorPipeline {
    fn default() -> Self {
        Self {
            enabled: false,
            global_lut: None,
            global_lut_path: None,
            global_lut_opacity: 1.0,
            use_tetrahedral: true,
        }
    }
}

impl ColorPipeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a LUT file (.cube, .3dl, .csp) as the global LUT.
    pub fn load_global_lut(&mut self, path: &Path) -> Result<(), lut::LutError> {
        let lut_data = lut::load_lut(path)?;
        self.global_lut = Some(lut_data);
        self.global_lut_path = Some(path.to_path_buf());
        self.enabled = true;
        Ok(())
    }

    /// Clear the global LUT.
    pub fn clear_global_lut(&mut self) {
        self.global_lut = None;
        self.global_lut_path = None;
    }

    /// Whether the pipeline changes the picture: switched on with a LUT
    /// loaded. On with none loaded it applies nothing, so nothing that
    /// reads it should treat it as there.
    pub fn applies(&self) -> bool {
        self.enabled && self.global_lut.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_a_lut_enables_the_pipeline() {
        // A fresh holder carries no LUT and is off, so a frame renders
        // ungraded until the project actually names one.
        let mut pipeline = ColorPipeline::new();
        assert!(!pipeline.enabled);
        assert!(pipeline.global_lut.is_none());

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir()
            .join(format!("klipp_global_lut_{}_{nanos}.cube", std::process::id()));
        lut::write_cube(&path, &[[0.0; 3]; 8], 2, "holder-test")
            .expect("write_cube should succeed");

        let loaded = pipeline.load_global_lut(&path);
        let _ = std::fs::remove_file(&path);
        loaded.expect("load_global_lut should accept our own .cube");

        assert!(pipeline.enabled, "loading a LUT turns the pipeline on");
        assert!(pipeline.applies(), "on, with a LUT");
        assert_eq!(pipeline.global_lut.as_ref().map(|l| l.size), Some(2));
        assert_eq!(pipeline.global_lut_path.as_deref(), Some(path.as_path()));

        pipeline.clear_global_lut();
        assert!(pipeline.global_lut.is_none());
        assert!(pipeline.global_lut_path.is_none());
        assert!(pipeline.enabled && !pipeline.applies(), "on, with no LUT: it changes nothing");
    }
}
