//! Finding what a request names: markers, clips, nodes, transitions, log
//! formats, and the ids they come as (#826).

#[allow(unused_imports)]
use super::*;

pub(crate) fn marker_color(s: &str) -> Result<klipp_core::MarkerColor, String> {
    use klipp_core::MarkerColor::*;
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "red" => Red,
        "orange" => Orange,
        "yellow" => Yellow,
        "green" => Green,
        "blue" => Blue,
        "purple" => Purple,
        other => return Err(format!("no marker colour \"{other}\": red, orange, yellow, green, blue or purple")),
    })
}

pub(crate) fn marker_kind(s: &str) -> Result<klipp_core::MarkerKind, String> {
    use klipp_core::MarkerKind::*;
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "" | "none" => None,
        "chapter" => Chapter,
        "todo" | "to_do" => ToDo,
        "comment" => Comment,
        "best_take" => BestTake,
        "section" => Section,
        other => return Err(format!("no marker kind \"{other}\": chapter, todo, comment, best_take or section")),
    })
}

pub(crate) fn marker_kind_name(k: klipp_core::MarkerKind) -> &'static str {
    use klipp_core::MarkerKind::*;
    match k {
        None => "none",
        Chapter => "chapter",
        ToDo => "todo",
        Comment => "comment",
        BestTake => "best_take",
        Section => "section",
    }
}

/// The marker named by `id`, when the timeline has it.
pub(crate) fn find_marker(ctx: &ControlContext, id: &str) -> Result<klipp_core::MarkerId, String> {
    let id = klipp_core::MarkerId(parse_uuid(id, "marker_id")?);
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    if seq.markers.iter().any(|m| m.id == id) {
        Ok(id)
    } else {
        Err(format!("no marker {} on the timeline (list_markers has them)", id.0))
    }
}

/// A frame rate as a fraction: the NTSC rates (23.976, 29.97, 47.95,
/// 59.94, 119.88) as n*1000/1001, the rest as whole numbers.
pub(crate) fn fps_fraction(fps: f64) -> Result<(u32, u32), String> {
    for base in [24u32, 30, 48, 60, 120] {
        if (fps - base as f64 * 1000.0 / 1001.0).abs() < 0.01 {
            return Ok((base * 1000, 1001));
        }
    }
    let whole = fps.round();
    if (fps - whole).abs() < 0.01 && (1.0..=240.0).contains(&whole) {
        return Ok((whole as u32, 1));
    }
    Err(format!("{fps} fps is not a frame rate Klipp takes: a whole number, or 23.976, 29.97, 59.94"))
}

pub(crate) fn parse_uuid(s: &str, what: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(s).map_err(|e| format!("{what} is not a uuid: {e}"))
}

pub(crate) fn clip_id(s: &str) -> Result<klipp_core::ClipId, String> {
    Ok(klipp_core::ClipId(parse_uuid(s, "clip_id")?))
}

/// A clip id that names a clip on the timeline.
///
/// Commands that address a clip used to parse the id and send the
/// command on, so an id that named nothing was answered `accepted` and
/// then dropped by the UI's `find_clip`. Checking here costs one read of
/// the sequence and turns a silent no-op into an error the caller can
/// act on.
pub(crate) fn existing_clip_id(
    ctx: &ControlContext,
    s: &str,
) -> Result<klipp_core::ClipId, String> {
    let want = clip_id(s)?;
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    if seq.tracks.iter().flat_map(|t| t.clips.iter()).any(|c| c.id == want) {
        return Ok(want);
    }
    let mut have: Vec<String> = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.id.0.to_string())
        .collect();
    have.truncate(8);
    Err(format!(
        "no clip with id {s} on the timeline. Clips are: {}",
        if have.is_empty() { "none".to_string() } else { have.join(", ") },
    ))
}

/// A node id that names a node in `clip`'s graph.
///
/// The same silent no-op as [`existing_clip_id`], and a worse one: node
/// ids are per clip, so passing the id of *another* clip's node — the
/// two clips' Correctors look alike, and both graphs start out as
/// Source, Corrector, Output — was accepted and changed nothing. The
/// grade read back as identity with no indication why.
///
/// Nodes inside a Compound's subgraph count: they are addressable, and
/// rejecting them here would break a legitimate write. A clip with no
/// Corrector is given one by path (`graph_ops`), as any node is made.
pub(crate) fn existing_node_id(
    ctx: &ControlContext,
    clip: klipp_core::ClipId,
    s: &str,
    what: &str,
) -> Result<uuid::Uuid, String> {
    let want = parse_uuid(s, what)?;
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    let graph = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .find(|c| c.id == clip)
        .map(|c| c.own_grade())
        .ok_or_else(|| format!("no clip with id {} on the timeline", clip.0))?;
    if find_node(graph, want) {
        return Ok(want);
    }
    Err(format!(
        "clip {} has no node {s}. Its nodes are: {}. \
         Node ids belong to one clip — another clip's Corrector will not do.",
        clip.0,
        node_summary(graph).join(", "),
    ))
}

