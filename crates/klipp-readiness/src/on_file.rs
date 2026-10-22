//! A level-1 scene moved onto real media for level 2 ([`crate::route2`]):
//! each media item it shows reads a lossless file of the still picture
//! the family's level-1 source gives, so the viewer and export read the
//! same frames from disk.

use std::sync::Arc;

use klipp_core::{RationalTime, Resolution};

use crate::route::Scene;

/// `scene` with its media read from a file named `name` whose every frame
/// is `picture` (`scene.width × scene.height`, display-referred linear
/// RGBA), `frames` long at `fps`. A scene with no media (a title over
/// nothing) comes back as it was.
pub fn on_file(scene: &Scene, name: &str, fps: u32, frames: i64, picture: &dyn Fn() -> Vec<f32>) -> Result<Arc<Scene>, String> {
    on_file_frames(scene, name, fps, frames, &|_| picture())
}

/// [`on_file`], frame `i` of the file `frame(i)`: a picture that changes
/// from frame to frame (a frame blend's, #2215).
pub fn on_file_frames(scene: &Scene, name: &str, fps: u32, frames: i64, frame: &dyn Fn(i64) -> Vec<f32>) -> Result<Arc<Scene>, String> {
    let mut moved = scene.clone();
    let ids: Vec<_> = moved.pool.all().map(|m| m.id).collect();
    if ids.is_empty() {
        return Ok(Arc::new(moved));
    }
    let (w, h) = (scene.width, scene.height);
    let path = crate::route2::known_media(name, w, h, fps, frames, frame)?;
    for id in ids {
        let item = moved.pool.get_mut(&id).ok_or("a media item went missing")?;
        item.path = path.clone();
        item.kind = klipp_media::MediaItemKind::File;
        item.duration = RationalTime { value: frames, rate: fps };
        item.fps = klipp_core::Fps::new(fps, 1);
        item.resolution = Resolution { width: w, height: h };
    }
    Ok(Arc::new(moved))
}
