use crate::ffi;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug)]
pub enum PluginError {
    LoadFailed(String),
    MissingSymbol(&'static str),
    NotFound,
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadFailed(e) => write!(f, "failed to load: {e}"),
            Self::MissingSymbol(s) => write!(f, "missing symbol: {s}"),
            Self::NotFound => write!(f, "plugin not found"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OfxCategory {
    Filter,
    Color,
    Transition,
    Generator,
    Other,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum OfxValue {
    Double(f64),
    Int(i32),
    Bool(bool),
    String(String),
}

/// Info about a discovered OFX plugin (safe to send to UI).
#[derive(Debug, Clone)]
pub struct OfxPluginInfo {
    pub name: String,
    pub version: (u32, u32),
    pub category: OfxCategory,
}

/// A loaded OFX plugin. Holds the library handle to keep it alive.
pub struct OfxPlugin {
    pub name: String,
    pub version: (u32, u32),
    pub parameters: Vec<String>,
    pub category: OfxCategory,
    pub description: String,
    _lib: Arc<libloading::Library>,
}

impl std::fmt::Debug for OfxPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfxPlugin")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("category", &self.category)
            .finish()
    }
}

impl OfxPlugin {
    pub fn info(&self) -> OfxPluginInfo {
        OfxPluginInfo {
            name: self.name.clone(),
            version: self.version,
            category: self.category,
        }
    }
}

/// OFX plugin host — discovers and loads .ofx.bundle plugins.
pub struct OfxHost {
    plugins: Vec<OfxPlugin>,
    search_paths: Vec<PathBuf>,
}

impl Default for OfxHost {
    fn default() -> Self {
        Self::new()
    }
}

impl OfxHost {
    pub fn new() -> Self {
        let mut search_paths = vec![
            PathBuf::from("/usr/OFX/Plugins"),
            PathBuf::from("/usr/local/OFX/Plugins"),
        ];
        if let Some(home) = dirs::home_dir() {
            search_paths.push(home.join(".OFX/Plugins"));
        }
        Self {
            plugins: Vec::new(),
            search_paths,
        }
    }

    /// Scan search paths and load all .ofx.bundle plugins.
    /// Bundle layout: `<path>/<name>.ofx.bundle/Contents/Linux-x86-64/<name>.ofx`
    /// Also checks `OFX_PLUGIN_PATH` env var (semicolon-separated).
    pub fn discover_plugins(&mut self) -> usize {
        let mut paths = self.search_paths.clone();
        if let Ok(env_path) = std::env::var("OFX_PLUGIN_PATH") {
            for p in env_path.split(';') {
                paths.insert(0, PathBuf::from(p));
            }
        }

        let mut count = 0;
        for search_path in &paths {
            let Ok(entries) = std::fs::read_dir(search_path) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if path.is_dir() && name.ends_with(".ofx.bundle") {
                    let stem = name.trim_end_matches(".ofx.bundle");
                    let binary = path
                        .join("Contents")
                        .join("Linux-x86-64")
                        .join(format!("{stem}.ofx"));
                    if binary.exists() {
                        match self.load_plugin(&binary) {
                            Ok(plugins) => {
                                for p in plugins {
                                    info!(
                                        "OFX: loaded '{}' v{}.{}",
                                        p.name, p.version.0, p.version.1
                                    );
                                    self.plugins.push(p);
                                    count += 1;
                                }
                            }
                            Err(e) => warn!("OFX: failed to load {}: {e}", binary.display()),
                        }
                    }
                }
            }
        }
        info!("OFX: discovered {count} plugin(s) total");
        count
    }

    /// Load an OFX binary (.ofx = ELF .so) and extract plugin identifiers.
    fn load_plugin(&self, binary: &Path) -> Result<Vec<OfxPlugin>, PluginError> {
        // Safety: loading an untrusted shared library.
        let lib = Arc::new(
            unsafe { libloading::Library::new(binary) }
                .map_err(|e| PluginError::LoadFailed(e.to_string()))?,
        );

        let get_n: libloading::Symbol<ffi::OfxGetNumberOfPluginsFn> = unsafe {
            lib.get(b"OfxGetNumberOfPlugins\0")
        }
        .map_err(|_| PluginError::MissingSymbol("OfxGetNumberOfPlugins"))?;

        let get_plugin: libloading::Symbol<ffi::OfxGetPluginFn> =
            unsafe { lib.get(b"OfxGetPlugin\0") }
                .map_err(|_| PluginError::MissingSymbol("OfxGetPlugin"))?;

        let n = unsafe { get_n() };
        let mut result = Vec::new();

        for i in 0..n {
            let raw = unsafe { get_plugin(i) };
            if raw.is_null() {
                continue;
            }
            let raw = unsafe { &*raw };

            // Verify this is an image effect plugin
            let api = unsafe { std::ffi::CStr::from_ptr(raw.plugin_api) }.to_string_lossy();
            if api != "OfxImageEffectPluginAPI" {
                continue;
            }

            let name = unsafe { std::ffi::CStr::from_ptr(raw.plugin_identifier) }
                .to_string_lossy()
                .to_string();

            result.push(OfxPlugin {
                name,
                version: (raw.plugin_version_major, raw.plugin_version_minor),
                parameters: Vec::new(),
                category: OfxCategory::Filter,
                description: String::new(),
                _lib: lib.clone(),
            });
        }

        Ok(result)
    }

    /// Apply a clip's full effects chain to an RGBA frame.
    /// Returns the original bytes unchanged if the chain is empty.
    pub fn apply_effects_chain(
        &self,
        effects: &[AppliedEffect],
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        if effects.is_empty() || effects.iter().all(|e| !e.enabled) {
            return rgba.to_vec();
        }
        let mut current = rgba.to_vec();
        for effect in effects {
            if !effect.enabled {
                continue;
            }
            match self.apply_effect(&effect.plugin_name, &effect.param_values, &current, width, height)
            {
                Ok(result) => current = result,
                Err(e) => warn!("OFX: apply_effect '{}' failed: {e}", effect.plugin_name),
            }
        }
        current
    }

    /// Apply a single effect. Phase 6A: stub — returns input unchanged.
    fn apply_effect(
        &self,
        plugin_name: &str,
        _param_values: &HashMap<String, OfxValue>,
        input: &[u8],
        _width: u32,
        _height: u32,
    ) -> Result<Vec<u8>, PluginError> {
        let _plugin = self
            .plugins
            .iter()
            .find(|p| p.name == plugin_name)
            .ok_or(PluginError::NotFound)?;
        // Phase 6A: passthrough. Phase 6B: call kOfxImageEffectActionRender.
        Ok(input.to_vec())
    }

    pub fn plugins(&self) -> &[OfxPlugin] {
        &self.plugins
    }

    pub fn plugin_infos(&self) -> Vec<OfxPluginInfo> {
        self.plugins.iter().map(|p| p.info()).collect()
    }
}

/// An effect applied to a clip, with its parameter values.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppliedEffect {
    pub plugin_name: String,
    pub enabled: bool,
    pub param_values: HashMap<String, OfxValue>,
}
