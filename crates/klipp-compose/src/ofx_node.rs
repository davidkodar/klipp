//! The renderer's OFX host as an OFX node runs it (M4 step 3): each node's
//! plug-in instance its own, made at its clip's frame rate, rendered with
//! only the frames its node read, and destroyed when its slot drops it.
//!
//! The host is shared (`Arc<Mutex<..>>`): the renderer holds it, and each
//! instance keeps it alive, so the host never goes before its instances.
//! A slot drops its instance outside any lock of the host's (the
//! executor's `end_frame`, a replaced slot), as `OfxPluginInstance`'s drop
//! takes it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use klipp_core::OfxValue;
use klipp_nodes::frame_context::{EncodedFrame, NEIGHBOUR_CEILING};
use klipp_nodes::plugin::{OfxDescription, OfxInstance, OfxNodeHost, Refusal};
use klipp_plugin::{OfxHost, OfxPluginInstance, PluginError};

/// The renderer's host, lent to a frame's OFX nodes.
pub(crate) struct OfxNodes(pub(crate) Arc<Mutex<OfxHost>>);

/// What a host failure means to the node: a plug-in not there, or one
/// whose action failed (`kOfxStatFailed`).
fn refusal(e: PluginError) -> Refusal {
    match e {
        PluginError::NotFound | PluginError::LoadFailed(_) | PluginError::MissingSymbol(_) => Refusal::Missing,
        PluginError::ActionFailed(why) => {
            tracing::warn!("OFX: {why}");
            Refusal::Failed(1)
        }
    }
}

impl OfxNodeHost for OfxNodes {
    fn describe(&self, plugin: &str) -> Result<OfxDescription, Refusal> {
        let d = self.0.lock().unwrap_or_else(|e| e.into_inner()).describe(plugin).ok_or(Refusal::Missing)?;
        Ok(OfxDescription {
            filter: d.filter,
            // A temporal plug-in may ask for any frames: the most a node
            // may read, and the host names at most the nearest it asked.
            budget: if d.temporal { NEIGHBOUR_CEILING } else { 0 },
            sequential_render: d.sequential_render,
            unsafe_threads: d.unsafe_threads,
        })
    }

    fn instance(&self, plugin: &str, fps: f64) -> Result<Box<dyn OfxInstance>, Refusal> {
        OfxHost::new_instance(&self.0, plugin, fps).map(|i| Box::new(Instance(i)) as Box<dyn OfxInstance>).map_err(refusal)
    }
}

/// One node's plug-in instance.
struct Instance(OfxPluginInstance);

impl OfxInstance for Instance {
    fn frames_needed(&mut self, params: &HashMap<String, OfxValue>, time: f64) -> Vec<f64> {
        self.0.frames_needed(params, time)
    }

    fn render(&mut self, params: &HashMap<String, OfxValue>, rgba8: &[u8], width: u32, height: u32, time: f64, render_scale: f64, frames: &[(f64, EncodedFrame)]) -> Result<Vec<u8>, Refusal> {
        let frames = frames.iter().map(|(t, f)| (*t, (f.rgba8.clone(), f.width, f.height))).collect();
        self.0.render(params, rgba8, width, height, time, render_scale, frames).map_err(refusal)
    }

    fn bytes(&self) -> usize {
        self.0.bytes()
    }
}
