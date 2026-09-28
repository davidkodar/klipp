//! The Narrate window: a scratch voice from text, made on the machine.
//!
//! Type the words, choose a voice, a speed and the pause between
//! sentences, and hear it, add it to the pool, or place it at the
//! playhead on the Voice track as dialogue. The take is made by
//! `klipp_audio::narration`, as the control socket's `narrate` makes
//! one, so the same words in the same voice and delivery are the same
//! file whichever asked for them.
//!
//! Speaking takes a second or two, and a voice not downloaded yet
//! longer, so it happens on a thread and the window says so while it
//! waits.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use klipp_audio::narration::{self, Voice};
use klipp_core::AppCommand;

use crate::state::AppState;

/// What a finished take is for.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Then {
    Preview,
    AddToPool,
    /// Place at this many seconds on the timeline.
    Place(f64),
}

/// A take as asked for: the same request is the same file.
#[derive(Debug, Clone, PartialEq)]
struct Spec {
    text: String,
    voice: String,
    speed: Option<f32>,
    pause_secs: Option<f32>,
}

pub struct NarratePanel {
    text: String,
    voice: String,
    speed: f32,
    pause_on: bool,
    pause_secs: f32,
    voices: Option<Result<Vec<Voice>, String>>,
    working: Option<(Then, Spec, Receiver<Result<PathBuf, String>>)>,
    /// The last take made, so Add or Place after a Preview does not
    /// speak it again.
    last: Option<(Spec, PathBuf)>,
    /// A take on its way into the pool, to place once it is there.
    to_place: Option<(PathBuf, f64)>,
    error: Option<String>,
}

impl Default for NarratePanel {
    fn default() -> Self {
        Self {
            text: String::new(),
            voice: narration::DEFAULT_VOICE.to_string(),
            speed: 1.0,
            pause_on: false,
            pause_secs: 0.4,
            voices: None,
            working: None,
            last: None,
            to_place: None,
            error: None,
        }
    }
}

impl NarratePanel {
    fn spec(&self) -> Spec {
        Spec {
            text: self.text.trim().to_string(),
            voice: self.voice.clone(),
            // As the voice speaks, when left at 1: no argument, and the
            // take keeps the name it would have had without the slider.
            speed: ((self.speed - 1.0).abs() > 0.005).then_some(self.speed),
            pause_secs: self.pause_on.then_some(self.pause_secs),
        }
    }

    fn voice(&self) -> Option<&Voice> {
        self.voices.as_ref()?.as_ref().ok()?.iter().find(|v| v.id == self.voice)
    }

