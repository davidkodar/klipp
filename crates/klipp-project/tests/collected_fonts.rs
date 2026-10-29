//! A collected project carries the fonts its titles use (#3009): the
//! files and licence are copied into its folder and listed by relative
//! path, and opened where the family is not installed (its source folder
//! gone), its titles draw in its own copy.
//!
//! Its own binary: it sets the process's project fonts.

use std::path::{Path, PathBuf};

use klipp_core::Timecode;
use klipp_media::MediaPool;
use klipp_project::consolidate::{consolidate_project, ConsolidateOptions};
use klipp_project::project_fonts::{load_fonts, CollectFont};
use klipp_project::{save_project_with_color, ColorSettings};
use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("klipp-collected-fonts-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

/// A bundled face renamed: the same glyphs as `Bebas Neue` under a family
/// no machine has, so only the collected copy can draw it. The names are
/// swapped letter for letter, so every offset in the file stays.
fn renamed_face() -> Vec<u8> {
    let mut bytes = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/BebasNeue/BebasNeue-Regular.ttf")).unwrap();
    let swap = |bytes: &mut Vec<u8>, from: &[u8], to: &[u8]| {
        let mut i = 0;
        while i + from.len() <= bytes.len() {
            if &bytes[i..i + from.len()] == from {
                bytes[i..i + from.len()].copy_from_slice(to);
                i += from.len();
            } else {
                i += 1;
            }
        }
    };
    let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_be_bytes).collect::<Vec<u8>>();
    swap(&mut bytes, b"Bebas", b"Zebas");
    swap(&mut bytes, &utf16("Bebas"), &utf16("Zebas"));
    bytes
}

fn title(family: &str) -> TitleData {
    TitleData::single_text(TextBox { text: "COLLECTED".into(), font_name: family.into(), font_size: 160.0, color: [255, 255, 255, 255], ..Default::default() })
}

fn render(family: &str) -> Vec<u8> {
    klipp_title::TitleRenderer::new().render(&title(family), 480, 270)
}

#[test]
fn a_collected_project_draws_its_titles_in_its_own_fonts() {
    let (system, source, dest) = (tmp("system"), tmp("source"), tmp("dest"));
    std::fs::create_dir_all(&system).unwrap();
    std::fs::create_dir_all(&source).unwrap();
    // "Installed" here only: the face and its licence.
    std::fs::write(system.join("ZebasNeue-Regular.ttf"), renamed_face()).unwrap();
    std::fs::write(system.join("OFL.txt"), b"SIL Open Font License 1.1").unwrap();
    let family = klipp_title::font_discovery::scan_folder(&system).into_keys().next().expect("the renamed face reads");
    assert_eq!(family, "Zebas Neue");

    let mut seq = Sequence::new_default();
    let mut track = Track::new("T1", TrackKind::Text);
    track.clips = vec![Clip { kind: ClipKind::Title(title("Zebas Neue")), ..Clip::gap(Timecode::from_frames(0, 24), Timecode::from_frames(48, 24)) }];
    seq.tracks.push(track);
    save_project_with_color(&source, std::slice::from_ref(&seq), &MediaPool::new(), &ColorSettings::default(), None, None, "").unwrap();
    assert_eq!(klipp_project::project_fonts::fonts_used(std::slice::from_ref(&seq)), ["Zebas Neue"]);

    let fonts = vec![
        CollectFont { family: "Zebas Neue".into(), source: Ok((vec![system.join("ZebasNeue-Regular.ttf")], system.join("OFL.txt"))) },
        CollectFont { family: "Locked Face".into(), source: Err("its licence does not allow embedding".into()) },
    ];
    let options = ConsolidateOptions { dest_dir: dest.clone(), include_proxies: false, include_skims: false, mode: Default::default(), looks: Vec::new(), fonts };
    let result = consolidate_project(&source, &MediaPool::new(), &[seq], &ColorSettings::default(), options, None).expect("collected");
    assert_eq!(result.fonts_copied, ["Zebas Neue"]);
    assert!(result.warnings.iter().any(|w| w == "font Locked Face not copied: its licence does not allow embedding"), "{:?}", result.warnings);
    assert!(dest.join("fonts/zebas-neue/ZebasNeue-Regular.ttf").is_file() && dest.join("fonts/zebas-neue/OFL.txt").is_file());
    let list = std::fs::read_to_string(dest.join("fonts.json")).unwrap();
    assert!(list.contains("\"fonts/zebas-neue/ZebasNeue-Regular.ttf\"") && !list.contains(system.to_str().unwrap()), "relative paths only: {list}");

    // Opened elsewhere: the family is nowhere but in the project.
    std::fs::remove_dir_all(&system).unwrap();
    let without = render("Zebas Neue");
    klipp_title::catalogue::set_project_fonts(load_fonts(&dest).expect("the list reads"));
    let with = render("Zebas Neue");
    let bebas = render("Bebas Neue");
    klipp_title::catalogue::set_project_fonts(Vec::new());
    let ink = |p: &[u8]| p.chunks_exact(4).filter(|p| p[3] > 128).count();
    assert!(ink(&with) > 1000, "the title draws: {}", ink(&with));
    assert_eq!(with, bebas, "drawn in the project's copy, the same glyphs as the face it was made from");
    assert_ne!(with, without, "without the project's copy it falls back to another face");
    assert!(Path::new(&dest).join("fonts.json").is_file());
}