/// `Ok` when nodes `a` and `b` of `clip` are in one graph, the top-level
/// one or the same Compound's: an edge can only join nodes of one graph,
/// and the app edits the graph that holds them (#1179).
pub(crate) fn in_one_graph(ctx: &ControlContext, clip: klipp_core::ClipId, a: uuid::Uuid, b: uuid::Uuid) -> Result<(), String> {
    let seq = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    let graph = seq
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .find(|c| c.id == clip)
        .map(|c| c.own_grade())
        .ok_or_else(|| format!("no clip with id {} on the timeline", clip.0))?;
    let where_is = |id| graph.path_to_node(id).map_or_else(|| "nowhere".to_string(), |p| match p.last() {
        None => "the top-level graph".to_string(),
        Some(compound) => format!("Compound {compound}'s graph"),
    });
    if graph.path_to_node(a) == graph.path_to_node(b) {
        return Ok(());
    }
    Err(format!("an edge joins nodes of one graph: {a} is in {}, {b} in {}", where_is(a), where_is(b)))
}

/// Whether `id` is anywhere in `graph`, including inside a Compound.
pub(crate) fn find_node(graph: &klipp_nodes::NodeGraph, id: uuid::Uuid) -> bool {
    graph.nodes.iter().any(|n| {
        n.id == id
            || n.params
                .compound_subgraph
                .as_ref()
                .is_some_and(|sub| find_node(sub, id))
    })
}

/// `kind id` for each node, so the error says what the caller could have
/// meant rather than only what they got wrong.
pub(crate) fn node_summary(graph: &klipp_nodes::NodeGraph) -> Vec<String> {
    graph
        .nodes
        .iter()
        .map(|n| format!("{:?} {}", n.kind, n.id))
        .collect()
}

/// Every `LogFormat` a client may name, in the spelling the wire uses.
pub const LOG_FORMAT_NAMES: &[&str] = &[
    "Srgb", "Linear", "LogC3", "LogC4", "SLog2", "SLog3", "CanonLog", "CanonLog3",
    "VLog", "NLog", "FLog", "DJIDLog", "GoProFlat", "AppleLog", "LeicaLLog", "LeicaLLogBt709",
    "RedLog3G10", "BmdFilmGen5", "HLG", "PQ",
];

pub(crate) fn parse_log_format(name: &str) -> Result<klipp_core::LogFormat, String> {
    serde_json::from_value(json!(name)).map_err(|_| {
        format!("unknown log format {name:?}; expected one of {}", LOG_FORMAT_NAMES.join(", "))
    })
}

/// Where a transition sits on its track (#711): the cut between its two
/// clips, and the stretch it covers there by its alignment, in seconds.
/// `None` when its first clip is no longer on the track.
pub(crate) fn transition_span(t: &klipp_timeline::Track, tr: &klipp_timeline::Transition) -> Option<(f64, f64, f64)> {
    let cut = t.find_clip(tr.clip_a_id)?.timeline_out.to_secs_f64();
    let d = tr.duration.to_secs_f64();
    let (from, to) = match tr.alignment {
        klipp_core::TransitionAlignment::Center => (cut - d / 2.0, cut + d / 2.0),
        klipp_core::TransitionAlignment::StartAtCut => (cut, cut + d),
        klipp_core::TransitionAlignment::EndAtCut => (cut - d, cut),
    };
    Some((cut, from, to))
}

/// A transition's alignment in words, and as a client names it.
pub(crate) fn transition_alignment(a: klipp_core::TransitionAlignment) -> (&'static str, &'static str) {
    match a {
        klipp_core::TransitionAlignment::Center => ("centred on the cut", "center"),
        klipp_core::TransitionAlignment::StartAtCut => ("after the cut", "start_at_cut"),
        klipp_core::TransitionAlignment::EndAtCut => ("before the cut", "end_at_cut"),
    }
}
