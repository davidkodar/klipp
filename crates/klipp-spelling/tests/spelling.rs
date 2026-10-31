//! Spelling over a sequence's titles and captions (#3236), against the
//! small dictionaries in `tests/dictionaries`, and hostile dictionaries
//! refused.

use std::path::{Path, PathBuf};

use klipp_core::Timecode;
use klipp_spelling::{check_sequence, dictionaries, Dictionaries, Known, Missing};
use klipp_timeline::{Brief, CastMember, Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dictionaries")
}

fn dicts() -> Dictionaries {
    Dictionaries::in_dirs(vec![fixtures()], None)
}

fn title(text: &str, at: f64, language: Option<&str>) -> Clip {
    let mut c = Clip::gap(Timecode::from_secs_f64(at, 25), Timecode::from_secs_f64(at + 1.0, 25));
    c.kind = ClipKind::Title(TitleData::single_text(TextBox { text: text.into(), language: language.map(Into::into), ..Default::default() }));
    c
}

fn sequence(tracks: Vec<Track>, brief: Option<Brief>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.tracks = tracks;
    seq.brief = brief;
    seq
}

fn track(kind: TrackKind, language: Option<&str>, clips: Vec<Clip>) -> Track {
    let mut t = Track::new("T", kind);
    t.language = language.map(Into::into);
    t.clips = clips;
    t
}

fn marked(report: &klipp_spelling::Report) -> Vec<(&str, &str)> {
    report.misspellings.iter().map(|m| (m.word.as_str(), m.language.as_str())).collect()
}

#[test]
fn a_misspelt_word_is_marked_with_where_it_is_and_what_it_might_be() {
    let seq = sequence(vec![track(TrackKind::Text, None, vec![title("Teh quick brown fox jumps", 0.0, None)])], None);
    let report = check_sequence(&seq, &Known::default(), &dicts(), "en", 5);
    assert_eq!(marked(&report), [("Teh", "en")]);
    let m = &report.misspellings[0];
    assert_eq!((m.start, m.end, m.track), (0, 3, 0));
    assert_eq!(m.suggestions.first().map(String::as_str), Some("The"), "the dictionary's REP, in the word's case");
    assert!(report.missing.is_empty());
    let none = check_sequence(&seq, &Known::default(), &dicts(), "en", 0);
    assert!(none.misspellings[0].suggestions.is_empty(), "no suggestions when none asked for");
}

#[test]
fn each_text_is_checked_in_its_own_language() {
    // The title in English by the brief, a caption track in Swedish, and
    // one title on it set to English itself.
    let brief = Brief { language: Some("en".into()), ..Default::default() };
    let seq = sequence(
        vec![
            track(TrackKind::Text, None, vec![title("Meet at the harbour at dawn", 0.0, None)]),
            track(TrackKind::Subtitle, Some("sv"), vec![title("God morgon, välkommen till hamnen", 1.0, None), title("Recieve the dog", 2.0, Some("en-US")), title("God mornig", 3.0, None)]),
        ],
        Some(brief),
    );
    let report = check_sequence(&seq, &Known::default(), &dicts(), "de", 1);
    assert_eq!(marked(&report), [("Recieve", "en-US"), ("mornig", "sv")], "Swedish words not marked as English, nor English as Swedish");
    assert!(report.missing.is_empty(), "the fallback language is not reached: {:?}", report.missing);
}

#[test]
fn the_projects_own_words_and_the_persons_are_not_marked() {
    let brief = Brief { vocabulary: vec!["Omarchy".into()], cast: vec![CastMember { name: "Ana Ruiz".into(), ..Default::default() }], ..Default::default() };
    let seq = sequence(vec![track(TrackKind::Text, None, vec![title("Ana Ruiz meets the OMARCHY dog at Kestrelholm with Hyprland", 0.0, None)])], Some(brief.clone()));
    let known = Known::new(seq.brief.as_ref(), ["kestrelholm_take_04.mov"]);
    let report = check_sequence(&seq, &known, &dicts(), "en", 0);
    assert_eq!(marked(&report), [("meets", "en"), ("with", "en"), ("Hyprland", "en")], "the cast, the vocabulary in any case, a file name's word");

    let dir = std::env::temp_dir().join(format!("klipp-spelling-known-{}", std::process::id()));
    let list = dir.join("dictionary.txt");
    let _ = std::fs::remove_file(&list);
    klipp_spelling::personal::learn(&list, "Hyprland").unwrap();
    let mut known = known;
    for w in klipp_spelling::personal::read(&list) {
        known.add(&w);
    }
    assert_eq!(marked(&check_sequence(&seq, &known, &dicts(), "en", 0)), [("meets", "en"), ("with", "en")], "a word on the person's list");
}

