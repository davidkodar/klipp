//! Heavy nodes' outputs held for the parked frame (#1863).
//!
//! A slider step on a parked frame runs the clip's grade again from the
//! source. Above the node being edited nothing changed, and a heavy node
//! there (temporal noise reduction, a frame blend, an OFX or frei0r
//! plug-in) costs tens to hundreds of milliseconds at 4K for the same
//! picture every step. The executor keeps such a node's output, keyed by
//! everything the output is a function of, and the next run takes it
//! instead of running the node and the nodes above it.
//!
//! Nothing is held without a [`HeldSource`]: only a viewer parked on a
//! frame makes one, so playback and export never read or fill the cache.
//! There is no invalidation besides the key: an edit above the node, a
//! moved time, another file, retime or render setting is another key.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use klipp_core::{ClipId, GradeEncoding, LogFormat, MediaId, WorkingColorspace};
use serde::Serialize;

use crate::decline::Decline;
use crate::graph::{Node, NodeGraph, NodeId};
use crate::kinds::NodeKind;

/// The cache's size when nothing else is set: three 4K frames.
pub const DEFAULT_BUDGET: usize = 512 << 20;

/// Entries the budget always admits, whatever the frame's size: a 4K
/// denoise and plug-in pair, or one of them at 8K, still hold.
pub const MIN_ENTRIES: usize = 2;

/// Bumped when the key's layout changes.
const KEY_VERSION: u32 = 1;

/// How the clip's media is timed at the frame: what a frame blend's
/// partner and weight, and a denoiser's neighbours, depend on besides the
/// decoded frame.
#[derive(Clone, Copy, PartialEq, Debug, Serialize)]
pub struct Retime {
    /// The clip's speed at the frame (1 = as shot).
    pub speed: f64,
    pub reverse: bool,
    /// The source time the clip shows at this clip time: a varispeed's
    /// sample, which a constant speed predicts.
    pub source_at_clip_time: f64,
    /// The media's own rate, when known.
    pub media_fps: Option<f64>,
    /// The rate the media is conformed to, when it is.
    pub conform_fps: Option<f64>,
}

/// What the parked frame is the picture of, as the render that read it
/// saw it. Made only by a viewer parked on a frame.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub struct HeldSource {
    /// The clip whose grade run it is for: another clip's run, a layer
    /// above or a thumbnail's, never takes it.
    pub clip: ClipId,
    pub media: MediaId,
    /// The file decoded: the proxy or the original.
    pub path: PathBuf,
    /// Its length and modification time when read: a file replaced at
    /// the same path is another picture.
    pub file_len: u64,
    pub file_modified: SystemTime,
    pub log_format: Option<LogFormat>,
    /// The decoded frame's own presentation time, in microseconds: the
    /// frame shown, not the time asked for, so a frame still from before
    /// a seek is never stored as the parked one.
    pub pts_micros: i64,
    /// The decoded frame's size before it was fitted to the render.
    pub decoded: (u32, u32),
    pub retime: Retime,
    /// The proxy the frame is shown from, when it is: its path, length
    /// and modification time. The neighbours a denoiser reads are the
    /// original's (`path`), so both files are the picture.
    pub proxy: Option<(PathBuf, u64, SystemTime)>,
}

/// What else a run's output depends on.
#[derive(Clone, PartialEq, Debug, Serialize)]
pub(crate) struct RunEnv {
    pub width: u32,
    pub height: u32,
    pub clip_time_secs: f64,
    /// The source time the frame is rendered at, exactly: a blend's
    /// weight is read from it.
    pub source_secs: f64,
    pub frame_number: u64,
    pub working: WorkingColorspace,
    pub encoding: GradeEncoding,
    pub lut_tetrahedral: bool,
    pub render_scale: f32,
    /// Whether the executor has a GPU: nodes above the held one ran on
    /// its chain or on the CPU, which agree only within a tolerance.
    pub gpu: bool,
    /// Whether this run's Source was seeded on the GPU rather than given
    /// on the CPU (see [`source_read`]).
    pub seeded: bool,
    /// The colour domain of the picture the Source reads (#1867).
    pub source_domain: String,
}

/// Why a heavy node isn't held this run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The node, or one above it, is looked at: tapped, soloed,
    /// previewed as a matte or thumbnailed.
    LookedAt,
    /// A node above it reads something besides its parameters: a file, an
    /// analysis, a matte.
    Impure,
    /// It keeps history between frames.
    History,
    /// It is the graph's result: nothing below it to edit.
    Result,
    /// Its key couldn't be written.
    Unkeyable,
}

