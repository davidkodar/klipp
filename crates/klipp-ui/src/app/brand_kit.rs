//! A brand kit taken into the open sequence from the app (#2722), as
//! `apply_brand_kit` takes it over the socket: its look (the brief and
//! the caption look) as one undo step, then its intro, outro and logo as
//! another once the kit's files are in the media pool.

use super::*;
use crate::state::{BrandKitParts, PendingBrandKit};
use klipp_document::brand_kit_ops::{look_edits, sequence_ops, Bookend, SequenceParts, Watermark};

/// How long the kit's files have to import before it is given up.
const IMPORT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

impl KlippApp {
    /// Take the kit `name` into the open sequence, the `parts` ticked.
    pub(super) fn apply_brand_kit(&mut self, name: &str, parts: BrandKitParts) {
        let found = match klipp_project::brand_kit::get(klipp_timeline::template_places::data_root().as_deref(), name) {
            Ok(f) => f,
            Err(e) => return self.state.toast.show(format!("Brand kit: {e}")),
        };
        let kit = &found.kit;
        // Its own looks are read before anything is changed.
        for style in kit.title_styles() {
            if let Err(e) = klipp_document::title_style::resolve_motion(&style) {
                return self.state.toast.show(format!("Brand kit \"{}\": the look \"{}\": {e}", kit.name, style.name));
            }
        }
        let seq = self.state.sequence();
        let brief = parts.look.then(|| kit.into_brief(&seq.brief.clone().unwrap_or_default()));
        let captions = if parts.captions { kit.caption_look() } else { None };
        let look = look_edits(seq, brief, captions.as_ref());
        let mut said = Vec::new();
        if parts.look {
            said.push(format!("{} title look{}", kit.title_styles().len(), if kit.title_styles().len() == 1 { "" } else { "s" }));
        }
        if look.caption_tracks > 0 {
            said.push(format!("captions on {} track{}", look.caption_tracks, if look.caption_tracks == 1 { "" } else { "s" }));
        }
        if !look.edits.is_empty() {
            self.apply_edits_as_one(&look.edits, &format!("Apply Brand Kit {}", kit.name));
            self.state.toast.show(format!("Brand kit \"{}\": {}", kit.name, said.join(", ")));
        }
        if !(parts.intro || parts.outro || parts.watermark) {
            return;
        }
        // The kit's files are imported first; the parts wait for them.
        let paths: Vec<std::path::PathBuf> = kit_files(&found, parts).into_iter().filter(|p| self.state.media_pool.find_by_path(p).is_none()).collect();
        let imports: Vec<AppCommand> = paths.into_iter().map(|path| AppCommand::ImportMedia { path }).collect();
        self.send_commands(imports);
        self.state.pending_brand_kit = Some(PendingBrandKit { found, parts, sequence: self.state.sequence().id, until: std::time::Instant::now() + IMPORT_WAIT });
        self.advance_brand_kit();
    }

    /// The waiting kit's intro, outro and logo placed once its files have
    /// imported, as one undo step; given up after a while, or when its
    /// sequence is no longer the open one.
    pub(super) fn advance_brand_kit(&mut self) {
        let Some(pending) = self.state.pending_brand_kit.as_ref() else { return };
        let name = pending.found.kit.name.clone();
        if pending.sequence != self.state.sequence().id {
            self.state.pending_brand_kit = None;
            return;
        }
        let ready = kit_files(&pending.found, pending.parts).iter().all(|p| self.state.media_pool.find_by_path(p).is_some_and(|m| m.resolution.width > 0));
        if !ready {
            if std::time::Instant::now() > pending.until {
                self.state.pending_brand_kit = None;
                self.state.toast.show(format!("Brand kit \"{name}\": its files did not finish importing; try again"));
            }
            return;
        }
        let Some(pending) = self.state.pending_brand_kit.take() else { return };
        match brand_kit_ops(&self.state, &pending) {
            Ok(ops) if ops.is_empty() => self.state.toast.show(format!("Brand kit \"{name}\" has no intro, outro or logo")),
            Ok(ops) => self.state.pending_doc_ops.push(crate::state::DocOpsRequest { label: format!("Brand Kit {name} Intro, Outro and Logo"), ops, independent: false }),
            Err(e) => self.state.toast.show(format!("Brand kit \"{name}\": {e}")),
        }
    }
}

