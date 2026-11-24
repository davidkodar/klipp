//! The Export page's Cover section (#3311): a sequence's covers, made,
//! changed, compared at the size a phone shows them, and written out.
//! Shown for a sequence made for a platform, or once asked for
//! ([`crate::prefs::CoverShow`]); otherwise nothing of it shows.
//! Covers are drawn and proposed by [`klipp_render::cover`], as the
//! export draws them, on threads of their own.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};

use klipp_core::CoverId;
use klipp_document::history::EditCommand;
use klipp_document::ops::Op;
use klipp_timeline::cover::Cover;
use klipp_timeline::Sequence;

/// How wide a cover is drawn in the section: as a phone's feed shows it.
pub const FEED_W: f32 = 320.0;

/// How wide a cover is in a phone's search list: what Compare shows.
pub const LIST_W: f32 = 168.0;

/// The cover op for what the section asks.
pub enum Ask<'a> {
    New { at_secs: f64 },
    Words { cover: CoverId, text: &'a str },
    Size { cover: CoverId, size: &'a str },
    Remove { cover: CoverId },
}

fn op(ask: &Ask) -> Op {
    let v = match ask {
        Ask::New { at_secs } => serde_json::json!({ "op": "cover", "at": { "secs": at_secs } }),
        Ask::Words { cover, text } => serde_json::json!({ "op": "cover", "cover": cover.to_string(), "title": text }),
        Ask::Size { cover, size } => serde_json::json!({ "op": "cover", "cover": cover.to_string(), "size": size }),
        Ask::Remove { cover } => serde_json::json!({ "op": "cover", "cover": cover.to_string(), "remove": true }),
    };
    serde_json::from_value(v).expect("a cover op")
}

/// The undo step `ask` makes of `seq`'s covers, and what it did; why not,
/// when the op refuses.
pub fn edit(seq: &Sequence, pool: &klipp_media::MediaPool, ask: &Ask) -> Result<(String, EditCommand), String> {
    let plan = klipp_document::ops::resolve(seq, pool, &[op(ask)]).map_err(|r| format!("{} ({})", r.why, r.hint))?;
    let step = plan.steps.into_iter().next().ok_or("no step")?;
    Ok((step.what, step.edit))
}

/// A cover as the section names it: its name or its time, and its size.
pub fn label(c: &Cover) -> String {
    let size = klipp_core::cover_sizes::by_id(&c.size).map_or(c.size.clone(), |s| format!("{} {}×{}", s.name, s.width, s.height));
    let words = c.text.as_ref().and_then(|t| t.primary_text()).map(|t| t.text.trim().to_string()).filter(|t| !t.is_empty());
    let name = if c.name.is_empty() { words.map(|w| format!("\u{201c}{w}\u{201d}")).unwrap_or_else(|| "Cover".into()) } else { c.name.clone() };
    format!("{name} \u{00b7} {:.2} s \u{00b7} {size}", c.at_secs)
}

/// What a cover's picture depends on: the cover and the sequence's
/// edits, so a drawn one is drawn again when either changes.
pub fn key(c: &Cover, revision: (usize, usize)) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(c).unwrap_or_default().hash(&mut h);
    revision.hash(&mut h);
    h.finish()
}

/// The size a cover is drawn at in the section: its own shape, [`FEED_W`]
/// wide, even.
pub fn thumb_size(c: &Cover) -> (u32, u32) {
    let (w, h) = klipp_core::cover_sizes::by_id(&c.size).map_or((16, 9), |s| (s.width, s.height));
    let tw = FEED_W as u32;
    (tw, (((tw as f64 * h as f64 / w.max(1) as f64).round() as u32).max(2)) & !1)
}

/// A frame proposed, as the section lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposed {
    pub at_secs: f64,
    pub why: String,
}