#[test]
fn numbers_addresses_and_other_scripts_are_not_marked() {
    let seq = sequence(vec![track(TrackKind::Text, None, vec![title("The 4K dog at klipp.app, @anaruiz #dawn — Καλημέρα 東京 don\u{2019}t", 0.0, None)])], None);
    assert!(check_sequence(&seq, &Known::default(), &dicts(), "en", 0).misspellings.is_empty());
}

#[test]
fn a_language_with_no_dictionary_is_said_once_and_its_text_left_alone() {
    let seq = sequence(vec![track(TrackKind::Text, Some("de"), vec![title("Guten Morgen", 0.0, None), title("Gute Nacht", 1.0, None)])], None);
    let report = check_sequence(&seq, &Known::default(), &dicts(), "en", 0);
    assert!(report.misspellings.is_empty());
    assert_eq!(report.missing.len(), 1, "{:?}", report.missing);
    assert!(matches!(&report.missing[0], Missing::NotInstalled { language, packages } if language == "de" && packages.contains("hunspell-de")));
}

#[test]
fn a_tag_is_only_ever_made_into_these_file_names() {
    assert_eq!(dictionaries::file_stems("en", Some("GB")).unwrap(), ["en_GB", "en_US", "en"]);
    assert_eq!(dictionaries::file_stems("de-AT", None).unwrap(), ["de_AT", "de_DE", "de"]);
    assert_eq!(dictionaries::file_stems("sv", None).unwrap(), ["sv_SE", "sv"]);
    for bad in ["../../etc/passwd", "en/../x", "en_GB_x", "e", "engl", "en-G1", "", "en_US.aff", "/usr/share/hunspell/en_US"] {
        assert!(dictionaries::file_stems(bad, None).is_none(), "{bad:?} is not a tag");
    }
    let seq = sequence(vec![track(TrackKind::Text, Some("../../tests/dictionaries/en_US"), vec![title("Teh", 0.0, None)])], None);
    let report = check_sequence(&seq, &Known::default(), &dicts(), "en", 0);
    assert!(report.misspellings.is_empty());
    assert!(matches!(&report.missing[..], [Missing::Refused { .. }]), "{:?}", report.missing);
    assert_eq!(dictionaries::locale_region(Some("en_GB.UTF-8")).as_deref(), Some("GB"));
    assert_eq!(dictionaries::locale_language(Some("sv_SE.UTF-8")).as_deref(), Some("sv"));
    assert_eq!(dictionaries::locale_language(Some("C")), None);
}

/// A folder of its own holding one dictionary pair.
fn pair(name: &str, aff: &[u8], dic: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp-spelling-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("xx_XX.aff"), aff).unwrap();
    std::fs::write(dir.join("xx_XX.dic"), dic).unwrap();
    dir
}

fn refused(dir: PathBuf) -> String {
    match Dictionaries::in_dirs(vec![dir], None).get("xx-XX") {
        Err(Missing::Refused { why, .. }) => why,
        Err(other) => panic!("not refused: {other:?}"),
        Ok(_) => panic!("read"),
    }
}

#[test]
fn a_dictionary_in_another_encoding_is_read_in_it() {
    // "café" in ISO 8859-1: é is the one byte E9.
    let dir = pair("latin1", b"SET ISO8859-1\nTRY e\n", b"1\ncaf\xe9\n");
    let d = Dictionaries::in_dirs(vec![dir], None).get("xx-XX").expect("read");
    assert!(d.checker.check("café"));
    assert!(!d.checker.check("cafe"));
}

#[test]
fn hostile_dictionaries_are_refused_with_why() {
    let why = refused(pair("set", b"SET KLINGON-1\n", b"1\nqapla\n"));
    assert!(why.contains("KLINGON-1"), "{why}");
    let why = refused(pair("utf8", b"SET UTF-8\n", b"1\ncaf\xe9\n"));
    assert!(why.contains("not UTF-8"), "{why}");
    let why = refused(pair("parse", b"FLAG num\nSFX 1 Y notanumber\n", b"1\nword/1\n"));
    assert!(why.contains("does not read"), "{why}");

    // Past the cap: a sparse file, so nothing that size is written.
    let dir = pair("big", b"SET UTF-8\n", b"1\nword\n");
    std::fs::File::options().write(true).open(dir.join("xx_XX.dic")).unwrap().set_len(dictionaries::MAX_DIC_BYTES + 1).unwrap();
    let why = refused(dir);
    assert!(why.contains("more than"), "{why}");
    let dir = pair("bigaff", b"SET UTF-8\n", b"1\nword\n");
    std::fs::File::options().write(true).open(dir.join("xx_XX.aff")).unwrap().set_len(dictionaries::MAX_AFF_BYTES + 1).unwrap();
    assert!(refused(dir).contains("more than"));
}
