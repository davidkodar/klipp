//! The camera's own record of a clip: the NonRealTimeMeta XML that Sony
//! (XAVC, as `C0001M01.XML` beside `C0001.MP4`) and Canon (XF, as
//! `A001C001.XML` beside the clip) write when they record. It carries
//! what the container does not: the lens and exposure, the gamma and
//! gamut the picture was shot in, and the start timecode.

use klipp_core::ShotMetadata;
use std::path::{Path, PathBuf};

/// What a sidecar says about its clip.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Sidecar {
    pub shot: ShotMetadata,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
}

/// The sidecar beside `media`, if the camera left one: Sony's
/// `<stem>M01.XML`, or Canon's `<stem>.XML`, in either case.
pub fn sidecar_path_for(media: &Path) -> Option<PathBuf> {
    let stem = media.file_stem()?.to_str()?;
    let dir = media.parent()?;
    for name in [format!("{stem}M01.XML"), format!("{stem}M01.xml"), format!("{stem}.XML"), format!("{stem}.xml")] {
        let p = dir.join(name);
        if p != media && p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Read and parse the sidecar beside `media`, if there is one.
pub fn read_sidecar(media: &Path) -> Option<Sidecar> {
    let path = sidecar_path_for(media)?;
    let xml = std::fs::read_to_string(&path).ok()?;
    let mut sidecar = parse_sidecar(&xml)?;
    sidecar.shot.tags.push(("sidecar:file".into(), path.file_name()?.to_string_lossy().into_owned()));
    Some(sidecar)
}

/// Parse a NonRealTimeMeta document. `None` when it is not one.
pub fn parse_sidecar(xml: &str) -> Option<Sidecar> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let root = doc.root_element();
    if root.tag_name().name() != "NonRealTimeMeta" {
        return None;
    }
    let mut out = Sidecar::default();
    let shot = &mut out.shot;
    let mut tc_fps: Option<u32> = None;
    let mut drop_frame = false;
    for node in root.descendants().filter(|n| n.is_element()) {
        match node.tag_name().name() {
            "Device" => {
                out.camera_make = attr(node, "manufacturer");
                out.camera_model = attr(node, "modelName");
                for a in node.attributes() {
                    shot.tags.push((format!("sidecar:Device/{}", a.name()), a.value().to_string()));
                }
            }
            "Item" => {
                let (Some(name), Some(value)) = (attr(node, "name"), attr(node, "value")) else { continue };
                let group = node.parent_element().and_then(|g| attr(g, "name")).unwrap_or_default();
                take_item(shot, &name, &value);
                shot.tags.push((format!("sidecar:{group}/{name}"), value));
            }
            "LtcChangeTable" => {
                tc_fps = attr(node, "tcFps").and_then(|v| v.parse().ok());
                drop_frame = attr(node, "halfStep").is_some_and(|v| v == "true");
            }
            "LtcChange" => {
                if attr(node, "frameCount").as_deref() == Some("0") && shot.start_timecode.is_none() {
                    shot.start_timecode = attr(node, "value").and_then(|v| ltc_value_to_timecode(&v, drop_frame));
                }
            }
            "CreationDate" | "LastUpdate" | "Duration" | "VideoFrame" | "VideoLayout" | "AudioFormat" | "TargetMaterial" => {
                for a in node.attributes() {
                    shot.tags.push((format!("sidecar:{}/{}", node.tag_name().name(), a.name()), a.value().to_string()));
                }
            }
            "Lens" | "CodecInfo" | "Creator" | "MediaProfile" => {
                for a in node.attributes() {
                    shot.tags.push((format!("sidecar:{}/{}", node.tag_name().name(), a.name()), a.value().to_string()));
                }
            }
            _ => {}
        }
    }
    if let Some(fps) = tc_fps {
        shot.tags.push(("sidecar:LtcChangeTable/tcFps".into(), fps.to_string()));
    }
    if shot.lens.is_some() || shot.iso.is_some() || shot.shutter.is_some() || shot.aperture.is_some() || shot.focal_length_mm.is_some() {
        shot.exposure_source = Some(match out.camera_make.as_deref() {
            Some(make) => format!("{make} XML"),
            None => "camera XML".to_string(),
        });
    }
    Some(out)
}

fn attr(node: roxmltree::Node, name: &str) -> Option<String> {
    node.attribute(name).map(str::to_string)
}

/// Fill the field an item names, if it is one of the ones we show. The
/// names differ a little between cameras and firmware, so each field
/// answers to several.
fn take_item(shot: &mut ShotMetadata, name: &str, value: &str) {
    let key = name.to_ascii_lowercase();
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    match key.as_str() {
        "isosensitivity" | "iso" | "exposureindex" => {
            if shot.iso.is_none() {
                shot.iso = digits(value).parse().ok();
            }
        }
        "shutterspeed_time" | "exposuretime" | "shutterspeed" | "shutterspeed_angle" if shot.shutter.is_none() => {
            shot.shutter = Some(if key == "shutterspeed_angle" { format!("{value}\u{00B0}") } else { value.to_string() });
        }
        "fnumber" | "irisfnumber" | "lensiris" | "iris" | "aperture" if shot.aperture.is_none() => {
            shot.aperture = value.trim_start_matches(['F', 'f', '/']).parse().ok();
        }
        "focallength" | "lenszoomactualfocallength" | "lenszoom35mmstillcameraequivalentfocallength" if shot.focal_length_mm.is_none() => {
            shot.focal_length_mm = value.trim_end_matches("mm").trim().parse().ok();
        }
        "colortemperature" | "whitebalance" | "whitebalancecolortemperature" if shot.white_balance.is_none() => {
            shot.white_balance = Some(match digits(value).parse::<u32>() {
                Ok(k) if k > 0 => format!("{k} K"),
                _ => value.to_string(),
            });
        }
        "lensattributes" | "lensmodel" | "lensname" | "lens" if shot.lens.is_none() => {
            shot.lens = Some(value.to_string());
        }
        "capturegammaequation" if shot.gamma.is_none() => shot.gamma = Some(value.to_string()),
        "capturecolorprimaries" if shot.colour_gamut.is_none() => shot.colour_gamut = Some(value.to_string()),
        _ => {}
    }
}

fn digits(v: &str) -> String {
    v.chars().filter(char::is_ascii_digit).collect()
}

/// Sony packs a timecode as eight hex digits, "FFSSMMHH", each pair BCD:
/// "00301412" is 12:14:30:00.
fn ltc_value_to_timecode(v: &str, drop_frame: bool) -> Option<String> {
    let v = v.trim();
    if v.len() != 8 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let pair = |i: usize| v[i..i + 2].parse::<u32>().ok();
    let (ff, ss, mm, hh) = (pair(0)?, pair(2)?, pair(4)?, pair(6)?);
    let sep = if drop_frame { ';' } else { ':' };
    Some(format!("{hh:02}:{mm:02}:{ss:02}{sep}{ff:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SONY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<NonRealTimeMeta xmlns="urn:schemas-professionalDisc:nonRealTimeMeta:ver.2.20" lastUpdate="2024-03-15T10:30:00+01:00">
  <TargetMaterial umidRef="060A2B340101010501010D4313000000"/>
  <Duration value="1250"/>
  <LtcChangeTable tcFps="25" halfStep="false">
    <LtcChange frameCount="0" value="00301412" status="increment"/>
    <LtcChange frameCount="1249" value="49311412" status="end"/>
  </LtcChangeTable>
  <CreationDate value="2024-03-15T10:30:00+01:00"/>
  <VideoFormat>
    <VideoFrame videoCodec="AVC_3840_2160_HP@L51" captureFps="25p" formatFps="25p"/>
    <VideoLayout pixel="3840" numOfVerticalLine="2160" aspectRatio="16:9"/>
  </VideoFormat>
  <Device manufacturer="Sony" modelName="ILCE-7M4" serialNo="1234567"/>
  <AcquisitionRecord>
    <Group name="CameraUnitMetadataSet">
      <Item name="CaptureGammaEquation" value="s-log3-cine"/>
      <Item name="CaptureColorPrimaries" value="s-gamut3-cine"/>
      <Item name="CodingEquations" value="rec2020"/>
      <Item name="ISOSensitivity" value="800"/>
      <Item name="ShutterSpeed_Time" value="1/50"/>
      <Item name="FNumber" value="F2.8"/>
      <Item name="FocalLength" value="24"/>
      <Item name="ColorTemperature" value="5600"/>
      <Item name="LensAttributes" value="FE 24-70mm F2.8 GM II"/>
    </Group>
  </AcquisitionRecord>
</NonRealTimeMeta>"#;

    #[test]
    fn a_sony_sidecar_gives_the_exposure_gamma_gamut_and_timecode() {
        let s = parse_sidecar(SONY).expect("a NonRealTimeMeta document");
        assert_eq!(s.camera_make.as_deref(), Some("Sony"));
        assert_eq!(s.camera_model.as_deref(), Some("ILCE-7M4"));
        let shot = &s.shot;
        assert_eq!(shot.start_timecode.as_deref(), Some("12:14:30:00"));
        assert_eq!(shot.iso, Some(800));
        assert_eq!(shot.shutter.as_deref(), Some("1/50"));
        assert_eq!(shot.aperture, Some(2.8));
        assert_eq!(shot.focal_length_mm, Some(24.0));
        assert_eq!(shot.white_balance.as_deref(), Some("5600 K"));
        assert_eq!(shot.lens.as_deref(), Some("FE 24-70mm F2.8 GM II"));
        assert_eq!(shot.gamma.as_deref(), Some("s-log3-cine"));
        assert_eq!(shot.colour_gamut.as_deref(), Some("s-gamut3-cine"));
        assert_eq!(shot.exposure_source.as_deref(), Some("Sony XML"));
        assert_eq!(shot.exposure_summary().as_deref(), Some("ISO 800 \u{00B7} 1/50 \u{00B7} f/2.8 \u{00B7} 24 mm"));
        // Every item is kept as a tag, by group, and the rest of the record too.
        assert!(shot.tags.iter().any(|(k, v)| k == "sidecar:CameraUnitMetadataSet/CodingEquations" && v == "rec2020"));
        assert!(shot.tags.iter().any(|(k, v)| k == "sidecar:Device/serialNo" && v == "1234567"));
        assert!(shot.tags.iter().any(|(k, v)| k == "sidecar:VideoFrame/captureFps" && v == "25p"));
        assert!(shot.tags.iter().any(|(k, v)| k == "sidecar:LtcChangeTable/tcFps" && v == "25"));
    }

    #[test]
    fn drop_frame_timecode_uses_a_semicolon_and_bad_values_are_refused() {
        assert_eq!(ltc_value_to_timecode("15594523", true).as_deref(), Some("23:45:59;15"));
        assert_eq!(ltc_value_to_timecode("0030141", false), None);
        assert_eq!(ltc_value_to_timecode("zz301412", false), None);
    }

    #[test]
    fn other_xml_is_not_a_sidecar() {
        assert_eq!(parse_sidecar("<fcpxml version=\"1.9\"/>"), None);
        assert_eq!(parse_sidecar("not xml"), None);
    }

    #[test]
    fn the_sidecar_is_found_beside_the_clip_in_either_case() {
        let dir = std::env::temp_dir().join(format!("klipp-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("C0001.MP4");
        std::fs::write(&clip, b"").unwrap();
        assert_eq!(sidecar_path_for(&clip), None);
        std::fs::write(dir.join("C0001M01.XML"), SONY).unwrap();
        assert_eq!(sidecar_path_for(&clip), Some(dir.join("C0001M01.XML")));
        let read = read_sidecar(&clip).expect("read");
        assert_eq!(read.shot.iso, Some(800));
        assert!(read.shot.tags.iter().any(|(k, v)| k == "sidecar:file" && v == "C0001M01.XML"));
        let canon = dir.join("A001C001_240315_XYZ.MXF");
        std::fs::write(&canon, b"").unwrap();
        std::fs::write(dir.join("A001C001_240315_XYZ.XML"), SONY.replace("Sony", "Canon")).unwrap();
        assert_eq!(read_sidecar(&canon).unwrap().camera_make.as_deref(), Some("Canon"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
