//! The Sound Over Time window: a whole file's sound as frequency
//! against time, with what is wrong with it named.
//!
//! Klipp's scrolling analyser shows what is playing. That is the right
//! instrument for setting a level and the wrong one for the question
//! people actually ask of a long recording: *where* did the hum start,
//! *when* did the radio mic begin to hiss, *which* two minutes have the
//! air conditioning in them. Those are answered by seeing the whole
//! file at once.
//!
//! The reading is the same one `klipp_audio_spectrum` gives an
//! assistant, drawn by the same code the live scope uses, so the
//! picture here, the scrolling one, and the numbers an agent reads are
//! three views of one measurement rather than three measurements.
//!
//! Decoding a long file takes seconds, so it happens on a thread and
//! the window says so while it waits.

use std::sync::mpsc::{Receiver, TryRecvError};

use klipp_analysis::spectrogram::Spectrogram;

use crate::state::AppState;
use crate::theme::KlippTheme;

/// How many columns the file is read in. One per pixel of a wide
/// window, so a fault a second long is its own column on anything up
/// to about ten minutes, and the cost does not grow with the file.
const COLUMNS: usize = 900;
/// Log-frequency rows. More than the eye resolves in a 200 px tall
/// strip, and enough that 50 Hz and 60 Hz land in different rows.
const BINS: usize = 160;

enum Reading {
    /// Nothing asked for yet.
    Idle,
    /// A thread is decoding; the file it is reading, and the answer.
    Working(String, Receiver<Result<Spectrogram, String>>),
    Done(String, Box<Spectrogram>, Vec<(String, String, String)>),
    Failed(String, String),
}

pub struct SoundOverTimePanel {
    reading: Reading,
    /// The media the reading belongs to, so moving the playhead to a
    /// different file offers to read that one instead.
    media: Option<klipp_core::MediaId>,
    texture: Option<egui::TextureHandle>,
    /// The width and floor the picture was drawn at, so a resize or a
    /// turn of the floor redraws it.
    drawn_at: (usize, f32),
    db_floor: f32,
}

impl Default for SoundOverTimePanel {
    fn default() -> Self {
        Self { reading: Reading::Idle, media: None, texture: None, drawn_at: (0, f32::NAN), db_floor: -72.0 }
    }
}

/// The file whose sound is under the playhead: the first audible clip
/// there, picture or sound.
fn media_at_playhead(state: &AppState) -> Option<(klipp_core::MediaId, String, std::path::PathBuf)> {
    let seq = state.sequence();
    let tc = state.playhead;
    let clip = seq
        .tracks
        .iter()
        .filter(|t| !t.muted)
        .find_map(|t| t.clip_at_timecode(tc).map(|(_, c)| c))?;
    let id = clip.media_id()?;
    let item = state.media_pool.get(&id)?;
    item.has_audio.then(|| (id, item.filename.clone(), item.path.clone()))
}