/// What the section keeps between frames: the cover being changed, its
/// words as typed, whether covers are compared, the pictures drawn, and
/// the work on other threads.
pub struct CoverPanel {
    pub selected: Option<CoverId>,
    pub words: String,
    pub compare: bool,
    pub thumbs: HashMap<CoverId, (u64, egui::TextureHandle)>,
    pub asked: HashMap<CoverId, u64>,
    pub drawn_tx: Sender<(CoverId, u64, Result<klipp_render::still::Still, String>)>,
    pub drawn_rx: Receiver<(CoverId, u64, Result<klipp_render::still::Still, String>)>,
    pub proposals: Option<Vec<Proposed>>,
    pub proposing: Option<Receiver<Result<(Vec<Proposed>, Vec<String>), String>>>,
    pub notes_tx: Sender<String>,
    pub notes_rx: Receiver<String>,
}

impl Default for CoverPanel {
    fn default() -> Self {
        let (drawn_tx, drawn_rx) = channel();
        let (notes_tx, notes_rx) = channel();
        Self { selected: None, words: String::new(), compare: false, thumbs: HashMap::new(), asked: HashMap::new(), drawn_tx, drawn_rx, proposals: None, proposing: None, notes_tx, notes_rx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #3311: the section's asks make the cover op's undo steps, a new
    /// cover of the sequence's shape at the frame asked; words and a size
    /// change it; remove takes it out; a frame past the end is refused.
    #[test]
    fn the_cover_sections_asks_are_the_cover_ops_steps() {
        let mut seq = Sequence::new_default();
        let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        seq.tracks[v].clips = vec![klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Generator(klipp_core::GeneratorData::ColorBars), source_in: tc(0.0), source_out: tc(10.0), ..klipp_timeline::Clip::gap(tc(0.0), tc(10.0)) }];
        let pool = klipp_media::MediaPool::new();
        let covers = |e: &EditCommand| match e {
            EditCommand::SetCovers { new, .. } => new.clone(),
            other => panic!("{other:?}"),
        };
        let (what, step) = edit(&seq, &pool, &Ask::New { at_secs: 2.0 }).unwrap();
        assert!(what.contains("made"), "{what}");
        let c = covers(&step).remove(0);
        assert_eq!((c.at_secs, c.size.as_str()), (2.0, "youtube"));
        assert_eq!(label(&c), "Cover \u{00b7} 2.00 s \u{00b7} YouTube thumbnail 1280×720");
        assert_eq!(thumb_size(&c), (320, 180));
        seq.covers = vec![c.clone()];

        let (_, step) = edit(&seq, &pool, &Ask::Words { cover: c.id, text: "Big News" }).unwrap();
        let worded = covers(&step).remove(0);
        assert!(label(&worded).starts_with("\u{201c}Big News\u{201d}"), "{}", label(&worded));
        assert_ne!(key(&worded, (1, 0)), key(&c, (1, 0)), "drawn again when its words change");
        assert_ne!(key(&c, (2, 0)), key(&c, (1, 0)), "and when the sequence is edited");
        let (_, step) = edit(&seq, &pool, &Ask::Size { cover: c.id, size: "vertical" }).unwrap();
        assert_eq!(thumb_size(&covers(&step)[0]), (320, 568));
        let (_, step) = edit(&seq, &pool, &Ask::Remove { cover: c.id }).unwrap();
        assert!(covers(&step).is_empty());
        assert!(edit(&seq, &pool, &Ask::New { at_secs: 50.0 }).unwrap_err().contains("past the sequence's end"));
    }

    /// The section shows for a sequence made for a platform unless
    /// hidden, for any once asked for, and for none otherwise: an
    /// existing project shows nothing new.
    #[test]
    fn the_cover_section_shows_only_where_it_fits_or_is_asked_for() {
        use crate::prefs::CoverShow;
        let plain = Sequence::new_default();
        let mut made_for = Sequence::new_default();
        made_for.brief = Some(klipp_timeline::Brief { platform: "YouTube".into(), ..Default::default() });
        assert!(!CoverShow::default().shows(&plain), "off by default");
        assert!(CoverShow::Auto.shows(&made_for));
        assert!(!CoverShow::Hidden.shows(&made_for), "hidden even for a platform");
        assert!(CoverShow::Shown.shows(&plain));
        let prefs: crate::prefs::ExportPrefs = serde_json::from_value(serde_json::json!({ "default_output_dir": null, "last_preset_id": null, "custom_presets": [] })).unwrap();
        assert_eq!(prefs.cover_section, CoverShow::Auto, "an old preferences file reads as before");
    }
}
