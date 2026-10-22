//! Real 3D text (#1512): a text's letters as solid shapes, extruded to
//! a depth with a bevel on their front and back edges, turned in three
//! dimensions under a perspective camera and lit from a direction.
//! Separate from `TextExtrusion`, the older stacked-copy look,
//! which stays as it is; a layer with both draws this one.

use crate::keyframe::Keyframe;
use serde::{Deserialize, Serialize};

/// The cross-section of a bevel: a straight chamfer or a quarter round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BevelProfile {
    #[default]
    Flat,
    Round,
}

/// A bevel on a face's edge: `size` in ems, both across the face and
/// into the depth, in `segments` steps (a round profile's smoothness).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bevel {
    #[serde(default = "bevel_size")]
    pub size: f32,
    #[serde(default = "bevel_segments")]
    pub segments: u8,
    #[serde(default)]
    pub profile: BevelProfile,
}

fn bevel_size() -> f32 {
    0.02
}
fn bevel_segments() -> u8 {
    3
}

impl Default for Bevel {
    fn default() -> Self {
        Self { size: bevel_size(), segments: bevel_segments(), profile: BevelProfile::Flat }
    }
}

/// The camera the text is seen through: a vertical field of view in
/// degrees, the camera as far from the text as keeps its front face at
/// its flat size (a wider view is nearer, the perspective stronger);
/// orthographic sees it without perspective.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Camera3d {
    #[serde(default = "camera_fov")]
    pub fov: f32,
    #[serde(default)]
    pub orthographic: bool,
}

fn camera_fov() -> f32 {
    30.0
}
impl Default for Camera3d {
    fn default() -> Self {
        Self { fov: camera_fov(), orthographic: false }
    }
}

/// The light: from `azimuth` degrees around (0 from the right, 90 from
/// below) and `elevation` degrees towards the viewer, at `intensity`;
/// `ambient` lights every surface alike, `specular` and `shininess` set
/// the highlight.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Light3d {
    #[serde(default = "light_azimuth")]
    pub azimuth: f32,
    #[serde(default = "light_elevation")]
    pub elevation: f32,
    #[serde(default = "one")]
    pub intensity: f32,
    #[serde(default = "light_ambient")]
    pub ambient: f32,
    #[serde(default = "light_specular")]
    pub specular: f32,
    #[serde(default = "light_shininess")]
    pub shininess: f32,
}

fn light_azimuth() -> f32 {
    -135.0
}
fn light_elevation() -> f32 {
    45.0
}
fn one() -> f32 {
    1.0
}
fn light_ambient() -> f32 {
    0.25
}
fn light_specular() -> f32 {
    0.35
}
fn light_shininess() -> f32 {
    32.0
}

impl Default for Light3d {
    fn default() -> Self {
        Self {
            azimuth: light_azimuth(),
            elevation: light_elevation(),
            intensity: one(),
            ambient: light_ambient(),
            specular: light_specular(),
            shininess: light_shininess(),
        }
    }
}

/// A text drawn in 3D (#1512). Lengths are in ems of the text's size;
/// `rotation` is degrees about x, y and z through the text's centre.
/// The face takes the text's own colour unless `face_color` says
/// otherwise, and the bevel the face's unless `bevel_color` does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Text3d {
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default = "depth")]
    pub depth: f32,
    #[serde(default)]
    pub front_bevel: Option<Bevel>,
    #[serde(default)]
    pub back_bevel: Option<Bevel>,
    #[serde(default)]
    pub rotation: [f32; 3],
    #[serde(default)]
    pub camera: Camera3d,
    #[serde(default)]
    pub light: Light3d,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_color: Option<[u8; 4]>,
    #[serde(default = "side_color")]
    pub side_color: [u8; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bevel_color: Option<[u8; 4]>,
    /// The keyed values over the clip's time, by name ([`KEYS`]).
    #[serde(default, skip_serializing_if = "Text3dKeys::is_empty")]
    pub keys: Text3dKeys,
}

fn enabled() -> bool {
    true
}
fn depth() -> f32 {
    0.25
}
fn side_color() -> [u8; 4] {
    [90, 90, 100, 255]
}