/// One held output.
struct Held {
    key: Vec<u8>,
    pixels: Arc<Vec<f32>>,
    /// The node's own declines on the run that made it, replayed on a
    /// hit so a hit says what the run would have.
    declines: Vec<Decline>,
    /// The same pixels on the GPU, once a GPU run took them (#1863, step
    /// B3): a chain then starts from them with a copy on the device, not
    /// a 133 MB upload at 4K. Counted against the GPU's own budget.
    gpu: Option<Arc<wgpu::Buffer>>,
}

impl Held {
    fn bytes(&self) -> usize {
        self.pixels.len() * std::mem::size_of::<f32>()
    }
}

/// The run's counts: lookups that found an entry, that didn't, nodes not
/// held and why, and entries stored.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct HeldCounts {
    pub hits: u64,
    pub misses: u64,
    pub refused: u64,
    pub stored: u64,
}

/// The executor's held outputs, least recently used first.
pub struct HeldOutputs {
    entries: VecDeque<Held>,
    budget: usize,
    /// The parked frame's source, for runs of its clip, until the render
    /// takes it away (`GraphExecutor::hold_outputs_for(None)`).
    pub(crate) token: Option<HeldSource>,
    counts: HeldCounts,
    /// The budget of the entries' GPU copies, apart from the CPU one: on
    /// a card with its own memory they are two pools.
    gpu_budget: usize,
    /// The device the GPU copies live on: another device (one replaced)
    /// can't read them.
    gpu_device: Option<Arc<wgpu::Device>>,
}

impl Default for HeldOutputs {
    fn default() -> Self {
        Self { entries: VecDeque::new(), budget: DEFAULT_BUDGET, token: None, counts: HeldCounts::default(), gpu_budget: DEFAULT_BUDGET, gpu_device: None }
    }
}

/// What a run does with the cache: the heavy nodes it takes held, with
/// their declines; the nodes above them it doesn't run; the keys of the
/// heavy nodes it runs and then stores.
#[derive(Default)]
pub(crate) struct HeldRun {
    pub hits: HashMap<NodeId, (Arc<Vec<f32>>, Vec<Decline>)>,
    pub skip: HashSet<NodeId>,
    pub store: HashMap<NodeId, Vec<u8>>,
    /// The heavy nodes not held, by [`refusal`] or for want of a key.
    pub refused: Vec<NodeId>,
    /// The keys of the hits, for their GPU copies.
    pub keys: HashMap<NodeId, Vec<u8>>,
}

