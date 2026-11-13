//! A resting render made ahead on the warm-up thread is kept by the
//! renderer that will place it only if it is still the title's picture:
//! a catalogue face that arrived, or an image file that changed, after
//! the render was begun makes it another picture, and the title is drawn
//! afresh. A test binary of its own because a fetch is counted for the
//! whole process.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use klipp_timeline::{ImageLayer, TextBox, TitleData, TitleObjectContent};
use klipp_title::{catalogue, TitleRenderer};

const W: u32 = 320;
const H: u32 = 180;

/// The two tests one at a time: the font one changes what every
/// renderer in the process has seen.
static ONE: Mutex<()> = Mutex::new(());

fn text() -> TitleData {
    TitleData::single_text(TextBox { text: "Arrives later".into(), font_size: 80.0, ..Default::default() })
}

/// Draws `td` at 1 s; the resting renders it made doing so, and the picture.
fn draw(r: &mut TitleRenderer, td: &TitleData) -> (u64, Vec<u8>) {
    let before = r.resting_renders();
    let bytes = r.render_with_window(td, W, H, None, 1.0, Some(5.0));
    (r.resting_renders() - before, bytes)
}

#[test]
fn a_render_made_before_a_face_arrived_is_not_kept() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let td = text();
    let mut player = TitleRenderer::new();
    let made = TitleRenderer::new().render_resting(&td, W, H);
    assert!(player.adopt(&td, W, H, made), "a current render is kept");
    assert_eq!(draw(&mut player, &td).0, 0, "kept, the frame draws nothing more");

    let mut player = TitleRenderer::new();
    let made = TitleRenderer::new().render_resting(&td, W, H);
    // A face arrives between the warm-up's raster and the adopt.
    let before = catalogue::fetches();
    catalogue::note_fonts_added();
    assert_eq!(catalogue::fetches(), before + 1);
    assert!(!player.adopt(&td, W, H, made), "a render begun before a face arrived is kept");
    let (rastered, shown) = draw(&mut player, &td);
    assert_eq!(rastered, 1, "the title is drawn afresh with the faces there now");
    assert_eq!(shown, draw(&mut TitleRenderer::new(), &td).1, "and shows what a renderer drawing it now does");
}

#[test]
fn a_render_of_an_image_since_changed_is_not_kept() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("klipp-stale-warm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("logo.png");
    let paint = |rgb: [u8; 3], secs: u64| {
        image::RgbaImage::from_pixel(64, 48, image::Rgba([rgb[0], rgb[1], rgb[2], 255])).save(&path).unwrap();
        // Stamped apart, as a file saved again later is.
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(secs)).unwrap();
    };
    let mut td = text();
    td.objects[0].content = TitleObjectContent::Image(ImageLayer { path: path.to_string_lossy().into_owned(), fit: Default::default() });

    paint([220, 30, 30], 0);
    let mut player = TitleRenderer::new();
    let made = TitleRenderer::new().render_resting(&td, W, H);
    assert!(player.adopt(&td, W, H, made), "a current render is kept");

    let mut player = TitleRenderer::new();
    let made = TitleRenderer::new().render_resting(&td, W, H);
    // The image is saved again between the warm-up's raster and the adopt.
    paint([30, 30, 220], 60);
    assert!(!player.adopt(&td, W, H, made), "a render of the image as it was is kept");
    let (rastered, shown) = draw(&mut player, &td);
    assert_eq!(rastered, 1, "the title is drawn afresh from the image as it is now");
    assert_eq!(shown, draw(&mut TitleRenderer::new(), &td).1, "and shows the image as it is now");
    let _ = std::fs::remove_dir_all(&dir);
}