impl Default for Text3d {
    fn default() -> Self {
        Self {
            enabled: true,
            depth: depth(),
            front_bevel: Some(Bevel::default()),
            back_bevel: None,
            rotation: [0.0; 3],
            camera: Camera3d::default(),
            light: Light3d::default(),
            face_color: None,
            side_color: side_color(),
            bevel_color: None,
            keys: Text3dKeys::default(),
        }
    }
}

/// The keyable values, each a track over the clip's time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Text3dKeys {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depth: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bevel: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rotate_x: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rotate_y: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rotate_z: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fov: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub light_azimuth: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub light_elevation: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub light_intensity: Vec<Keyframe>,
}

/// The keyable values' names, as `text3d.{name}` param keys.
pub const KEYS: [&str; 9] = ["depth", "bevel", "rotate_x", "rotate_y", "rotate_z", "fov", "light_azimuth", "light_elevation", "light_intensity"];

impl Text3dKeys {
    pub fn is_empty(&self) -> bool {
        KEYS.iter().all(|k| self.get(k).is_none_or(<[Keyframe]>::is_empty))
    }

    /// The track named `name` (one of [`KEYS`]).
    pub fn get(&self, name: &str) -> Option<&[Keyframe]> {
        Some(match name {
            "depth" => &self.depth,
            "bevel" => &self.bevel,
            "rotate_x" => &self.rotate_x,
            "rotate_y" => &self.rotate_y,
            "rotate_z" => &self.rotate_z,
            "fov" => &self.fov,
            "light_azimuth" => &self.light_azimuth,
            "light_elevation" => &self.light_elevation,
            "light_intensity" => &self.light_intensity,
            _ => return None,
        })
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Vec<Keyframe>> {
        Some(match name {
            "depth" => &mut self.depth,
            "bevel" => &mut self.bevel,
            "rotate_x" => &mut self.rotate_x,
            "rotate_y" => &mut self.rotate_y,
            "rotate_z" => &mut self.rotate_z,
            "fov" => &mut self.fov,
            "light_azimuth" => &mut self.light_azimuth,
            "light_elevation" => &mut self.light_elevation,
            "light_intensity" => &mut self.light_intensity,
            _ => return None,
        })
    }
}

impl Text3d {
    /// The text in 3D at `time_secs` of its clip: every keyed value as
    /// it is then, its keys gone. The bevel's key sets the front and
    /// back bevels' size alike.
    pub fn at(&self, time_secs: f64) -> Text3d {
        let mut t = self.clone();
        let k = std::mem::take(&mut t.keys);
        let at = |keys: &[Keyframe], v: f32| crate::keyframe::evaluate(keys, time_secs, v);
        t.depth = at(&k.depth, t.depth).max(0.0);
        if !k.bevel.is_empty() {
            for b in [&mut t.front_bevel, &mut t.back_bevel].into_iter().flatten() {
                b.size = at(&k.bevel, b.size).max(0.0);
            }
        }
        t.rotation = [at(&k.rotate_x, t.rotation[0]), at(&k.rotate_y, t.rotation[1]), at(&k.rotate_z, t.rotation[2])];
        t.camera.fov = at(&k.fov, t.camera.fov).clamp(1.0, 170.0);
        t.light.azimuth = at(&k.light_azimuth, t.light.azimuth);
        t.light.elevation = at(&k.light_elevation, t.light.elevation);
        t.light.intensity = at(&k.light_intensity, t.light.intensity).max(0.0);
        t
    }
}

/// The looks by name, as the Title tab and edit operations name them.
pub const LOOKS: [&str; 3] = ["block", "bevelled", "chrome"];

impl Text3d {
    /// The look `name` laid on these settings: depth, bevels, light and
    /// colours set, the turn, camera and keys kept. `None` for a name
    /// that is not one of [`LOOKS`].
    pub fn with_look(&self, name: &str) -> Option<Text3d> {
        let mut t = self.clone();
        t.enabled = true;
        t.light = Light3d { azimuth: t.light.azimuth, elevation: t.light.elevation, ..Light3d::default() };
        match name {
            // A deep slab, square edged, its walls a shade of its face.
            "block" => {
                t.depth = 0.35;
                (t.front_bevel, t.back_bevel) = (None, None);
                t.side_color = [70, 70, 80, 255];
                t.face_color = None;
                t.bevel_color = None;
            }
            // A rounded bevel catching the light round each letter.
            "bevelled" => {
                t.depth = 0.25;
                t.front_bevel = Some(Bevel { size: 0.03, segments: 4, profile: BevelProfile::Round });
                t.back_bevel = None;
                t.side_color = [90, 90, 100, 255];
                t.face_color = None;
                t.bevel_color = None;
            }
            // Polished metal: a bright face, a hard highlight, dark walls.
            "chrome" => {
                t.depth = 0.2;
                t.front_bevel = Some(Bevel { size: 0.04, segments: 5, profile: BevelProfile::Round });
                t.back_bevel = Some(Bevel { size: 0.02, segments: 2, profile: BevelProfile::Round });
                t.face_color = Some([205, 210, 220, 255]);
                t.bevel_color = Some([240, 242, 246, 255]);
                t.side_color = [45, 48, 56, 255];
                t.light.specular = 0.9;
                t.light.shininess = 64.0;
                t.light.ambient = 0.2;
            }
            _ => return None,
        }
        Some(t)
    }

