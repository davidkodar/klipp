//! What a collected project needs and can't carry (#3241): outside audio
//! and picture plug-ins, with their files and versions where they can be
//! read; fonts whose licence or absence keeps them out; media that is
//! offline; and models fetched on request. Each is said with where the
//! project uses it, so the person receiving the folder knows what to
//! install.

use std::collections::BTreeMap;

use klipp_core::AudioEffectInstance;
use klipp_effects::BuiltinEffect;
use klipp_media::{MediaItemKind, MediaPool};
use klipp_timeline::{Clip, Sequence};
use serde::Serialize;

/// What kind of thing is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    AudioPlugin,
    PicturePlugin,
    Font,
    OfflineMedia,
    Model,
}

/// One thing the copy needs and doesn't carry.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Dependency {
    pub need: Need,
    /// As the project names it: a plug-in's id, a font's family, a file's
    /// path, a model's id.
    pub id: String,
    /// A plug-in's name, file and version where they are known; why a
    /// font can't go; what a model is for.
    pub detail: String,
    /// Where the project uses it, in words: "Main: clip Interview",
    /// "Main: track A1", "Main: bus Reverb", "Main: a grade".
    pub used_in: Vec<String>,
}

/// What a plug-in is, as the hosts on this machine find it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Described {
    pub name: String,
    pub version: Option<String>,
    pub file: Option<String>,
}

/// A clip as a person finds it: its name, else where it starts.
fn clip_name(c: &Clip) -> String {
    c.name.clone().unwrap_or_else(|| format!("at {:.1} s", c.timeline_in.to_secs_f64()))
}

/// An outside audio plug-in's id: LV2, CLAP or LADSPA.
fn outside_audio(effect: &AudioEffectInstance) -> bool {
    ["lv2:", "clap:", "ladspa:"].iter().any(|p| effect.effect_type.starts_with(p))
}

/// A frei0r effect's id: the outside picture plug-in it runs.
fn frei0r_id(effect: &BuiltinEffect) -> Option<&str> {
    match effect {
        BuiltinEffect::Frei0r { type_id, .. } => Some(type_id),
        _ => None,
    }
}

/// The model `effect` needs fetched, by id, and what for.
fn audio_model(effect: &AudioEffectInstance) -> Option<(&'static str, &'static str)> {
    (effect.effect_type == "voice_isolation").then_some(("deepfilternet3", "Voice Isolation (without it the sound passes through)"))
}

fn matte_model(m: klipp_nodes::SubjectMatte) -> (&'static str, &'static str) {
    match m {
        klipp_nodes::SubjectMatte::Subject => ("rf_detr_seg_medium_v1", "the subject matte"),
        klipp_nodes::SubjectMatte::Foreground => ("birefnet_lite_v1", "the foreground matte"),
    }
}

