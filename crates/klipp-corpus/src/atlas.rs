//! Small, explicit pixel references. Normal checks never write this directory.
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use image::ImageEncoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const BUDGET: usize = 2 * 1024 * 1024;
const WIDTH: u32 = 2048;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Frame {
    path: String,
    width: u32,
    height: u32,
    offset: usize,
    len: usize,
    layout: Option<Value>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema: u32,
    reason: String,
    inputs_sha256: String,
    atlas_sha256: String,
    coverage: Value,
    frames: Vec<Frame>,
}

fn json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn files(root: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let ty = entry.file_type().map_err(|e| e.to_string())?;
            let path = entry.path();
            if ty.is_dir() {
                pending.push(path);
            } else if ty.is_file() && path.extension().is_some_and(|x| x == "png") {
                let key = path
                    .strip_prefix(root)
                    .map_err(|e| e.to_string())?
                    .to_str()
                    .ok_or("a reference path is not UTF-8")?
                    .to_owned();
                found.insert(key, path);
            } else if !ty.is_file() {
                return Err(format!(
                    "{}: links and special files are not corpus frames",
                    path.display()
                ));
            }
        }
    }
    if found.is_empty() {
        return Err("no rendered corpus frames".into());
    }
    Ok(found)
}

fn layout(path: &Path) -> Result<Option<Value>, String> {
    let side = path.with_extension("json");
    if side.is_file() {
        json(&side).map(Some)
    } else {
        Ok(None)
    }
}

fn collect(root: &Path) -> Result<(Vec<Frame>, Vec<u8>), String> {
    let mut index = Vec::new();
    let mut data = Vec::new();
    for (name, path) in files(root)? {
        let picture = image::open(&path)
            .map_err(|e| format!("{name}: {e}"))?
            .to_rgba8();
        index.push(Frame {
            path: name,
            width: picture.width(),
            height: picture.height(),
            offset: data.len(),
            len: picture.as_raw().len(),
            layout: layout(&path)?,
        });
        data.extend_from_slice(picture.as_raw());
    }
    Ok((index, data))
}

/// Deliberately record only after the caller has checked a second independent
/// run. The image hash makes a partially interrupted two-file update fail closed.
pub fn record(
    rendered: &Path,
    reference: &Path,
    inputs: &str,
    reason: &str,
    case: Option<&str>,
) -> Result<(), String> {
    if reason.trim().is_empty() {
        return Err("recording requires a reason".into());
    }
    let (frames, mut data) = collect(rendered)?;
    let coverage = json(&rendered.join("coverage.json"))?;
    if let Some(case) = case {
        preserve_other_cases(reference, &frames, &mut data, &coverage, case)?;
    }
    let rows = data.len().div_ceil(WIDTH as usize);
    data.resize(rows * WIDTH as usize, 0);
    let mut image = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut image,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(
        &data,
        WIDTH,
        u32::try_from(rows).map_err(|e| e.to_string())?,
        image::ExtendedColorType::L8,
    )
    .map_err(|e| e.to_string())?;
    let index = Index {
        schema: 1,
        reason: reason.trim().to_owned(),
        inputs_sha256: inputs.into(),
        atlas_sha256: crate::sha256(&image),
        coverage,
        frames,
    };
    let mut index = serde_json::to_vec_pretty(&index).map_err(|e| e.to_string())?;
    index.push(b'\n');
    if image.len() + index.len() > BUDGET {
        return Err(format!("references need {} bytes, over the {BUDGET}-byte budget; reduce sampled frames, not the one-level bar", image.len() + index.len()));
    }
    std::fs::create_dir_all(reference).map_err(|e| e.to_string())?;
    std::fs::write(reference.join("atlas.png"), image).map_err(|e| e.to_string())?;
    std::fs::write(reference.join("atlas.json"), index).map_err(|e| e.to_string())?;
    Ok(())
}