    /// Which of [`LOOKS`] these settings are, if one: what the Title
    /// tab marks as chosen.
    pub fn look(&self) -> Option<&'static str> {
        LOOKS.into_iter().find(|n| self.with_look(n).as_ref() == Some(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: crate::keyframe::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    /// Keyed values are as they are at a time; the keys go once read.
    #[test]
    fn a_keyed_3d_text_is_its_values_at_a_time() {
        let mut t = Text3d { back_bevel: Some(Bevel::default()), ..Default::default() };
        t.keys.rotate_y = vec![key(0.0, -60.0), key(2.0, 60.0)];
        t.keys.bevel = vec![key(0.0, 0.0), key(2.0, 0.04)];
        t.keys.light_azimuth = vec![key(0.0, 0.0), key(2.0, 90.0)];
        let at = t.at(1.0);
        assert_eq!((at.rotation, at.light.azimuth), ([0.0, 0.0, 0.0], 45.0));
        assert_eq!(at.front_bevel.map(|b| b.size), Some(0.02));
        assert_eq!(at.back_bevel.map(|b| b.size), Some(0.02), "one key, both bevels");
        assert!(at.keys.is_empty());
        assert_eq!(t.at(5.0).rotation[1], 60.0);
        for name in KEYS {
            assert!(t.keys.get(name).is_some() && t.clone().keys.get_mut(name).is_some(), "{name}");
        }
        assert!(t.keys.get("roll").is_none());
    }

    /// A look sets depth, bevels, light and colours and keeps the turn,
    /// the camera and the keys; it reads back as itself.
    #[test]
    fn a_look_keeps_the_turn_and_the_keys() {
        let mut t = Text3d { rotation: [0.0, 25.0, 0.0], ..Default::default() };
        t.keys.rotate_y = vec![key(0.0, 0.0)];
        t.camera.fov = 50.0;
        for name in LOOKS {
            let l = t.with_look(name).unwrap();
            assert_eq!((l.rotation, l.camera.fov, l.keys.rotate_y.len(), l.enabled), ([0.0, 25.0, 0.0], 50.0, 1, true), "{name}");
            assert_eq!(l.look(), Some(name));
        }
        assert_eq!(t.with_look("block").unwrap().front_bevel, None);
        assert!(t.with_look("chrome").unwrap().light.specular > 0.5);
        assert!(t.with_look("marble").is_none());
        assert_eq!(Text3d { depth: 0.31, ..t.with_look("block").unwrap() }.look(), None, "changed, it is no look");
    }

    /// A 3D text saves its own fields, reads back the same, and reads
    /// from almost nothing with its defaults.
    #[test]
    fn a_3d_text_saves_and_reads_back() {
        let mut t = Text3d { face_color: Some([200, 30, 30, 255]), back_bevel: Some(Bevel { size: 0.03, segments: 5, profile: BevelProfile::Round }), ..Default::default() };
        t.keys.fov = vec![key(1.0, 50.0)];
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Text3d>(&json).unwrap(), t);
        assert!(!serde_json::to_string(&Text3d::default()).unwrap().contains("keys"));
        let bare: Text3d = serde_json::from_str("{}").unwrap();
        assert_eq!((bare.depth, bare.camera, bare.light), (0.25, Camera3d::default(), Light3d::default()));
    }
}
