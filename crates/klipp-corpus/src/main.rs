//! Reproducible media and project inputs for the saved-project pixel checkpoint.
mod atlas;
mod cases;
mod checks;
mod media;
mod pixels;
mod plugins;
mod projects;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    cases: Vec<String>,
    stream_metadata: BTreeMap<String, serde_json::Value>,
    video_frames: BTreeMap<String, Vec<String>>,
    audio_pcm_sha256: String,
    project_sha256: BTreeMap<String, String>,
    plugin_sources_sha256: BTreeMap<String, String>,
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// Never remove an output tree: even a marker is not permission to recursively
// delete it. Each run has a fresh child; failed inputs remain available to inspect.
fn run_directory(root: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let run = root.join(format!("run-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&run).map_err(|e| e.to_string())?;
    std::fs::write(
        run.join(".klipp-scratch"),
        b"klipp-corpus generated inputs\n",
    )
    .map_err(|e| e.to_string())?;
    std::fs::canonicalize(run).map_err(|e| e.to_string())
}

fn generate(directory: &Path) -> Result<Manifest, String> {
    media::generate(&directory.join("media"))?;
    Ok(Manifest {
        schema: 2,
        cases: projects::CASES.iter().map(|s| s.to_string()).collect(),
        stream_metadata: media::VIDEOS
            .iter()
            .map(|(name, _)| *name)
            .chain(["sync.wav"])
            .map(|name| {
                media::metadata(&directory.join("media").join(name))
                    .map(|value| (name.to_owned(), value))
            })
            .collect::<Result<_, _>>()?,
        video_frames: media::VIDEOS
            .iter()
            .map(|(name, _)| {
                media::decoded_video(&directory.join("media").join(name))
                    .map(|rows| ((*name).to_owned(), rows))
            })
            .collect::<Result<_, _>>()?,
        audio_pcm_sha256: media::decoded_audio(&directory.join("media/sync.wav"))?,
        project_sha256: projects::generate(directory)?,
        plugin_sources_sha256: plugins::generate(directory)?,
    })
}

fn compare(expected: &Manifest, actual: &Manifest) -> Result<(), String> {
    if expected.schema != actual.schema || expected.cases != actual.cases {
        return Err("corpus schema/case coverage changed".into());
    }
    if expected.plugin_sources_sha256 != actual.plugin_sources_sha256 {
        return Err("native fixture sources changed".into());
    }
    if expected.stream_metadata != actual.stream_metadata {
        return Err(
            "corpus stream metadata changed (codec, dimensions, colour tags, rate or channels)"
                .into(),
        );
    }
    if expected.video_frames.keys().ne(actual.video_frames.keys()) {
        return Err("corpus video file coverage changed".into());
    }
    for (name, wanted) in &expected.video_frames {
        let got = &actual.video_frames[name];
        if wanted.len() != got.len() {
            return Err(format!("{name}: decoded frame count changed"));
        }
        for (frame, (a, b)) in wanted.iter().zip(got).enumerate() {
            if a != b {
                return Err(format!(
                    "{name}: decoded frame {frame} changed\nexpected {a}\nactual   {b}"
                ));
            }
        }
    }
    if expected.audio_pcm_sha256 != actual.audio_pcm_sha256 {
        return Err("sync.wav: decoded PCM changed".into());
    }
    if expected
        .project_sha256
        .keys()
        .ne(actual.project_sha256.keys())
    {
        return Err("corpus project file coverage changed".into());
    }
    for (file, hash) in &expected.project_sha256 {
        if actual.project_sha256.get(file) != Some(hash) {
            return Err(format!("{file}: canonical project bytes changed"));
        }
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().unwrap_or_default();
    if mode == "render-worker" {
        let root = PathBuf::from(
            args.next()
                .ok_or("render worker needs its generated root")?,
        );
        if args.next().is_some() {
            return Err("unexpected render worker argument".into());
        }
        return pixels::worker(&root);
    }
    if !matches!(
        mode.as_str(),
        "generate" | "open" | "reopen" | "pixels" | "all"
    ) {
        return Err("usage: klipp-corpus generate|open|reopen|pixels|all [--out DIRECTORY] [--record REASON (generate or pixels only)]".into());
    }
    let mut out = repository().join("target/klipp-corpus/generated");
    let mut reason = None;
    let mut case = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--case" => {
                let name = args.next().ok_or("--case needs a case name")?;
                if !cases::CASES.iter().any(|c| c.id() == name) {
                    return Err(format!("unknown case: {name}"));
                }
                case = Some(name);
            }
            "--out" => out = PathBuf::from(args.next().ok_or("--out needs a directory")?),
            "--record" => {
                let value = args.next().ok_or("--record needs a reason")?;
                if value.trim().is_empty() || value.starts_with('-') {
                    return Err("--record needs a nonempty reason".into());
                }
                reason = Some(value);
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    if reason.is_some() && !matches!(mode.as_str(), "generate" | "pixels") {
        return Err(
            "--record is only valid with generate or pixels; other checks never update references"
                .into(),
        );
    }
    if case.is_some() && (mode != "pixels" || reason.is_none()) {
        return Err(
            "--case is only valid with pixels --record; all other references remain checked".into(),
        );
    }
    let manifest_path = repository().join("corpus/generated.json");
    // Fail before generation when the committed contract is absent or malformed.
    let expected = if reason.is_none() || mode != "generate" {
        Some(
            serde_json::from_slice::<Manifest>(&std::fs::read(&manifest_path).map_err(|e| {
                format!(
                    "{}: {e}; recording requires --record REASON",
                    manifest_path.display()
                )
            })?)
            .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let directory = run_directory(&out)?;
    println!("Generating {}", directory.display());
    let actual = generate(&directory)?;
    if let Some(reason) = reason.as_ref().filter(|_| mode == "generate") {
        // A reference cannot be recorded from inputs that vary between runs.
        let second = run_directory(&out)?;
        compare(&actual, &generate(&second)?)?;
        let mut bytes = serde_json::to_vec_pretty(&actual).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        std::fs::write(&manifest_path, bytes).map_err(|e| e.to_string())?;
        println!("Recorded deterministic input hashes: {reason}");
    } else {
        compare(expected.as_ref().ok_or("missing input contract")?, &actual)?;
        println!(
            "PASS: {} projects, {} video frames, stereo PCM match",
            actual.cases.len(),
            actual.video_frames.values().map(Vec::len).sum::<usize>()
        );
    }
    if matches!(mode.as_str(), "open" | "reopen" | "all") {
        for case in projects::CASES {
            let project = directory.join("projects").join(format!("{case}.klipp"));
            match mode.as_str() {
                "open" => checks::open(&project),
                "reopen" => checks::reopen(&project, &directory.join("reopened").join(case.id())),
                "all" => checks::open(&project).and_then(|()| {
                    checks::reopen(&project, &directory.join("reopened").join(case.id()))
                }),
                _ => unreachable!(),
            }
            .map_err(|e| format!("{case}: {e}"))?;
            println!("PASS {mode}: {case}");
        }
    }
    if matches!(mode.as_str(), "pixels" | "all") {
        pixels::render(&directory)?;
        let inputs = sha256(&std::fs::read(&manifest_path).map_err(|e| e.to_string())?);
        let reference = repository().join("corpus/references/generated");
        if let Some(reason) = reason {
            let second = run_directory(&out)?;
            compare(&actual, &generate(&second)?)?;
            pixels::render(&second)?;
            atlas::repeatable(&directory.join("pixels"), &second.join("pixels"))?;
            atlas::record(
                &directory.join("pixels"),
                &reference,
                &inputs,
                &reason,
                case.as_deref(),
            )?;
            println!("Recorded pixel references: {reason}");
        } else {
            atlas::compare(&reference, &directory.join("pixels"), &inputs)?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("corpus: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_inputs_reproduce() {
        let expected: Manifest = serde_json::from_slice(
            &std::fs::read(repository().join("corpus/generated.json")).unwrap(),
        )
        .unwrap();
        let directory = run_directory(&std::env::temp_dir().join("klipp-corpus-tests")).unwrap();
        let actual = generate(&directory).unwrap();
        compare(&expected, &actual).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn independent_projects_have_identical_bytes() {
        let parent = std::env::temp_dir().join("klipp-corpus-tests");
        let a = run_directory(&parent).unwrap();
        let b = run_directory(&parent).unwrap();
        let first = projects::generate(&a).unwrap();
        let second = projects::generate(&b).unwrap();
        assert_eq!(
            first, second,
            "project UUIDs, stamps and paths must be reproducible"
        );
        std::fs::remove_dir_all(a).unwrap();
        std::fs::remove_dir_all(b).unwrap();
    }

    fn fixture() -> Manifest {
        Manifest {
            schema: 2,
            cases: vec!["edit".into()],
            stream_metadata: BTreeMap::new(),
            video_frames: BTreeMap::from([(
                "motion.mkv".into(),
                vec!["frame0".into(), "frame1".into()],
            )]),
            audio_pcm_sha256: "pcm".into(),
            project_sha256: BTreeMap::from([("edit/project.json".into(), "project".into())]),
            plugin_sources_sha256: BTreeMap::new(),
        }
    }
    #[test]
    fn planted_changes_are_named() {
        let expected = fixture();
        let mut actual = fixture();
        actual.video_frames.get_mut("motion.mkv").unwrap()[1].push('!');
        assert!(compare(&expected, &actual).unwrap_err().contains("frame 1"));
        let mut actual = fixture();
        actual.project_sha256.clear();
        assert!(compare(&expected, &actual)
            .unwrap_err()
            .contains("coverage"));
        let mut actual = fixture();
        actual.audio_pcm_sha256.push('!');
        assert!(compare(&expected, &actual).unwrap_err().contains("PCM"));
        let mut actual = fixture();
        actual
            .project_sha256
            .insert("edit/project.json".into(), "changed".into());
        assert!(compare(&expected, &actual)
            .unwrap_err()
            .contains("edit/project.json"));
        assert!(compare(&expected, &fixture()).is_ok());
        let mut actual = fixture();
        actual.cases.clear();
        assert!(compare(&expected, &actual)
            .unwrap_err()
            .contains("coverage"));
        let mut actual = fixture();
        actual.stream_metadata.insert(
            "motion.mkv".into(),
            serde_json::json!({"color_space": "bt2020nc"}),
        );
        assert!(compare(&expected, &actual)
            .unwrap_err()
            .contains("metadata"));
    }
}