    /// Make the take asked for, or reuse the last one when it is the
    /// same; a reused take's command comes back at once.
    fn start(&mut self, then: Then) -> Option<AppCommand> {
        let spec = self.spec();
        self.error = None;
        if let Some((last, path)) = &self.last {
            if *last == spec && path.is_file() {
                let path = path.clone();
                return self.finish(then, spec, Ok(path));
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let job = spec.clone();
        std::thread::Builder::new()
            .name("klipp-narrate".into())
            .spawn(move || {
                let dir = klipp_audio_search::download_dir().join("narration");
                let got = narration::synthesize(&narration::Request {
                    text: &job.text,
                    voice: Some(&job.voice),
                    speed: job.speed,
                    pause_secs: job.pause_secs,
                    download: true,
                    path: None,
                    dir: &dir,
                });
                let _ = tx.send(got);
            })
            .ok();
        self.working = Some((then, spec, rx));
        None
    }

    /// Where a finished take goes; the commands are sent by `show`.
    fn finish(&mut self, then: Then, spec: Spec, got: Result<PathBuf, String>) -> Option<AppCommand> {
        let path = match got {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e);
                return None;
            }
        };
        self.last = Some((spec, path.clone()));
        match then {
            Then::Preview => {
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        if let Err(e) = klipp_audio_search::preview_play(bytes) {
                            self.error = Some(format!("Preview: {e}"));
                        }
                    }
                    Err(e) => self.error = Some(format!("{}: {e}", path.display())),
                }
                None
            }
            Then::AddToPool => Some(AppCommand::ImportMedia { path }),
            Then::Place(at) => {
                self.to_place = Some((path.clone(), at));
                Some(AppCommand::ImportMedia { path })
            }
        }
    }

    fn poll(&mut self) -> Option<AppCommand> {
        let (then, spec, got) = {
            let (then, spec, rx) = self.working.as_ref()?;
            match rx.try_recv() {
                Ok(got) => (*then, spec.clone(), got),
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => (*then, spec.clone(), Err("the narration thread stopped".to_string())),
            }
        };
        self.working = None;
        // A voice fetched for the take is installed now.
        self.voices = None;
        self.finish(then, spec, got)
    }

    /// Draw the window, and hand back what to send.
    pub fn show(&mut self, ctx: &egui::Context, state: &mut AppState, open: &mut bool) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        cmds.extend(self.poll());

        // A take on its way into the pool is placed once it has arrived,
        // or at once if it was already there.
        if let Some((path, at)) = &self.to_place {
            if let Some(item) = state.media_pool.find_by_path(path) {
                let ops = klipp_document::ops::place_narration(state.sequence(), item.id, *at);
                state.pending_doc_ops.push(crate::state::DocOpsRequest { label: "Narrate".to_string(), ops, independent: false });
                self.to_place = None;
            }
        }

        if self.voices.is_none() {
            self.voices = Some(narration::voices());
        }

        let mut asked = None;
        egui::Window::new("Narrate")
            .open(open)
            .default_size([460.0, 320.0])
            .min_width(340.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("A scratch voice to cut to, made on this machine. Replace it with a recorded one later.").weak());
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .hint_text("What the voice says")
                        .desired_rows(5)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);

                egui::Grid::new("narrate_settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Voice");
                    ui.horizontal(|ui| {
                        match &self.voices {
                            Some(Ok(voices)) => {
                                let shown = voices.iter().find(|v| v.id == self.voice).map(voice_label).unwrap_or_else(|| self.voice.clone());
                                egui::ComboBox::from_id_salt("narrate_voice").selected_text(shown).width(260.0).show_ui(ui, |ui| {
                                    for v in voices {
                                        ui.selectable_value(&mut self.voice, v.id.clone(), voice_label(v));
                                    }
                                });
                            }
                            Some(Err(e)) => {
                                ui.colored_label(ui.visuals().error_fg_color, e);
                            }
                            None => {}
                        }
                        if ui.small_button("Voices…").on_hover_text("Download or remove voices in the AI model manager").clicked() {
                            state.ai_models_dialog.search = "voice".to_string();
                            state.show_ai_models_dialog = true;
                        }
                    });
                    ui.end_row();

                    ui.label("Speed");
                    ui.horizontal(|ui| {
                        ui.add(egui::Slider::new(&mut self.speed, 0.5..=2.0).fixed_decimals(2).suffix("×"))
                            .on_hover_text("1 is as the voice speaks; about 0.85 is an unhurried read");
                        if ui.small_button("Reset").clicked() {
                            self.speed = 1.0;
                        }
                    });
                    ui.end_row();

                    ui.label("Pause");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.pause_on, "between sentences");
                        ui.add_enabled(self.pause_on, egui::Slider::new(&mut self.pause_secs, 0.0..=3.0).fixed_decimals(2).suffix(" s"));
                    });
                    ui.end_row();
                });

                // What the voice may be used for, and a plain warning
                // when it is not for paid work.
                if let Some(v) = self.voice() {
                    if v.commercial_use {
                        ui.label(egui::RichText::new(format!("Licence: {}", v.license)).weak().small());
                    } else {
                        ui.colored_label(ui.visuals().warn_fg_color, format!("Not for commercial use: {}. Choose another voice for paid work.", v.license));
                    }
                }

                ui.add_space(8.0);
                let busy = self.working.is_some();
                let ready = !busy && !self.text.trim().is_empty() && self.voices.as_ref().is_some_and(|v| v.is_ok());
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready, egui::Button::new("Preview")).on_hover_text("Hear it before it goes anywhere").clicked() {
                        asked = self.start(Then::Preview);
                    }
                    if ui.add_enabled(ready, egui::Button::new("Add to Pool")).clicked() {
                        asked = self.start(Then::AddToPool);
                    }
                    if ui.add_enabled(ready, egui::Button::new("Place at Playhead")).on_hover_text("On the Voice track, as dialogue, so a duck comes down under it").clicked() {
                        let at = state.playhead.to_secs_f64();
                        asked = self.start(Then::Place(at));
                    }
                    if klipp_audio_search::preview_active() && ui.small_button("Stop").clicked() {
                        klipp_audio_search::preview_stop();
                    }
                });

                if busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        let downloading = self.voice().is_some_and(|v| !v.installed);
                        ui.label(if downloading { "Downloading the voice, then speaking…" } else { "Speaking…" });
                    });
                } else if let Some(v) = self.voice().filter(|v| !v.installed) {
                    ui.label(egui::RichText::new(format!("{} is downloaded the first time it is used ({}).", v.name, crate::model_download::fmt_bytes(v.size_bytes))).weak());
                }
                if let Some(e) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            });

        cmds.extend(asked);
        // A take already in the pool is not imported again: placed, or
        // said to be there.
        cmds.retain(|c| match c {
            AppCommand::ImportMedia { path } if state.media_pool.find_by_path(path).is_some() => {
                if self.to_place.is_none() {
                    state.toast.show("That take is already in the pool");
                }
                false
            }
            _ => true,
        });
        if self.working.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
        cmds
    }
}

/// A voice as the picker shows it: its name, and its size when it still
/// has to be fetched.
fn voice_label(v: &Voice) -> String {
    if v.installed {
        v.name.clone()
    } else {
        format!("{}  ({} to download)", v.name, crate::model_download::fmt_bytes(v.size_bytes))
    }
}