/// A case update still checks every other case, and keeps its reference bytes
/// exactly (even when a fresh render differs by the permitted one level).
fn preserve_other_cases(
    reference: &Path,
    frames: &[Frame],
    data: &mut [u8],
    coverage: &Value,
    case: &str,
) -> Result<(), String> {
    let index: Index = serde_json::from_slice(
        &std::fs::read(reference.join("atlas.json"))
            .map_err(|e| format!("a case update needs existing references: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    let png = std::fs::read(reference.join("atlas.png")).map_err(|e| e.to_string())?;
    if index.schema != 1 || crate::sha256(&png) != index.atlas_sha256 {
        return Err("existing reference schema or atlas hash changed".into());
    }
    let old = image::load_from_memory(&png)
        .map_err(|e| e.to_string())?
        .to_luma8()
        .into_raw();
    let prefix = format!("{case}/");
    let wanted: BTreeMap<_, _> = index
        .frames
        .iter()
        .filter(|f| !f.path.starts_with(&prefix))
        .map(|f| (f.path.as_str(), f))
        .collect();
    let found: BTreeMap<_, _> = frames
        .iter()
        .filter(|f| !f.path.starts_with(&prefix))
        .map(|f| (f.path.as_str(), f))
        .collect();
    if wanted.keys().ne(found.keys()) || !frames.iter().any(|f| f.path.starts_with(&prefix)) {
        return Err("case update changed other frame coverage or names no rendered case".into());
    }
    let other_coverage = |v: &Value| -> Result<BTreeMap<String, Value>, String> {
        Ok(v.as_object()
            .ok_or("coverage is not an object")?
            .iter()
            .filter(|(k, _)| !k.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    };
    if other_coverage(&index.coverage)? != other_coverage(coverage)? {
        return Err("case update changed another case's route coverage".into());
    }
    for (name, a) in wanted {
        let b = found[name];
        if (a.width, a.height, &a.layout) != (b.width, b.height, &b.layout) {
            return Err(format!("{name}: case update changed another case's layout"));
        }
        let end = a
            .offset
            .checked_add(a.len)
            .ok_or("reference offset overflow")?;
        let expected = old.get(a.offset..end).ok_or("reference atlas is short")?;
        let actual = &mut data[b.offset..b.offset + b.len];
        compare_frame(name, expected, actual)?;
        actual.copy_from_slice(expected);
    }
    Ok(())
}

fn compare_frame(name: &str, expected: &[u8], actual: &[u8]) -> Result<(), String> {
    if expected.len() != actual.len() {
        return Err(format!("{name}: sample count changed"));
    }
    let (worst, _) = klipp_render::pixel_check::sample_difference(
        expected.iter().copied().zip(actual.iter().copied()),
    );
    if worst > 1 {
        return Err(format!("{name}: {worst} levels from its reference (bar 1)"));
    }
    Ok(())
}

pub fn compare(reference: &Path, rendered: &Path, inputs: &str) -> Result<(), String> {
    let text = std::fs::read(reference.join("atlas.json")).map_err(|e| {
        format!("pixel references missing: {e}; recording requires pixels --record REASON")
    })?;
    let index: Index = serde_json::from_slice(&text).map_err(|e| e.to_string())?;
    if index.schema != 1 || index.inputs_sha256 != inputs {
        return Err("pixel reference schema or input contract changed".into());
    }
    let png = std::fs::read(reference.join("atlas.png")).map_err(|e| e.to_string())?;
    if text.len() + png.len() > BUDGET {
        return Err("pixel references exceed the 2 MiB budget".into());
    }
    if crate::sha256(&png) != index.atlas_sha256 {
        return Err("pixel reference atlas hash changed".into());
    }
    let data = image::load_from_memory(&png)
        .map_err(|e| e.to_string())?
        .to_luma8()
        .into_raw();
    let found = files(rendered)?;
    if index.frames.len() != found.len() {
        return Err("rendered frame/route coverage changed".into());
    }
    if index.coverage != json(&rendered.join("coverage.json"))? {
        return Err("rendered route coverage or declines changed".into());
    }
    let mut visited = std::collections::BTreeSet::new();
    for frame in index.frames {
        if !Path::new(&frame.path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
            || !visited.insert(frame.path.clone())
        {
            return Err("invalid or duplicate reference frame path".into());
        }
        let path = found
            .get(&frame.path)
            .ok_or_else(|| format!("{}: frame or route missing", frame.path))?;
        let picture = image::open(path).map_err(|e| e.to_string())?.to_rgba8();
        if picture.dimensions() != (frame.width, frame.height) || layout(path)? != frame.layout {
            return Err(format!("{}: dimensions or YUV layout changed", frame.path));
        }
        let end = frame
            .offset
            .checked_add(frame.len)
            .ok_or("reference offset overflow")?;
        let expected = data
            .get(frame.offset..end)
            .ok_or("reference atlas is short")?;
        compare_frame(&frame.path, expected, picture.as_raw())?;
    }
    println!(
        "PASS pixels: {} frames, one-level bar, coverage and declines unchanged",
        found.len()
    );
    Ok(())
}

/// Two freshly rendered runs must agree before a reference is recorded.
pub fn repeatable(first: &Path, second: &Path) -> Result<(), String> {
    let (a, x) = collect(first)?;
    let (b, y) = collect(second)?;
    if a != b || json(&first.join("coverage.json"))? != json(&second.join("coverage.json"))? {
        return Err("independent renders changed frame coverage, layout or declines".into());
    }
    for frame in a {
        let end = frame.offset + frame.len;
        compare_frame(&frame.path, &x[frame.offset..end], &y[frame.offset..end])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_atlas_rejects_a_two_level_frame_change() {
        let reference = crate::repository().join("corpus/references/generated");
        let manifest = std::fs::read(reference.join("atlas.json")).unwrap();
        let png = std::fs::read(reference.join("atlas.png")).unwrap();
        let index: Index = serde_json::from_slice(&manifest).unwrap();
        let data = image::load_from_memory(&png).unwrap().to_luma8().into_raw();
        let inputs = crate::sha256(
            &std::fs::read(crate::repository().join("corpus/generated.json")).unwrap(),
        );
        let rendered = std::env::temp_dir().join(format!(
            "klipp-recorded-atlas-control-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&rendered).unwrap();
        std::fs::write(
            rendered.join("coverage.json"),
            serde_json::to_vec(&index.coverage).unwrap(),
        )
        .unwrap();
        for frame in &index.frames {
            let path = rendered.join(&frame.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            image::save_buffer(
                &path,
                &data[frame.offset..frame.offset + frame.len],
                frame.width,
                frame.height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
            if let Some(layout) = &frame.layout {
                std::fs::write(
                    path.with_extension("json"),
                    serde_json::to_vec(layout).unwrap(),
                )
                .unwrap();
            }
        }
        compare(&reference, &rendered, &inputs).unwrap();
        let frame = index
            .frames
            .iter()
            .find(|f| f.path.ends_with("-cpu.png"))
            .unwrap();
        let original = &data[frame.offset..frame.offset + frame.len];
        let channel = original
            .iter()
            .enumerate()
            .position(|(i, value)| i % 4 != 3 && *value <= 253)
            .expect("a generated chart has a non-white colour channel");
        for delta in [1, 2] {
            let mut changed = original.to_vec();
            changed[channel] += delta;
            image::save_buffer(
                rendered.join(&frame.path),
                &changed,
                frame.width,
                frame.height,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
            let result = compare(&reference, &rendered, &inputs);
            if delta == 1 {
                result.unwrap();
            } else {
                let error = result.unwrap_err();
                assert!(
                    error.contains(&frame.path) && error.contains("2 levels"),
                    "{error}"
                );
            }
        }
        assert_eq!(
            manifest,
            std::fs::read(reference.join("atlas.json")).unwrap()
        );
        assert_eq!(png, std::fs::read(reference.join("atlas.png")).unwrap());
        std::fs::remove_dir_all(rendered).unwrap();
    }

    #[test]
    fn a_case_update_preserves_other_reference_bytes_and_refuses_their_regression() {
        let root = std::env::temp_dir().join(format!("klipp-atlas-case-{}", uuid::Uuid::new_v4()));
        let rendered = root.join("rendered");
        let reference = root.join("reference");
        for case in ["grade", "edit"] {
            std::fs::create_dir_all(rendered.join(case)).unwrap();
        }
        std::fs::write(
            rendered.join("coverage.json"),
            br#"{"grade/0":{},"edit/0":{}}"#,
        )
        .unwrap();
        let write = |case: &str, level| {
            image::save_buffer(
                rendered.join(case).join("0-cpu.png"),
                &[level, 80, 120, 255],
                1,
                1,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        };
        write("grade", 40);
        write("edit", 40);
        record(&rendered, &reference, "inputs", "initial", None).unwrap();
        write("grade", 90);
        write("edit", 41);
        record(
            &rendered,
            &reference,
            "inputs",
            "grade change",
            Some("grade"),
        )
        .unwrap();
        let index: Index =
            serde_json::from_slice(&std::fs::read(reference.join("atlas.json")).unwrap()).unwrap();
        let bytes = image::open(reference.join("atlas.png"))
            .unwrap()
            .to_luma8()
            .into_raw();
        for frame in index.frames {
            assert_eq!(
                bytes[frame.offset],
                if frame.path.starts_with("grade/") {
                    90
                } else {
                    40
                }
            );
        }
        let before = std::fs::read(reference.join("atlas.json")).unwrap();
        write("edit", 42);
        assert!(
            record(&rendered, &reference, "inputs", "must fail", Some("grade"))
                .unwrap_err()
                .contains("edit/0-cpu.png")
        );
        assert_eq!(before, std::fs::read(reference.join("atlas.json")).unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_planted_frame_change_fails_without_updating_the_atlas() {
        let root = std::env::temp_dir().join(format!("klipp-atlas-fault-{}", uuid::Uuid::new_v4()));
        let rendered = root.join("rendered");
        let reference = root.join("reference");
        std::fs::create_dir_all(rendered.join("grade/0")).unwrap();
        std::fs::write(rendered.join("coverage.json"), b"{}").unwrap();
        let frame = rendered.join("grade/0/24-cpu.png");
        let write = |first: [u8; 4]| {
            let bytes = [first, [160, 30, 100, 255]].concat();
            image::save_buffer(&frame, &bytes, 2, 1, image::ExtendedColorType::Rgba8).unwrap();
        };
        write([40, 80, 120, 255]);
        assert!(
            compare(&reference, &rendered, "inputs").is_err(),
            "missing references must not be recorded silently"
        );
        record(&rendered, &reference, "inputs", "test reference", None).unwrap();
        let before = std::fs::read(reference.join("atlas.png")).unwrap();
        compare(&reference, &rendered, "inputs").unwrap();
        write([41, 79, 121, 255]);
        compare(&reference, &rendered, "inputs").unwrap();
        write([42, 80, 120, 255]);
        let error = compare(&reference, &rendered, "inputs").unwrap_err();
        assert!(
            error.contains("grade/0/24-cpu.png") && error.contains("2 levels"),
            "{error}"
        );
        write([40, 80, 120, 253]);
        assert!(
            compare(&reference, &rendered, "inputs").is_err(),
            "alpha is checked too"
        );
        assert_eq!(
            before,
            std::fs::read(reference.join("atlas.png")).unwrap(),
            "a check must not rewrite its reference"
        );
        std::fs::remove_file(frame).unwrap();
        assert!(
            compare(&reference, &rendered, "inputs").is_err(),
            "a missing route must not pass"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