impl HeldOutputs {
    pub fn counts(&self) -> HeldCounts {
        self.counts
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The GPU copies' budget in bytes, apart from the CPU one's (#2266):
    /// set from the GPU's free memory by the viewer. Copies past it are
    /// dropped as the next is kept; the CPU pixels stay.
    pub fn set_gpu_budget(&mut self, bytes: usize) {
        self.gpu_budget = bytes;
    }

    /// The budget in bytes; the [`MIN_ENTRIES`] newest always stay.
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes;
        self.evict();
    }

    fn bytes(&self) -> usize {
        self.entries.iter().map(Held::bytes).sum()
    }

    /// Drop the least recently used past the budget, keeping at least
    /// [`MIN_ENTRIES`].
    fn evict(&mut self) {
        while self.entries.len() > MIN_ENTRIES && self.bytes() > self.budget {
            self.entries.pop_front();
        }
    }

    /// The entry under `key`, made the most recently used.
    fn get(&mut self, key: &[u8]) -> Option<(Arc<Vec<f32>>, Vec<Decline>)> {
        let i = self.entries.iter().position(|h| h.key == key)?;
        let held = self.entries.remove(i).expect("found above");
        let got = (Arc::clone(&held.pixels), held.declines.clone());
        self.entries.push_back(held);
        Some(got)
    }

    /// Whether an entry is under `key`, touching nothing.
    fn has(&self, key: &[u8]) -> bool {
        self.entries.iter().any(|h| h.key == key)
    }

    /// The GPU copy of the entry under `key` on `device`, if one was made
    /// there. Copies made on another device (the one before a
    /// replacement) are all forgotten.
    pub(crate) fn gpu_of(&mut self, key: &[u8], device: &Arc<wgpu::Device>) -> Option<Arc<wgpu::Buffer>> {
        if self.gpu_device.as_ref().is_some_and(|d| !Arc::ptr_eq(d, device)) {
            self.drop_gpu();
        }
        self.entries.iter().find(|h| h.key == key)?.gpu.clone()
    }

    /// Keep `buffer`, made on `device`, as the GPU copy of the entry under
    /// `key`, then drop the least recently used copies past the GPU
    /// budget, keeping at least [`MIN_ENTRIES`].
    pub(crate) fn set_gpu(&mut self, key: &[u8], buffer: Arc<wgpu::Buffer>, device: &Arc<wgpu::Device>) {
        self.gpu_device = Some(Arc::clone(device));
        if let Some(h) = self.entries.iter_mut().find(|h| h.key == key) {
            h.gpu = Some(buffer);
        }
        let gpu_bytes = |e: &VecDeque<Held>| e.iter().filter_map(|h| h.gpu.as_ref()).map(|b| b.size() as usize).sum::<usize>();
        while self.entries.iter().filter(|h| h.gpu.is_some()).count() > MIN_ENTRIES && gpu_bytes(&self.entries) > self.gpu_budget {
            if let Some(h) = self.entries.iter_mut().find(|h| h.gpu.is_some()) {
                h.gpu = None;
            }
        }
    }

    /// Forget every GPU copy: the device was lost or replaced, and its
    /// buffers with it. The CPU pixels stay.
    pub fn drop_gpu(&mut self) {
        for h in &mut self.entries {
            h.gpu = None;
        }
        self.gpu_device = None;
    }

    /// GPU copies held now.
    pub fn gpu_len(&self) -> usize {
        self.entries.iter().filter(|h| h.gpu.is_some()).count()
    }

    /// Hold `pixels`, a heavy node's output, under its `key`.
    pub(crate) fn put(&mut self, key: Vec<u8>, pixels: Arc<Vec<f32>>, declines: Vec<Decline>) {
        self.entries.retain(|h| h.key != key);
        self.entries.push_back(Held { key, pixels, declines, gpu: None });
        self.counts.stored += 1;
        self.evict();
    }

    /// The run's use of the cache for `graph` with the parked frame's
    /// `source`: each heavy node on the way to the output (`order` the
    /// run's order, `feeding` the nodes that reach the output) looked up
    /// by its key, unless [`refusal`] says no. The nodes above a hit run
    /// no more when every reader of theirs is a hit or itself not run.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn plan(&mut self, graph: &NodeGraph, order: &[NodeId], feeding: &HashSet<NodeId>, source: &HeldSource, env: &RunEnv, looked_at: &dyn Fn(NodeId) -> bool, history: &dyn Fn(&Node) -> bool) -> HeldRun {
        let mut run = HeldRun::default();
        for (id, found) in lookups(graph, order, feeding, source, env, looked_at, history) {
            match found {
                Err(_) => {
                    self.counts.refused += 1;
                    run.refused.push(id);
                }
                Ok(key) => match self.get(&key) {
                    Some(hit) => {
                        self.counts.hits += 1;
                        run.hits.insert(id, hit);
                        run.keys.insert(id, key);
                    }
                    None => {
                        self.counts.misses += 1;
                        run.store.insert(id, key);
                    }
                },
            }
        }
        run.skip = skippable(graph, order, feeding, &run.hits);
        run
    }

    /// Whether a run of `graph` with `source` would take every heavy node
    /// on the way to the output from the cache, and so needs no source
    /// frame for them: the same lookups and refusals as [`Self::plan`],
    /// counting and touching nothing.
    // The engine's parked GPU path asks it before it seeds from a held
    // output (#1863, step B3); until then only the tests do.
    #[allow(clippy::too_many_arguments, dead_code)]
    pub(crate) fn ready(&self, graph: &NodeGraph, order: &[NodeId], feeding: &HashSet<NodeId>, source: &HeldSource, env: &RunEnv, looked_at: &dyn Fn(NodeId) -> bool, history: &dyn Fn(&Node) -> bool) -> bool {
        let found = lookups(graph, order, feeding, source, env, looked_at, history);
        !found.is_empty() && found.iter().all(|(_, k)| k.as_ref().is_ok_and(|k| self.has(k)))
    }
}

/// Whether `node` is heavy: it runs on the CPU, and costs more than a
/// frame's copy.
pub(crate) fn is_heavy(node: &Node) -> bool {
    match node.kind {
        NodeKind::Ofx | NodeKind::Frei0r => true,
        NodeKind::Effect | NodeKind::EffectSegment => node.params.effects.iter().any(klipp_effects::registry::needs_source_neighbours),
        _ => false,
    }
}

