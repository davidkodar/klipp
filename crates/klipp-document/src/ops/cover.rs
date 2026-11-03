//! Covers (#3311): the `cover` op. A cover is a frame of the sequence with
//! words over it, made at a platform's thumbnail size
//! ([`klipp_timeline::cover`]). This adds one, changes one, or takes one
//! out, as one undo step each. Its words are a title, made by the title op
//! from the same fields on the cover's own frame, so a style, a backing or
//! a size work as they do on the timeline.

use super::*;
use klipp_timeline::cover::{find, Cover, CoverCrop};

/// What the op was given, apart from the cover it names.
pub(crate) struct CoverArgs<'a> {
    pub remove: bool,
    pub name: Option<&'a str>,
    pub at: Option<&'a Anchor>,
    pub size: Option<&'a str>,
    pub crop: Option<CoverCrop>,
    pub title: Option<&'a serde_json::Value>,
    pub with_titles: Option<bool>,
}

/// The title the op's `title` makes on a frame of `size`, in the brief's
/// styles; the brand kit's heading where it names no style and the brief
/// has one. `None` for empty words, which take a cover's away.
fn words(seq: &Sequence, size: &klipp_core::cover_sizes::CoverSize, title: &serde_json::Value) -> Result<Option<klipp_timeline::TitleData>, String> {
    let mut fields = match title {
        serde_json::Value::String(text) => serde_json::json!({ "text": text }),
        other => other.clone(),
    };
    if fields.get("text").and_then(serde_json::Value::as_str).is_some_and(|t| t.trim().is_empty()) {
        return Ok(None);
    }
    let heading = crate::title_style::look(seq.brief.as_ref(), "heading").is_some();
    if let Some(obj) = fields.as_object_mut().filter(|o| heading && !o.contains_key("style")) {
        obj.insert("style".into(), serde_json::json!("heading"));
    }
    let mut frame = Sequence::new_default();
    frame.tracks.clear();
    frame.fps = seq.fps;
    frame.resolution = klipp_core::Resolution { width: size.width, height: size.height };
    frame.brief = seq.brief.clone();
    title_from_fields(&frame, &fields, 0.0, 1.0).map(Some)
}

/// The `cover` op: `named` changed or taken out, or a new cover made.
pub(crate) fn cover(seq: &Sequence, fps: Fps, i: usize, named: Option<&str>, a: CoverArgs) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    use klipp_core::cover_sizes;
    let old = seq.covers.clone();
    let mut new = old.clone();
    let sizes = || cover_sizes::all().iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", ");
    let at = named.map(|n| find(&old, n).ok_or_else(|| {
        let have: Vec<String> = old.iter().map(|c| if c.name.is_empty() { c.id.to_string() } else { format!("{} ({})", c.name, c.id) }).collect();
        refuse(i, format!("no cover {:?}", n.trim()), if have.is_empty() { "the sequence has no covers; leave cover out to make one".to_string() } else { format!("its covers are {}", have.join(", ")) })
    })).transpose()?;

    if a.remove {
        let k = at.ok_or_else(|| refuse(i, "remove names no cover", "give cover, its id or name"))?;
        let gone = new.remove(k);
        let what = format!("cover {} taken out", if gone.name.is_empty() { gone.id.to_string() } else { quoted(&gone.name) });
        return Ok((what, EditCommand::SetCovers { old, new }, Vec::new()));
    }

    let mut c = match at {
        Some(k) => new[k].clone(),
        None => {
            if old.len() >= klipp_timeline::cover::MAX_COVERS {
                return Err(refuse(i, format!("the sequence has {} covers, the most it keeps", old.len()), "take one out first"));
            }
            if a.at.is_none() {
                return Err(refuse(i, "a new cover needs at, its frame", "give at, as {\"secs\": 12.4} or a marker's time"));
            }
            let size = a.size.map(str::to_string).or_else(|| cover_sizes::for_frame(seq.resolution.width, seq.resolution.height).map(|s| s.id.clone())).unwrap_or_default();
            Cover { id: klipp_core::CoverId::new(), name: String::new(), at_secs: 0.0, size, crop: None, text: None, with_titles: false }
        }
    };
    if let Some(n) = a.name {
        c.name = n.to_string();
    }
    if let Some(anchor) = a.at {
        let t = resolve_anchor(seq, fps, anchor, i)?;
        let end = seq.total_duration().to_secs_f64();
        if secs(t) >= end && end > 0.0 {
            return Err(refuse(i, format!("{:.2} s is past the sequence's end at {end:.2} s", secs(t)), "give a frame inside the sequence"));
        }
        c.at_secs = secs(t);
    }
    if let Some(s) = a.size {
        c.size = s.to_string();
    }
    let size = cover_sizes::by_id(&c.size).ok_or_else(|| refuse(i, format!("{:?} is not a cover size", c.size), format!("the sizes are {}", sizes())))?;
    c.size = size.id.clone();
    if let Some(crop) = a.crop {
        c.crop = Some(crop.clamped());
    }
    if let Some(on) = a.with_titles {
        c.with_titles = on;
    }
    if let Some(title) = a.title {
        c.text = words(seq, size, title).map_err(|e| refuse(i, e, "give the words as text, or the title op's fields without at, secs or track"))?;
    }
    let c = c.checked().ok_or_else(|| refuse(i, "the cover's frame isn't a time", "give at as seconds inside the sequence"))?;
    klipp_timeline::cover::cover_sequence(seq, &c, size.width, size.height).map_err(|e| refuse(i, e, "leave crop out, or give zoom 1"))?;

    let label = if c.name.is_empty() { c.id.to_string() } else { format!("{} ({})", quoted(&c.name), c.id) };
    let mut said = vec![format!("{} {}×{}", size.name, size.width, size.height), format!("at {:.2} s", c.at_secs)];
    match c.crop {
        Some(crop) if crop.is_whole() => said.push("the whole frame".to_string()),
        Some(crop) => said.push(format!("cropped to {:.0}% at {:.2}, {:.2}", 100.0 / crop.zoom, crop.x, crop.y)),
        None => {}
    }
    if let Some(td) = &c.text {
        said.push(format!("with {}", quoted(&td.primary_text().map(|t| t.text.clone()).unwrap_or_default())));
    }
    let what = format!("cover {label} {}: {}", if at.is_some() { "changed" } else { "made" }, said.join(", "));
    match at {
        Some(k) => new[k] = c,
        None => new.push(c),
    }
    Ok((what, EditCommand::SetCovers { old, new }, Vec::new()))
}