impl SoundOverTimePanel {
    fn start(&mut self, id: klipp_core::MediaId, name: String, path: std::path::PathBuf) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let got = klipp_analysis::spectrogram::of_file(std::path::Path::new("ffmpeg"), &path, 0.0, None, COLUMNS, BINS);
            let _ = tx.send(got);
        });
        self.media = Some(id);
        self.texture = None;
        self.reading = Reading::Working(name, rx);
    }

    /// Collect the thread's answer if it has one.
    fn poll(&mut self) {
        let Reading::Working(name, rx) = &self.reading else { return };
        match rx.try_recv() {
            Ok(Ok(spec)) => {
                let found = klipp_analysis::spectrogram::findings(&spec);
                self.reading = Reading::Done(name.clone(), Box::new(spec), found);
            }
            Ok(Err(why)) => self.reading = Reading::Failed(name.clone(), why),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.reading = Reading::Failed(name.clone(), "the reading stopped before it finished".into())
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, state: &AppState, theme: &KlippTheme, open: &mut bool) {
        self.poll();
        let under = media_at_playhead(state);
        egui::Window::new("Sound Over Time")
            .open(open)
            .default_size([620.0, 420.0])
            .min_width(320.0)
            .min_height(220.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    match &under {
                        Some((id, name, path)) => {
                            let same = self.media == Some(*id);
                            let label = if same { "Read it again".to_string() } else { format!("Read {name}") };
                            if ui.add(egui::Button::new(label)).on_hover_text("Decode the whole file and read its sound. A long file takes a few seconds; it happens on a thread and the window stays live.").clicked() {
                                self.start(*id, name.clone(), path.clone());
                            }
                        }
                        None => {
                            ui.label(egui::RichText::new("No sound under the playhead.").color(theme.text_secondary));
                        }
                    }
                    ui.add(egui::Slider::new(&mut self.db_floor, -96.0..=-24.0).text("floor dB"))
                        .on_hover_text("How quiet a sound still shows. Lower to see the noise floor, raise to see only what is loud.");
                });
                match &self.reading {
                    Reading::Idle => {
                        ui.label(egui::RichText::new("The scrolling analyser shows what is playing. This reads the whole file at once, which is the only way to see where a hum starts or when a hiss creeps in.").small().color(theme.text_secondary));
                    }
                    Reading::Working(name, _) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(egui::RichText::new(format!("Reading {name}…")).color(theme.text_secondary));
                        });
                        ctx.request_repaint_after(std::time::Duration::from_millis(120));
                    }
                    Reading::Failed(name, why) => {
                        ui.label(egui::RichText::new(format!("{name}: {why}")).color(theme.marker_error));
                    }
                    Reading::Done(..) => {
                        // Lifted out of the borrow so the picture can be
                        // drawn into `self` while the reading is read.
                        let Reading::Done(name, spec, found) = &self.reading else { unreachable!() };
                        let (name, spec, found) = (name.clone(), spec.clone(), found.clone());
                        self.picture(ui, ctx, theme, &spec, state);
                        ui.label(
                            egui::RichText::new(format!("{name} — {:.1} s in {:.2} s columns, up to {:.0} kHz", spec.duration_secs, spec.hop_secs, spec.rate as f32 / 2000.0))
                                .small()
                                .color(theme.text_dim),
                        );
                        if found.is_empty() {
                            ui.label(egui::RichText::new("Nothing to report: no band comes close enough to the voice to be heard under it.").color(theme.marker_success));
                        }
                        for (sev, what, fix) in &found {
                            let colour = if sev == "error" { theme.marker_error } else { theme.marker_warning };
                            ui.label(egui::RichText::new(what).color(colour).size(crate::theme::type_scale::BODY));
                            ui.label(egui::RichText::new(fix).small().color(theme.text_secondary));
                        }
                    }
                }
            });
    }

    fn picture(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, theme: &KlippTheme, spec: &Spectrogram, state: &AppState) {
        let width = ui.available_width().max(120.0);
        let height = 180.0f32;
        let want = width.round() as usize;
        if self.texture.is_none() || self.drawn_at != (want, self.db_floor) {
            let image = crate::widgets::scopes::render_audio_spectrogram(&spec.columns, want, height as usize, self.db_floor);
            self.texture = Some(ctx.load_texture("sound-over-time", image, egui::TextureOptions::LINEAR));
            self.drawn_at = (want, self.db_floor);
        }
        let Some(tex) = &self.texture else { return };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        ui.painter().image(tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        // Where the playhead is in this file, so a band seen here can
        // be found by ear.
        if spec.duration_secs > 0.0 {
            let at = state.playhead.to_secs_f64();
            if at >= 0.0 && at <= spec.duration_secs {
                let x = rect.left() + (at / spec.duration_secs) as f32 * rect.width();
                ui.painter().line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0_f32, theme.playhead));
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, secs: f32, amp: f32) -> Vec<f32> {
        let rate = klipp_analysis::spectrogram::RATE;
        let n = (secs * rate as f32) as usize;
        (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin()).collect()
    }

    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        ctx
    }

    #[test]
    fn the_window_opens_with_nothing_read_and_does_not_panic() {
        let state = AppState::default();
        let mut panel = SoundOverTimePanel::default();
        let theme = KlippTheme::default();
        let ctx = ctx();
        let mut open = true;
        let _ = ctx.run(Default::default(), |ctx| {
            panel.show(ctx, &state, &theme, &mut open);
        });
        assert!(matches!(panel.reading, Reading::Idle));
    }

    #[test]
    fn a_finished_reading_draws_its_picture_and_names_its_faults() {
        // The reading a thread would have produced, put in by hand so
        // the test does not need ffmpeg or a file.
        let voice = sine(1000.0, 4.0, 0.3);
        let hum = sine(50.0, 4.0, 0.06);
        let mixed: Vec<f32> = voice.iter().zip(&hum).map(|(a, b)| a + b).collect();
        let spec = klipp_analysis::spectrogram::of_samples(&mixed, 120, BINS);
        let found = klipp_analysis::spectrogram::findings(&spec);
        assert!(found.iter().any(|(_, w, _)| w.starts_with("hum")), "{found:?}");

        let state = AppState::default();
        let mut panel = SoundOverTimePanel { reading: Reading::Done("take-1.wav".into(), Box::new(spec), found), ..Default::default() };
        let theme = KlippTheme::default();
        let ctx = ctx();
        let mut open = true;
        let _ = ctx.run(Default::default(), |ctx| {
            panel.show(ctx, &state, &theme, &mut open);
        });
        assert!(panel.texture.is_some(), "the picture was drawn");
        assert!(panel.drawn_at.0 > 0);
        // Turning the floor redraws it: the picture is a function of
        // the floor, and an unchanged texture would be a lie.
        let was = panel.drawn_at;
        panel.db_floor = -48.0;
        let _ = ctx.run(Default::default(), |ctx| {
            panel.show(ctx, &state, &theme, &mut open);
        });
        assert_ne!(panel.drawn_at, was, "a new floor is a new picture");
    }

    #[test]
    fn a_failed_reading_says_why_instead_of_drawing_nothing() {
        let state = AppState::default();
        let mut panel = SoundOverTimePanel { reading: Reading::Failed("take-1.wav".into(), "ffmpeg could not read the sound".into()), ..Default::default() };
        let theme = KlippTheme::default();
        let ctx = ctx();
        let mut open = true;
        let _ = ctx.run(Default::default(), |ctx| {
            panel.show(ctx, &state, &theme, &mut open);
        });
        assert!(panel.texture.is_none());
    }

    #[test]
    fn a_reading_whose_thread_died_does_not_wait_for_ever() {
        // The thread panicking would otherwise leave the window
        // spinning with no way back.
        let (tx, rx) = std::sync::mpsc::channel::<Result<Spectrogram, String>>();
        drop(tx);
        let mut panel = SoundOverTimePanel { reading: Reading::Working("take-1.wav".into(), rx), ..Default::default() };
        panel.poll();
        assert!(matches!(panel.reading, Reading::Failed(..)));
    }
}
