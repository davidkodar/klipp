//! The Narrate window: a voice-over from text, made on the machine.
//!
//! Type the words, choose a voice, and hear it, add it to the pool, or
//! place it at the playhead on the Voice track as dialogue. The take is
//! made by `klipp_audio::narration`, as the control socket's `narrate`
//! makes one, so the same words in the same voice and delivery are the
//! same file whichever asked for them.
//!
//! One choice, plainly put: the voices are listed by name ("Heart, female,
//! US English"), each Kokoro voice can be heard from a bundled sample
//! before anything is downloaded, the ten of them come as one download
//! with a progress bar, and a program the voice needs and the machine
//! lacks is named, with the command that installs it, before any button
//! is pressed. Piper's lighter voices are under "More voices".

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use klipp_audio::narration::{self, Engine, Voice};
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

/// A download under way: bytes so far of `total`, a cancel, the answer.
struct Fetch {
    total: u64,
    done: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<(), String>>,
}

pub struct NarratePanel {
    text: String,
    voice: String,
    speed: f32,
    pause_on: bool,
    pause_secs: f32,
    voices: Option<Result<Vec<Voice>, String>>,
    working: Option<(Then, Spec, Receiver<Result<PathBuf, String>>)>,
    fetch: Option<Fetch>,
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
            voice: narration::default_voice().to_string(),
            speed: 1.0,
            pause_on: false,
            pause_secs: 0.4,
            voices: None,
            working: None,
            fetch: None,
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
                    download: false,
                    path: None,
                    dir: &dir,
                });
                let _ = tx.send(got);
            })
            .ok();
        self.working = Some((then, spec, rx));
        None
    }

    /// Fetch what the chosen voice still needs, counting as it goes.
    fn start_download(&mut self, voice: &Voice) {
        let (tx, rx) = std::sync::mpsc::channel();
        let done = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (ids, d, c) = (voice.missing.clone(), done.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("klipp-narrate-download".into())
            .spawn(move || {
                let _ = tx.send(narration::download(&ids, &d, &c));
            })
            .ok();
        self.error = None;
        self.fetch = Some(Fetch { total: voice.size_bytes.max(1), done, cancel, rx });
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
        if let Some(fetch) = &self.fetch {
            match fetch.rx.try_recv() {
                Ok(got) => {
                    if let Err(e) = got {
                        if e != "cancelled" {
                            self.error = Some(format!("Download: {e}"));
                        }
                    }
                    self.fetch = None;
                    self.voices = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.fetch = None;
                    self.voices = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        let (then, spec, got) = {
            let (then, spec, rx) = self.working.as_ref()?;
            match rx.try_recv() {
                Ok(got) => (*then, spec.clone(), got),
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => (*then, spec.clone(), Err("the narration thread stopped".to_string())),
            }
        };
        self.working = None;
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
            .default_size([480.0, 340.0])
            .min_width(360.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("A voice-over made on this machine, to cut to now and replace with a recorded one later if you like.").weak());
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .hint_text("What the voice says")
                        .desired_rows(5)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);

                let voices: Vec<Voice> = match &self.voices {
                    Some(Ok(v)) => v.clone(),
                    Some(Err(e)) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                        Vec::new()
                    }
                    None => Vec::new(),
                };
                let chosen = voices.iter().find(|v| v.id == self.voice).cloned();

                egui::Grid::new("narrate_settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Voice");
                    ui.horizontal(|ui| {
                        let shown = chosen.as_ref().map(picker_label).unwrap_or_else(|| self.voice.clone());
                        // Research-only Lessac is shown only while it is the one chosen.
                        let current = self.voice.clone();
                        egui::ComboBox::from_id_salt("narrate_voice").selected_text(shown).width(290.0).show_ui(ui, |ui| {
                            for (heading, pick) in [
                                ("US English", &(|v: &Voice| v.engine == Engine::Kokoro && v.about.ends_with("US English")) as &dyn Fn(&Voice) -> bool),
                                ("UK English", &|v: &Voice| v.engine == Engine::Kokoro && v.about.ends_with("UK English")),
                                ("More voices (Piper: lighter, more robotic)", &|v: &Voice| v.engine == Engine::Piper && (v.commercial_use || v.id == current)),
                            ] {
                                let group: Vec<&Voice> = voices.iter().filter(|v| pick(v)).collect();
                                if group.is_empty() {
                                    continue;
                                }
                                ui.label(egui::RichText::new(heading).small().weak());
                                for v in group {
                                    ui.selectable_value(&mut self.voice, v.id.clone(), picker_label(v));
                                }
                            }
                        });
                        let sample = chosen.as_ref().and_then(|v| v.sample);
                        if ui.add_enabled(sample.is_some(), egui::Button::new("▶ Sample")).on_hover_text("Hear this voice; nothing is downloaded").on_disabled_hover_text("Piper voices have no sample; Preview says your words once it is downloaded").clicked() {
                            if let Some(bytes) = sample {
                                if let Err(e) = klipp_audio_search::preview_play(bytes.to_vec()) {
                                    self.error = Some(format!("Sample: {e}"));
                                }
                            }
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

                // The one thing between this voice and a take, if anything:
                // a program to install, or a download, said once and plainly.
                ui.add_space(6.0);
                let mut ready = false;
                if let Some(v) = &chosen {
                    if let Some(why) = narration::engine_missing(v.engine) {
                        let install = match v.engine {
                            Engine::Kokoro => "sudo pacman -S espeak-ng",
                            Engine::Piper => "sudo pacman -S piper-tts",
                        };
                        ui.colored_label(ui.visuals().warn_fg_color, format!("This voice cannot speak yet: {why}."));
                        if cfg!(feature = "ai-ort") || v.engine == Engine::Piper {
                            ui.horizontal(|ui| {
                                ui.label("Install it with");
                                ui.code(install);
                                if ui.small_button("Copy").clicked() {
                                    ui.ctx().copy_text(install.to_string());
                                }
                            });
                        }
                    } else if let Some(fetch) = &self.fetch {
                        let got = fetch.done.load(Ordering::Relaxed);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::ProgressBar::new((got as f32 / fetch.total as f32).min(1.0))
                                    .desired_width(260.0)
                                    .text(format!("{} of {}", crate::model_download::fmt_bytes(got), crate::model_download::fmt_bytes(fetch.total))),
                            );
                            if ui.button("Cancel").clicked() {
                                fetch.cancel.store(true, Ordering::Relaxed);
                            }
                        });
                    } else if !v.installed {
                        let what = match v.engine {
                            Engine::Kokoro => format!("The narration voices are one download: {}, all ten Kokoro voices.", crate::model_download::fmt_bytes(v.size_bytes)),
                            Engine::Piper => format!("{} is a download of {}.", v.label, crate::model_download::fmt_bytes(v.size_bytes)),
                        };
                        ui.horizontal(|ui| {
                            ui.label(what);
                            if ui.button("Download").clicked() {
                                let v = v.clone();
                                self.start_download(&v);
                            }
                        });
                    } else if v.commercial_use {
                        ready = true;
                        ui.label(egui::RichText::new(format!("Licence: {}", v.license)).weak().small());
                    } else {
                        ready = true;
                        ui.colored_label(ui.visuals().warn_fg_color, format!("Not for commercial use: {}. Choose another voice for paid work.", v.license));
                    }
                }

                ui.add_space(8.0);
                let busy = self.working.is_some();
                let can = ready && !busy && !self.text.trim().is_empty();
                ui.horizontal(|ui| {
                    if ui.add_enabled(can, egui::Button::new("Preview")).on_hover_text("Hear your words before they go anywhere").clicked() {
                        asked = self.start(Then::Preview);
                    }
                    if ui.add_enabled(can, egui::Button::new("Add to Pool")).clicked() {
                        asked = self.start(Then::AddToPool);
                    }
                    if ui.add_enabled(can, egui::Button::new("Place at Playhead")).on_hover_text("On the Voice track, as dialogue, so a duck comes down under it").clicked() {
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
                        ui.label("Speaking…");
                    });
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
        if self.working.is_some() || self.fetch.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
        cmds
    }
}

/// A voice as the picker shows it: "Heart — female, US English", with the
/// default marked recommended.
fn picker_label(v: &Voice) -> String {
    let mut s = format!("{} — {}", v.label, v.about);
    if v.id == narration::DEFAULT_VOICE {
        s.push_str(" · recommended");
    }
    if !v.commercial_use {
        s.push_str(" · research only");
    }
    s
}