/// Whether a node above a held one is a function of its parameters and
/// its input alone. Anything else (a LUT or matte file, a window, a key,
/// an analysis, a compound) keeps the node below it from being held.
fn pure(node: &Node) -> bool {
    is_heavy(node)
        || matches!(
            node.kind,
            NodeKind::Source
                | NodeKind::InputTransform
                | NodeKind::Reroute
                | NodeKind::Exposure
                | NodeKind::Contrast
                | NodeKind::Saturation
                | NodeKind::ToneControls
                | NodeKind::ColorWheels
                | NodeKind::WheelSatBri
                | NodeKind::RGBCurves
                | NodeKind::HueSatCurves
                | NodeKind::ColorBalance
                | NodeKind::HdrZones
        )
}

/// Whether a node changes nothing on the way from the Source: a heavy
/// node read the CPU source itself through it.
fn passes_through(node: &Node, t: f64) -> bool {
    matches!(node.kind, NodeKind::Source | NodeKind::InputTransform | NodeKind::Reroute) || !crate::executor::effective_enabled(node, t)
}

/// The nodes `of` reads, at any distance and through any port, wireless
/// links too.
pub(crate) fn ancestors(graph: &NodeGraph, of: NodeId) -> HashSet<NodeId> {
    let mut parents: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for (from, to) in graph.iter_dependencies() {
        parents.entry(to).or_default().push(from);
    }
    let mut seen = HashSet::new();
    let mut stack = vec![of];
    while let Some(n) = stack.pop() {
        for &p in parents.get(&n).into_iter().flatten() {
            if seen.insert(p) {
                stack.push(p);
            }
        }
    }
    seen
}

/// Why `id`, a heavy node, isn't held this run, if it isn't. One rule for
/// a run and for [`HeldOutputs::ready`], so the engine's choice of path
/// and the run can't disagree.
pub(crate) fn refusal(graph: &NodeGraph, id: NodeId, above: &HashSet<NodeId>, by_id: &HashMap<NodeId, &Node>, looked_at: &dyn Fn(NodeId) -> bool, history: &dyn Fn(&Node) -> bool) -> Option<Refusal> {
    let node = by_id.get(&id)?;
    if graph.input_of(graph.output_node_id, 0) == Some(id) {
        return Some(Refusal::Result);
    }
    if history(node) {
        return Some(Refusal::History);
    }
    if looked_at(id) || above.iter().any(|a| looked_at(*a)) {
        return Some(Refusal::LookedAt);
    }
    if !above.iter().all(|a| by_id.get(a).is_some_and(|n| pure(n))) {
        return Some(Refusal::Impure);
    }
    None
}

/// What the Source above a held node was, for the key. A heavy node
/// never reads a source left on the GPU: the run refuses
/// (`CpuSourceRequired`) and runs again with the CPU frame, and its own
/// output is on the CPU. So the seed is part of the picture only through
/// a node that ran on the chain from it: one reached from the Source
/// through passthroughs, not through a heavy node. With none, the held
/// node's input is the same however the run was seeded.
fn source_read(graph: &NodeGraph, above: &HashSet<NodeId>, by_id: &HashMap<NodeId, &Node>, env: &RunEnv) -> bool {
    if !env.seeded {
        return false;
    }
    let mut readers: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for (from, to) in graph.iter_dependencies() {
        readers.entry(from).or_default().push(to);
    }
    let t = env.clip_time_secs;
    let mut stack: Vec<NodeId> = above.iter().copied().filter(|a| by_id.get(a).is_some_and(|n| n.kind == NodeKind::Source)).collect();
    let mut seen: HashSet<NodeId> = stack.iter().copied().collect();
    while let Some(n) = stack.pop() {
        for r in readers.get(&n).into_iter().flatten().filter(|r| above.contains(r)) {
            let Some(node) = by_id.get(r) else { continue };
            if is_heavy(node) && crate::executor::effective_enabled(node, t) {
                continue;
            }
            if !passes_through(node, t) {
                return true;
            }
            if seen.insert(*r) {
                stack.push(*r);
            }
        }
    }
    false
}