/// The ops that put the kit's intro, outro and logo on the open
/// sequence, its composition templates filled in.
fn brand_kit_ops(state: &crate::state::AppState, pending: &PendingBrandKit) -> Result<Vec<klipp_document::ops::Op>, String> {
    use klipp_document::ops::{Op, PlacedComposition};
    let (kit, parts) = (&pending.found.kit, pending.parts);
    let store = &state.composition_templates;
    let pool = &state.media_pool;
    let file = |name: &str| pending.found.dir.as_ref().map(|d| d.join(name)).ok_or_else(|| format!("its {name} has no folder to come from"));
    let bookend = |b: &klipp_project::brand_kit::Bookend| -> Result<(Bookend, f64), String> {
        match (&b.template, &b.file) {
            (Some(name), _) => {
                let t = store.find(name, None).ok_or_else(|| format!("there is no composition template \"{name}\""))?;
                let len = t.sequences.iter().find(|q| q.id.0 == t.composition_id).map(|q| q.total_duration().to_secs_f64()).filter(|l| *l > 0.0).ok_or_else(|| format!("the composition template \"{name}\" has no length"))?;
                Ok((Bookend::Template(t.name.clone()), len))
            }
            (None, Some(f)) => {
                let m = pool.find_by_path(&file(f)?).ok_or_else(|| format!("{f} is not imported"))?;
                Ok((Bookend::Media(m.id), m.duration.to_secs_f64()))
            }
            _ => Err("its intro or outro is neither a template nor a file".into()),
        }
    };
    let mut found = SequenceParts::default();
    if parts.intro {
        found.intro = kit.intro.as_ref().map(bookend).transpose()?;
    }
    if parts.outro {
        found.outro = kit.outro.as_ref().map(bookend).transpose()?;
    }
    if parts.watermark {
        if let Some(logo) = &kit.logo {
            let m = pool.find_by_path(&file(&logo.file)?).ok_or_else(|| format!("{} is not imported", logo.file))?;
            let (left, top) = logo.corner.sides();
            found.watermark = Some((Watermark { left, top, margin: logo.margin, scale: logo.scale, opacity: logo.opacity }, m.id, (m.resolution.width, m.resolution.height)));
        }
    }
    let mut ops = sequence_ops(state.sequence(), &found)?;
    // A composition template's sequences go with it, copied into the
    // project when it hasn't them.
    for op in &mut ops {
        if let Op::Template { name, composition, .. } = op {
            let t = store.find(name, None).ok_or_else(|| format!("there is no composition template \"{name}\""))?;
            let missing = klipp_timeline::composition_template::missing_sequences(t, &state.sequences);
            *composition = Some(Box::new(PlacedComposition { template: t.clone(), missing }));
        }
    }
    Ok(ops)
}

/// The files in the kit's folder the `parts` need: the logo for the
/// watermark, an intro's or outro's clip.
fn kit_files(found: &klipp_project::brand_kit::Found, parts: BrandKitParts) -> Vec<std::path::PathBuf> {
    let Some(dir) = &found.dir else { return Vec::new() };
    let kit = &found.kit;
    let mut names: Vec<&str> = Vec::new();
    if parts.watermark {
        names.extend(kit.logo.as_ref().map(|l| l.file.as_str()));
    }
    for (wanted, b) in [(parts.intro, &kit.intro), (parts.outro, &kit.outro)] {
        if wanted {
            names.extend(b.as_ref().and_then(|b| b.file.as_deref()));
        }
    }
    names.into_iter().map(|n| dir.join(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's way to a kit's logo: the kit's file found in the media
    /// pool once imported, and the watermark's three ops built for the
    /// open sequence, held for the length of what is on it.
    #[test]
    fn a_kit_logo_in_the_pool_becomes_the_watermark() {
        let dir = std::env::temp_dir().join(format!("klipp-brand-kit-app-{}", uuid::Uuid::new_v4()));
        let mut kit = klipp_project::brand_kit::BrandKit::new("Acme");
        kit.logo = Some(klipp_project::brand_kit::Logo { file: "mark.png".into(), corner: klipp_project::brand_kit::Corner::TopRight, margin: 0.03, scale: 0.12, opacity: 0.8 });
        let found = klipp_project::brand_kit::Found { kit, source: klipp_project::brand_kit::Source::Mine, dir: Some(dir.clone()) };
        let parts = BrandKitParts { look: false, captions: false, intro: false, outro: false, watermark: true };
        assert_eq!(kit_files(&found, parts), vec![dir.join("mark.png")]);
        assert!(kit_files(&found, BrandKitParts::default()).is_empty(), "the look needs no file");
        let mut state = crate::state::AppState::default();
        let mut logo = klipp_media::MediaItem::new_synthetic_title(klipp_core::MediaId::new(), "mark.png".into(), klipp_core::Timecode::from_secs_f64(5.0, 24), klipp_core::Fps::new(24, 1));
        logo.path = dir.join("mark.png");
        logo.resolution = klipp_core::Resolution { width: 200, height: 100 };
        let logo_id = state.media_pool.add(logo);
        let clip = klipp_timeline::test_clip::TestClip::media(8.0).build();
        state.sequences[state.active_sequence_idx].tracks[0].clips.push(clip);
        let pending = PendingBrandKit { found, parts, sequence: state.sequence().id, until: std::time::Instant::now() };
        let ops: Vec<serde_json::Value> = brand_kit_ops(&state, &pending).unwrap().iter().map(|o| serde_json::to_value(o).unwrap()).collect();
        assert_eq!(ops.len(), 3, "{ops:?}");
        assert_eq!(ops[1]["media_id"].as_str(), Some(logo_id.0.to_string().as_str()));
        assert!((ops[1]["source_out"].as_f64().unwrap() - 8.0).abs() < 1e-6, "{}", ops[1]);
        let x = ops[2]["poses"][0]["x"].as_f64().unwrap();
        assert!(x > 0.5, "top right: {x}");
    }
}