/// Everything `sequences` need that a collected folder can't carry: the
/// plug-ins (described by `describe`, given each id), the models, the
/// media in `pool` that is offline, and the `fonts` left out with why.
pub fn report(sequences: &[Sequence], pool: &MediaPool, fonts_left_out: &[(String, String)], describe: &dyn Fn(&str) -> Option<Described>) -> Vec<Dependency> {
    let mut found: BTreeMap<(Need, String), (String, Vec<String>)> = BTreeMap::new();
    let mut add = |need: Need, id: &str, detail: String, at: String| {
        let entry = found.entry((need, id.to_string())).or_insert_with(|| (detail, Vec::new()));
        if !entry.1.contains(&at) {
            entry.1.push(at);
        }
    };
    let plugin_detail = |id: &str| match describe(id) {
        Some(d) => [Some(d.name), d.version.map(|v| format!("version {v}")), d.file].into_iter().flatten().collect::<Vec<_>>().join(", "),
        None => "not installed here".to_string(),
    };
    for seq in sequences {
        let mut audio = |effects: &[AudioEffectInstance], at: String| {
            for e in effects {
                if outside_audio(e) {
                    add(Need::AudioPlugin, &e.effect_type, plugin_detail(&e.effect_type), at.clone());
                }
                if let Some((model, what)) = audio_model(e) {
                    add(Need::Model, model, what.to_string(), at.clone());
                }
            }
        };
        for track in &seq.tracks {
            audio(&track.effects.audio, format!("{}: track {}", seq.name, track.name));
            for clip in &track.clips {
                audio(&clip.audio_effects, format!("{}: clip {}", seq.name, clip_name(clip)));
            }
        }
        for bus in &seq.buses {
            audio(&bus.effects, format!("{}: bus {}", seq.name, bus.name));
        }
        for track in &seq.tracks {
            for id in track.effects.video_effects().iter().filter_map(frei0r_id) {
                add(Need::PicturePlugin, id, plugin_detail(id), format!("{}: track {}", seq.name, track.name));
            }
            for clip in &track.clips {
                let at = format!("{}: clip {}", seq.name, clip_name(clip));
                for e in clip.picture_effects().unwrap_or_default() {
                    if let Some(id) = frei0r_id(&e.effect) {
                        add(Need::PicturePlugin, id, plugin_detail(id), at.clone());
                    }
                }
                for o in clip.picture_ofx().unwrap_or_default() {
                    add(Need::PicturePlugin, &o.entry.plugin_name, plugin_detail(&o.entry.plugin_name), at.clone());
                }
            }
        }
        for graph in seq.grades() {
            let at = format!("{}: a grade", seq.name);
            let mut level = vec![graph];
            while let Some(g) = level.pop() {
                for n in &g.nodes {
                    for id in n.params.effects.iter().filter_map(frei0r_id) {
                        add(Need::PicturePlugin, id, plugin_detail(id), at.clone());
                    }
                    if let Some(ofx) = &n.params.ofx {
                        add(Need::PicturePlugin, &ofx.plugin, plugin_detail(&ofx.plugin), at.clone());
                    }
                    if let Some(m) = n.params.matte_subject {
                        let (model, what) = matte_model(m);
                        add(Need::Model, model, what.to_string(), at.clone());
                    }
                    if let Some(inner) = &n.params.compound_subgraph {
                        level.push(inner);
                    }
                }
            }
        }
    }
    for item in pool.all().filter(|i| i.kind == MediaItemKind::File && (i.offline || !i.path.exists())) {
        let users: Vec<String> = sequences
            .iter()
            .flat_map(|s| s.all_clips().filter(|c| c.media_id() == Some(item.id)).map(move |c| format!("{}: clip {}", s.name, clip_name(c))))
            .collect();
        let at = if users.is_empty() { vec!["the media pool".to_string()] } else { users };
        for u in at {
            add(Need::OfflineMedia, &item.path.to_string_lossy(), item.filename.clone(), u);
        }
    }
    for (family, why) in fonts_left_out {
        add(Need::Font, family, why.clone(), "titles".into());
    }
    found.into_iter().map(|((need, id), (detail, used_in))| Dependency { need, id, detail, used_in }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_nodes::{Node, NodeGraph, NodeId, NodeKind, NodeParams};
    use klipp_timeline::GradeAccess as _;

    fn effect(id: &str) -> AudioEffectInstance {
        AudioEffectInstance { effect_type: id.into(), enabled: true, wet_dry: 1.0, params: Vec::new() }
    }

    /// #3241: each kind the copy can't carry is said once, with where it
    /// is used: an outside audio plug-in on a clip, a track and a bus (its
    /// name and version as the host finds it, or that it isn't installed),
    /// Voice Isolation's model, a frei0r and an OFX node and a subject
    /// matte in a grade, an offline file and the clip using it, a font
    /// left out; a built-in effect is not.
    #[test]
    fn what_the_copy_cannot_carry_is_said_with_where() {
        let t = |f| RationalTime::from_frames(f, 24);
        let mut seq = Sequence::new_default();
        seq.name = "Main".into();
        let offline = MediaId::new();
        let mut clip = Clip::media(offline, t(0), t(24), t(0), t(24));
        clip.name = Some("Interview".into());
        clip.audio_effects = vec![effect("lv2:http://calf.sourceforge.net/plugins/Reverb"), effect("voice_isolation"), effect("compressor")];
        let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
        let mut grade = NodeGraph::default();
        grade.nodes.push(node(NodeKind::Frei0r, NodeParams { effects: vec![BuiltinEffect::Frei0r { type_id: "frei0r:/usr/lib/frei0r-1/glow.so:Glow".into(), params: Default::default() }], ..Default::default() }));
        grade.nodes.push(node(NodeKind::Ofx, NodeParams { ofx: Some(klipp_nodes::plugin::OfxNodeParams { plugin: "com.example.Blur".into(), values: Default::default(), filter: true }), ..Default::default() }));
        grade.nodes.push(node(NodeKind::ExternalMatte, NodeParams { matte_subject: Some(klipp_nodes::SubjectMatte::Subject), ..Default::default() }));
        clip.set_grade(grade);
        seq.tracks[0].clips.push(clip);
        seq.tracks[0].name = "V1".into();
        seq.tracks[0].effects.audio = vec![effect("ladspa:/usr/lib/ladspa/amp.so:amp_mono")];
        let mut bus = klipp_timeline::AuxBus::new("Verb");
        bus.effects = vec![effect("lv2:http://calf.sourceforge.net/plugins/Reverb")];
        seq.buses.push(bus);
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/nowhere/interview.mov".into(), None);
        (item.id, item.offline) = (offline, true);
        pool.add(item);
        let describe = |id: &str| id.starts_with("lv2:").then(|| Described { name: "Calf Reverb".into(), version: Some("0.90".into()), file: None });
        let got = report(&[seq], &pool, &[("Brand Sans".into(), "its licence does not allow embedding".into())], &describe);
        let row = |need: Need, id: &str| got.iter().find(|d| d.need == need && d.id == id).unwrap_or_else(|| panic!("{need:?} {id}: {got:#?}"));
        let reverb = row(Need::AudioPlugin, "lv2:http://calf.sourceforge.net/plugins/Reverb");
        assert_eq!((reverb.detail.as_str(), reverb.used_in.as_slice()), ("Calf Reverb, version 0.90", &["Main: clip Interview".to_string(), "Main: bus Verb".to_string()][..]));
        assert_eq!(row(Need::AudioPlugin, "ladspa:/usr/lib/ladspa/amp.so:amp_mono").used_in, ["Main: track V1"]);
        assert_eq!(row(Need::AudioPlugin, "ladspa:/usr/lib/ladspa/amp.so:amp_mono").detail, "not installed here");
        assert_eq!(row(Need::Model, "deepfilternet3").used_in, ["Main: clip Interview"]);
        assert_eq!(row(Need::PicturePlugin, "frei0r:/usr/lib/frei0r-1/glow.so:Glow").used_in, ["Main: a grade"]);
        row(Need::PicturePlugin, "com.example.Blur");
        row(Need::Model, "rf_detr_seg_medium_v1");
        assert_eq!(row(Need::OfflineMedia, "/nowhere/interview.mov").used_in, ["Main: clip Interview"]);
        assert_eq!(row(Need::Font, "Brand Sans").detail, "its licence does not allow embedding");
        assert!(!got.iter().any(|d| d.id == "compressor"), "a built-in effect travels with the project");
        assert_eq!(got.len(), 8);
    }
}