/// The key of heavy node `id` with `above` its ancestors: every node from
/// the source to it (kind, on at the time, parameters at the time, its
/// wireless ports), the edges into them, the source and the run. Written
/// through `serde_json::Value`, whose maps are ordered, so parameters
/// holding a map give the same bytes every time.
pub(crate) fn key_of(graph: &NodeGraph, id: NodeId, above: &HashSet<NodeId>, by_id: &HashMap<NodeId, &Node>, source: &HeldSource, env: &RunEnv) -> Option<Vec<u8>> {
    #[derive(Serialize)]
    struct NodeRecord<'a> {
        id: NodeId,
        kind: NodeKind,
        on: bool,
        params: std::borrow::Cow<'a, crate::params::NodeParams>,
        input_links: &'a [Option<String>],
        output_link: &'a Option<String>,
    }
    #[derive(Serialize)]
    struct Key<'a> {
        version: u32,
        source: &'a HeldSource,
        env: RunEnv,
        nodes: Vec<NodeRecord<'a>>,
        edges: Vec<&'a crate::graph::Edge>,
    }
    let t = env.clip_time_secs;
    let mut ids: Vec<NodeId> = above.iter().copied().chain([id]).collect();
    ids.sort();
    let nodes = ids
        .iter()
        .map(|i| {
            let n = by_id.get(i)?;
            Some(NodeRecord {
                id: n.id,
                kind: n.kind,
                on: crate::executor::effective_enabled(n, t),
                params: crate::executor::params_for_frame(n, t),
                input_links: &n.input_links,
                output_link: &n.output_link,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let inside: HashSet<NodeId> = ids.iter().copied().collect();
    let mut edges: Vec<&crate::graph::Edge> = graph.edges.iter().filter(|e| inside.contains(&e.to_node)).collect();
    edges.sort_by_key(|e| (e.to_node, e.to_port, e.from_node, e.from_port));
    let env = RunEnv { seeded: source_read(graph, above, by_id, env), ..env.clone() };
    let value = serde_json::to_value(Key { version: KEY_VERSION, source, env, nodes, edges }).ok()?;
    serde_json::to_vec(&value).ok()
}

/// Each heavy node the run reaches that is switched on, with its key or
/// why it has none.
#[allow(clippy::too_many_arguments)]
fn lookups(graph: &NodeGraph, order: &[NodeId], feeding: &HashSet<NodeId>, source: &HeldSource, env: &RunEnv, looked_at: &dyn Fn(NodeId) -> bool, history: &dyn Fn(&Node) -> bool) -> Vec<(NodeId, Result<Vec<u8>, Refusal>)> {
    let by_id: HashMap<NodeId, &Node> = graph.nodes.iter().map(|n| (n.id, n)).collect();
    order
        .iter()
        .filter(|id| feeding.contains(id))
        .filter_map(|id| by_id.get(id).map(|n| (*id, *n)))
        .filter(|(_, n)| is_heavy(n) && crate::executor::effective_enabled(n, env.clip_time_secs))
        .map(|(id, _)| {
            let above = ancestors(graph, id);
            let found = match refusal(graph, id, &above, &by_id, looked_at, history) {
                Some(why) => Err(why),
                None => key_of(graph, id, &above, &by_id, source, env).ok_or(Refusal::Unkeyable),
            };
            (id, found)
        })
        .collect()
}

/// The nodes a run with `hits` doesn't run: those above a hit whose every
/// reader that runs is a hit or not run itself. A node read from outside
/// the cut (a Source a Blend also reads) still runs. Decided from the
/// output up, so a node's readers are decided before it.
pub(crate) fn skippable(graph: &NodeGraph, order: &[NodeId], feeding: &HashSet<NodeId>, hits: &HashMap<NodeId, (Arc<Vec<f32>>, Vec<Decline>)>) -> HashSet<NodeId> {
    let mut skip = HashSet::new();
    if hits.is_empty() {
        return skip;
    }
    let mut above: HashSet<NodeId> = HashSet::new();
    for id in hits.keys() {
        above.extend(ancestors(graph, *id));
    }
    let mut readers: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for (from, to) in graph.iter_dependencies() {
        readers.entry(from).or_default().push(to);
    }
    for id in order.iter().rev() {
        if !above.contains(id) {
            continue;
        }
        let cut = readers
            .get(id)
            .into_iter()
            .flatten()
            .filter(|r| feeding.contains(r) || **r == graph.output_node_id)
            .all(|r| hits.contains_key(r) || skip.contains(r));
        if cut {
            skip.insert(*id);
        }
    }
    skip
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Vec<u8> {
        vec![n]
    }

    fn px(len: usize) -> Arc<Vec<f32>> {
        Arc::new(vec![0.0; len])
    }

    /// #1863: the least recently used goes first, a lookup counts as a
    /// use, and the budget keeps at least two entries whatever their size.
    #[test]
    fn the_least_recently_used_goes_and_two_always_stay() {
        let mut held = HeldOutputs::default();
        // Three entries of 100 bytes (25 floats) in a budget of 300.
        held.set_budget(300);
        for n in 1..=3 {
            held.put(key(n), px(25), vec![]);
        }
        assert_eq!(held.len(), 3);
        assert!(held.get(&key(1)).is_some(), "a lookup makes 1 the newest");
        held.put(key(4), px(25), vec![]);
        assert!(held.has(&key(1)) && held.has(&key(3)) && held.has(&key(4)), "2 was the least recently used");
        assert!(!held.has(&key(2)));
        // Entries each larger than the budget: the two newest still stay.
        held.set_budget(10);
        assert_eq!(held.len(), MIN_ENTRIES);
        assert!(held.has(&key(1)) && held.has(&key(4)));
        held.clear();
        assert!(held.is_empty());
    }

    /// Storing under a key already held replaces it, never a second copy.
    #[test]
    fn a_key_is_held_once() {
        let mut held = HeldOutputs::default();
        held.put(key(1), px(4), vec![]);
        held.put(key(1), Arc::new(vec![1.0; 4]), vec![]);
        assert_eq!(held.len(), 1);
        assert_eq!(held.get(&key(1)).unwrap().0[0], 1.0);
    }

    fn node(kind: NodeKind) -> Node {
        Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] }
    }

    fn chain(nodes: Vec<Node>) -> NodeGraph {
        let edges = nodes.windows(2).map(|w| crate::graph::Edge { from_node: w[0].id, from_port: 0, to_node: w[1].id, to_port: 0 }).collect();
        NodeGraph { output_node_id: nodes.last().unwrap().id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
    }

    /// The nodes above a hit run no more when only the cut reads them;
    /// one also read from below the hit still runs.
    #[test]
    fn a_node_read_from_outside_the_cut_still_runs() {
        let (s, up, heavy, down, out) = (node(NodeKind::Source), node(NodeKind::Exposure), node(NodeKind::Ofx), node(NodeKind::Exposure), node(NodeKind::Output));
        let ids = [s.id, up.id, heavy.id, down.id, out.id];
        let mut g = chain(vec![s, up, heavy, down, out]);
        let order: Vec<NodeId> = ids.to_vec();
        let feeding: HashSet<NodeId> = ids.into_iter().collect();
        let hits: HashMap<_, _> = [(ids[2], (px(4), vec![]))].into_iter().collect();
        assert_eq!(skippable(&g, &order, &feeding, &hits), [ids[0], ids[1]].into_iter().collect());
        // The node below also reads the Source, on port 1: it runs.
        g.edges.push(crate::graph::Edge { from_node: ids[0], from_port: 0, to_node: ids[3], to_port: 1 });
        assert_eq!(skippable(&g, &order, &feeding, &hits), [ids[1]].into_iter().collect());
    }

    fn source() -> HeldSource {
        HeldSource {
            clip: ClipId::nil(),
            media: MediaId::nil(),
            path: "/m.mov".into(),
            file_len: 10,
            file_modified: SystemTime::UNIX_EPOCH,
            log_format: None,
            pts_micros: 400_000,
            decoded: (4, 2),
            retime: Retime { speed: 1.0, reverse: false, source_at_clip_time: 0.4, media_fps: Some(25.0), conform_fps: None },
            proxy: None,
        }
    }

    fn env() -> RunEnv {
        RunEnv { width: 4, height: 2, clip_time_secs: 0.4, source_secs: 0.4, frame_number: 10, working: WorkingColorspace::Rec709Linear, encoding: GradeEncoding::default(), lut_tetrahedral: true, render_scale: 1.0, gpu: false, seeded: false, source_domain: "Display".into() }
    }

    fn key_in(g: &NodeGraph, id: NodeId, source: &HeldSource, env: &RunEnv) -> Vec<u8> {
        let by_id: HashMap<NodeId, &Node> = g.nodes.iter().map(|n| (n.id, n)).collect();
        key_of(g, id, &ancestors(g, id), &by_id, source, env).expect("a key")
    }

    /// The key is the picture's whole recipe: a change above the node,
    /// to the node, to the source or the run is another key; one below it
    /// is not; and the same parameters write the same bytes, a map among
    /// them too.
    #[test]
    fn the_key_covers_the_node_and_what_is_above_it() {
        let mut ofx = node(NodeKind::Ofx);
        let values = (0..16).map(|i| (format!("p{i}"), klipp_core::OfxValue::Int(i))).collect();
        ofx.params.ofx = Some(crate::plugin::OfxNodeParams { plugin: "x".into(), values, filter: true });
        let g = chain(vec![node(NodeKind::Source), node(NodeKind::Exposure), ofx, node(NodeKind::Exposure), node(NodeKind::Output)]);
        let (up, heavy, down) = (g.nodes[1].id, g.nodes[2].id, g.nodes[3].id);
        let base = key_in(&g, heavy, &source(), &env());
        // The same graph read back from its JSON (its maps in another
        // order in memory) keys alike.
        let again: NodeGraph = serde_json::from_str(&serde_json::to_string(&g).unwrap()).unwrap();
        assert_eq!(key_in(&again, heavy, &source(), &env()), base, "the same parameters, the same key");

        let edited = |at: NodeId, f: &dyn Fn(&mut Node)| {
            let mut g = g.clone();
            f(g.nodes.iter_mut().find(|n| n.id == at).unwrap());
            key_in(&g, heavy, &source(), &env())
        };
        assert_eq!(edited(down, &|n| n.params.exposure = Some(1.0)), base, "an edit below is the same key");
        assert_ne!(edited(up, &|n| n.params.exposure = Some(1.0)), base, "an edit above");
        assert_ne!(edited(up, &|n| n.enabled = false), base, "a node above switched off");
        assert_ne!(edited(heavy, &|n| n.params.ofx.as_mut().unwrap().plugin = "y".into()), base, "the node itself");

        let with = |f: &dyn Fn(&mut HeldSource)| {
            let mut s = source();
            f(&mut s);
            key_in(&g, heavy, &s, &env())
        };
        assert_ne!(with(&|s| s.path = "/n.mov".into()), base, "another file");
        assert_ne!(with(&|s| s.file_len = 11), base, "the file rewritten");
        assert_ne!(with(&|s| s.file_modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1)), base, "the file touched");
        assert_ne!(with(&|s| s.pts_micros += 40_000), base, "another frame");
        assert_ne!(with(&|s| s.decoded = (8, 4)), base, "decoded at another size");
        assert_ne!(with(&|s| s.log_format = Some(klipp_core::LogFormat::default())), base, "another log format");
        assert_ne!(with(&|s| s.media = MediaId::new()), base, "another media");
        assert_ne!(with(&|s| s.retime.speed = 0.5), base, "another speed");
        assert_ne!(with(&|s| s.retime.reverse = true), base, "reversed");
        assert_ne!(with(&|s| s.retime.source_at_clip_time = 0.41), base, "a varispeed's other sample");
        assert_ne!(with(&|s| s.retime.conform_fps = Some(24.0)), base, "conformed");
        assert_ne!(with(&|s| s.proxy = Some(("/m.proxy.mov".into(), 5, SystemTime::UNIX_EPOCH))), base, "shown from a proxy");

        let run = |f: &dyn Fn(&mut RunEnv)| {
            let mut e = env();
            f(&mut e);
            key_in(&g, heavy, &source(), &e)
        };
        assert_ne!(run(&|e| e.clip_time_secs = 0.44), base, "another clip time");
        assert_ne!(run(&|e| e.source_secs = 0.42), base, "another source time");
        assert_ne!(run(&|e| (e.width, e.height) = (8, 4)), base, "another size");
        assert_ne!(run(&|e| e.working = WorkingColorspace::AcesCG), base, "another working space");
        assert_ne!(run(&|e| e.lut_tetrahedral = false), base, "trilinear LUTs");
        assert_ne!(run(&|e| e.render_scale = 0.5), base, "another render scale");
        assert_ne!(run(&|e| e.gpu = true), base, "the GPU");
        assert_ne!(run(&|e| e.source_domain = "Scene".into()), base, "a scene-referred source");
        assert_ne!(run(&|e| e.seeded = true), base, "a seed, with an Exposure above the node");
    }

    /// A seeded run whose heavy node reads the Source through
    /// passthroughs alone keys as an unseeded one: the node read the CPU
    /// frame either way.
    #[test]
    fn a_seed_the_node_never_read_is_not_in_its_key() {
        let g = chain(vec![node(NodeKind::Source), node(NodeKind::InputTransform), node(NodeKind::Ofx), node(NodeKind::Exposure), node(NodeKind::Output)]);
        let heavy = g.nodes[2].id;
        let seeded = RunEnv { seeded: true, ..env() };
        assert_eq!(key_in(&g, heavy, &source(), &seeded), key_in(&g, heavy, &source(), &env()));
    }

    /// Below a heavy node the seed is out of the picture: a plug-in under a
    /// denoise keys alike seeded or not, since the denoise read the CPU
    /// frame; one under an Exposure on the chain from the seed doesn't.
    #[test]
    fn a_seed_read_only_through_a_heavy_node_is_not_in_the_key() {
        let mut tnr = node(NodeKind::Effect);
        tnr.params.effects = vec![klipp_effects::BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.2 }];
        let seeded = RunEnv { seeded: true, ..env() };
        let g = chain(vec![node(NodeKind::Source), tnr.clone(), node(NodeKind::Exposure), node(NodeKind::Ofx), node(NodeKind::Exposure), node(NodeKind::Output)]);
        let plugin = g.nodes[3].id;
        assert_eq!(key_in(&g, plugin, &source(), &seeded), key_in(&g, plugin, &source(), &env()), "through the denoise");
        let g = chain(vec![node(NodeKind::Source), node(NodeKind::Exposure), tnr, node(NodeKind::Ofx), node(NodeKind::Exposure), node(NodeKind::Output)]);
        let plugin = g.nodes[3].id;
        assert_ne!(key_in(&g, plugin, &source(), &seeded), key_in(&g, plugin, &source(), &env()), "an Exposure on the chain from the seed");
    }

    /// Refused: the graph's result, a node or one above it looked at, an
    /// impure node above, a plug-in with history.
    #[test]
    fn what_is_never_held() {
        let g = chain(vec![node(NodeKind::Source), node(NodeKind::Mask), node(NodeKind::Ofx), node(NodeKind::Exposure), node(NodeKind::Output)]);
        let by_id: HashMap<NodeId, &Node> = g.nodes.iter().map(|n| (n.id, n)).collect();
        let heavy = g.nodes[2].id;
        let above = ancestors(&g, heavy);
        let no = |_: NodeId| false;
        let plain = |_: &Node| false;
        assert_eq!(refusal(&g, heavy, &above, &by_id, &no, &plain), Some(Refusal::Impure), "a window above");
        let source = g.nodes[0].id;
        assert_eq!(refusal(&g, heavy, &above, &by_id, &|id| id == source, &plain), Some(Refusal::LookedAt), "the source tapped");
        assert_eq!(refusal(&g, heavy, &above, &by_id, &no, &|_| true), Some(Refusal::History));
        let last = chain(vec![node(NodeKind::Source), node(NodeKind::Ofx), node(NodeKind::Output)]);
        let by_id: HashMap<NodeId, &Node> = last.nodes.iter().map(|n| (n.id, n)).collect();
        let heavy = last.nodes[1].id;
        assert_eq!(refusal(&last, heavy, &ancestors(&last, heavy), &by_id, &no, &plain), Some(Refusal::Result));
    }

    /// `ready` is `plan`'s answer without its effects: false before the
    /// entry is stored, true after, false again where `plan` would refuse
    /// (one rule for both), and it counts and moves nothing.
    #[test]
    fn ready_answers_as_the_run_would() {
        let g = chain(vec![node(NodeKind::Source), node(NodeKind::Ofx), node(NodeKind::Exposure), node(NodeKind::Output)]);
        let order: Vec<NodeId> = g.nodes.iter().map(|n| n.id).collect();
        let feeding: HashSet<NodeId> = order.iter().copied().collect();
        let (no, plain) = (|_: NodeId| false, |_: &Node| false);
        let mut held = HeldOutputs::default();
        assert!(!held.ready(&g, &order, &feeding, &source(), &env(), &no, &plain), "nothing held yet");
        let run = held.plan(&g, &order, &feeding, &source(), &env(), &no, &plain);
        let key = run.store.get(&order[1]).expect("a miss stores").clone();
        held.put(key, px(32), vec![]);
        let counts = held.counts();
        assert!(held.ready(&g, &order, &feeding, &source(), &env(), &no, &plain));
        assert_eq!(held.counts(), counts, "ready counts nothing");
        let source_id = order[0];
        assert!(!held.ready(&g, &order, &feeding, &source(), &env(), &|id| id == source_id, &plain), "looked at: plan refuses, so does ready");
        assert!(!held.ready(&g, &order, &feeding, &source(), &env(), &no, &|_| true), "history: the same");
    }
}
