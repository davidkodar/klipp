//! The Delivery Check window (#1978): the check an assistant runs before
//! a piece goes out — the craft's rules, the brief, and on request the
//! colour of every shot and the mix's loudness — run from the app, with
//! each finding reviewed where it is read.
//!
//! The window sends the same requests an assistant does, `check` and
//! `review_finding`, through the app's own control runner, so a finding
//! has the same id, and a decision the same effect, from the app, the
//! socket and MCP. A decision is one undo step like any other edit.
//! Accepted findings are listed apart with their reasons, not counted;
//! a review whose finding is gone is listed too, so a decision that no
//! longer holds can be taken back.
use crate::widgets::menu::SubmenuRows;

use std::time::Instant;

use serde_json::{json, Value};

use crate::theme::{type_scale, KlippTheme};

/// What the window asks of the app this frame.
#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryAction {
    None,
    /// Run this control request on a worker; `check` says whether it is
    /// the check itself or a review of one of its findings.
    Run { request: Value, check: bool },
    /// Put the playhead here, in timeline seconds.
    Seek(f64),
    /// Set the brief's "reused footage is a warning" (#3272).
    ReuseWarns(bool),
}

/// A finding still counted: open, or marked to fix.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub severity: String,
    pub at_secs: Option<f64>,
    pub what: String,
    pub hint: String,
    /// The note of a finding marked to fix.
    pub to_fix: Option<String>,
}

/// A finding kept on purpose, with why.
#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    pub id: String,
    pub severity: String,
    pub at_secs: Option<f64>,
    pub what: String,
    pub rationale: String,
    pub revision: u64,
}

/// A review whose finding the last check did not find.
#[derive(Debug, Clone, PartialEq)]
pub struct Lapsed {
    pub id: String,
    pub decision: String,
    pub at_secs: Option<f64>,
    pub rationale: String,
}

/// A check's reply sorted for the window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sorted {
    pub rows: Vec<Row>,
    pub accepted: Vec<Accepted>,
    pub lapsed: Vec<Lapsed>,
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "error" => 0,
        "warning" => 1,
        _ => 2,
    }
}

/// The findings of a `check` reply: craft, colour and sound together,
/// the worst first and then by time, as the report orders them.
pub fn sort_reply(reply: &Value) -> Sorted {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    let mut rows: Vec<Row> = ["findings", "color_findings", "sound_findings"]
        .iter()
        .flat_map(|k| reply[*k].as_array().cloned().unwrap_or_default())
        .filter_map(|f| {
            let id = f["id"].as_str()?.to_string();
            let what = match f["media"].as_str() {
                Some(media) => format!("{media}: {}", s(&f, "what")),
                None => s(&f, "what"),
            };
            let to_fix = (f["review"]["decision"] == "to_fix").then(|| s(&f["review"], "rationale"));
            Some(Row { id, severity: s(&f, "severity"), at_secs: f["at_secs"].as_f64(), what, hint: s(&f, "hint"), to_fix })
        })
        .collect();
    rows.sort_by(|a, b| severity_rank(&a.severity).cmp(&severity_rank(&b.severity)).then(a.at_secs.unwrap_or(-1.0).total_cmp(&b.at_secs.unwrap_or(-1.0))));
    let accepted = reply["accepted"].as_array().cloned().unwrap_or_default().iter().map(|a| Accepted {
        id: s(a, "id"),
        severity: s(a, "severity"),
        at_secs: a["at_secs"].as_f64(),
        what: s(a, "what"),
        rationale: s(a, "rationale"),
        revision: a["revision"].as_u64().unwrap_or(0),
    }).collect();
    let lapsed = reply["lapsed_reviews"].as_array().cloned().unwrap_or_default().iter().map(|r| Lapsed {
        id: s(r, "finding_id"),
        decision: s(r, "decision"),
        at_secs: r["at_secs"].as_f64(),
        rationale: s(r, "rationale"),
    }).collect();
    Sorted { rows, accepted, lapsed }
}

/// The `check` request, with the slower passes asked for or not.
pub fn check_request(colour: bool, loudness: bool) -> Value {
    json!({ "method": "check", "color": colour, "loudness": loudness })
}

/// The `review_finding` request for a decision about finding `id`.
/// Accepting needs a reason, as the request itself does: refused here
/// so the window says so before anything is sent.
pub fn review_request(id: &str, decision: &str, reason: &str) -> Result<Value, &'static str> {
    let reason = reason.trim();
    if decision == "accepted" && reason.is_empty() {
        return Err("say why it is kept on purpose");
    }
    let mut v = json!({ "method": "review_finding", "finding_id": id, "decision": decision });
    if !reason.is_empty() {
        v["rationale"] = json!(reason);
    }
    Ok(v)
}

/// What a check running for `secs` with these passes is doing, for the
/// progress line: the check says nothing until it is done, so the line
/// says what it is working through and for how long.
pub fn progress_line(colour: bool, loudness: bool, secs: f64) -> String {
    let mut parts = vec!["the cut against the craft's rules and the brief"];
    if colour {
        parts.push("a frame of every shot measured for colour");
    }
    if loudness {
        parts.push("the whole mix metered");
    }
    let what = match parts.as_slice() {
        [one] => one.to_string(),
        [a, b] => format!("{a}, and {b}"),
        _ => format!("{}, and {}", parts[..parts.len() - 1].join(", "), parts[parts.len() - 1]),
    };
    format!("Checking {what} — {secs:.0} s")
}

/// What the window says before anything has been checked.
pub const EMPTY: &str = "The check reads the cut the way an editor does on a last watch: black and flash frames, jump cuts, titles across cuts or too short to read, captions under titles, cuts inside words, music left loud under speech, ends apart, offline media, and the brief's length, size, captions and cast. With colour, a frame of every shot is measured; with loudness, the mix is metered for quiet stretches. Nothing is changed.";

/// The reason or note being typed for one finding.
#[derive(Debug, Clone, PartialEq)]
struct Draft {
    id: String,
    decision: &'static str,
    text: String,
}

/// The window's state between frames.
#[derive(Default)]
pub struct DeliveryCheckPanel {
    pub colour: bool,
    pub loudness: bool,
    /// The last check's reply, and the passes it ran with.
    reply: Option<(Value, bool, bool)>,
    /// A check running since then, with its passes.
    pub running_check: Option<(Instant, bool, bool)>,
    /// A review being saved.
    pub running_review: bool,
    /// A decision has been saved since the last check, which a slow check
    /// was not re-run for.
    stale: bool,
    draft: Option<Draft>,
    /// The last thing that went wrong, said in the window.
    pub error: Option<String>,
}

impl DeliveryCheckPanel {
    /// A request's answer, put in the window.
    pub fn answer(&mut self, check: bool, reply: Result<Value, String>) -> DeliveryAction {
        if check {
            let (colour, loudness) = self.running_check.take().map(|(_, c, l)| (c, l)).unwrap_or((self.colour, self.loudness));
            match reply {
                Ok(v) => {
                    self.reply = Some((v, colour, loudness));
                    self.stale = false;
                    self.error = None;
                }
                Err(e) => self.error = Some(format!("The check did not finish: {e}")),
            }
            return DeliveryAction::None;
        }
        self.running_review = false;
        match reply {
            Ok(_) => {
                self.draft = None;
                self.error = None;
                // A check without the slow passes is quick: run it again so
                // the lists follow the decision. A slow one is not re-run
                // unasked; the window says its results are from before.
                match &self.reply {
                    Some((_, false, false)) => self.start_check(false, false),
                    _ => {
                        self.stale = true;
                        DeliveryAction::None
                    }
                }
            }
            Err(e) => {
                self.error = Some(e);
                DeliveryAction::None
            }
        }
    }

    fn start_check(&mut self, colour: bool, loudness: bool) -> DeliveryAction {
        self.running_check = Some((Instant::now(), colour, loudness));
        DeliveryAction::Run { request: check_request(colour, loudness), check: true }
    }

    fn busy(&self) -> bool {
        self.running_check.is_some() || self.running_review
    }

    /// `reuse_warns`: the brief's `reused_footage_warns`, shown as a switch.
    pub fn show(&mut self, ctx: &egui::Context, theme: &KlippTheme, tools: bool, reuse_warns: bool, open: &mut bool) -> DeliveryAction {
        let mut action = DeliveryAction::None;
        egui::Window::new("Delivery Check")
            .open(open)
            .default_size([460.0, 560.0])
            .min_width(320.0)
            .min_height(220.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                if !tools {
                    ui.label(egui::RichText::new("The check runs through the app's control runner, which is not available in this session.").size(type_scale::BODY).color(theme.text_secondary));
                    return;
                }
                self.header(ui, theme, &mut action);
                let mut warns = reuse_warns;
                if ui.checkbox(&mut warns, "Treat reused footage as warnings").on_hover_text("Footage shown twice in this sequence is a note; for a stock-heavy piece, make it a warning. Saved in the brief.").changed() {
                    action = DeliveryAction::ReuseWarns(warns);
                }
                if let Some(e) = &self.error {
                    ui.label(egui::RichText::new(e).size(type_scale::BODY).color(theme.marker_error));
                }
                let Some((reply, _, _)) = self.reply.clone() else {
                    if self.running_check.is_none() {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Nothing checked yet").size(type_scale::HEADLINE).color(theme.text_primary));
                        ui.label(egui::RichText::new(EMPTY).size(type_scale::BODY).color(theme.text_secondary));
                    }
                    return;
                };
                ui.separator();
                self.summary(ui, theme, &reply);
                let sorted = sort_reply(&reply);
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.rows(ui, theme, &sorted.rows, &mut action);
                    self.accepted(ui, theme, &sorted.accepted, &mut action);
                    self.lapsed(ui, theme, &sorted.lapsed, &mut action);
                });
            });
        action
    }

    fn header(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, action: &mut DeliveryAction) {
        ui.horizontal(|ui| {
            let check = ui.add_enabled(!self.busy(), egui::Button::new("Check")).on_hover_text("Run the check over the whole sequence");
            if check.clicked() {
                *action = self.start_check(self.colour, self.loudness);
            }
            ui.add_space(8.0);
            ui.add(crate::widgets::toggle::toggle(&mut self.colour)).on_hover_text("Measure a frame of every shot for colour: slower, it renders frames");
            ui.label(egui::RichText::new("Colour").size(type_scale::CONTROL).color(theme.text_secondary));
            ui.add_space(6.0);
            ui.add(crate::widgets::toggle::toggle(&mut self.loudness)).on_hover_text("Meter the whole mix for stretches well under the rest: slower, it mixes the piece's sound");
            ui.label(egui::RichText::new("Loudness").size(type_scale::CONTROL).color(theme.text_secondary));
        });
        if let Some((since, colour, loudness)) = self.running_check {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(progress_line(colour, loudness, since.elapsed().as_secs_f64())).size(type_scale::BODY).color(theme.text_secondary));
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        } else if self.running_review {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new("Saving the decision").size(type_scale::BODY).color(theme.text_secondary));
            });
        }
    }

    fn summary(&self, ui: &mut egui::Ui, theme: &KlippTheme, reply: &Value) {
        let n = |k: &str| reply[k].as_u64().unwrap_or(0);
        let colour = if n("errors") > 0 { theme.marker_error } else if n("warnings") > 0 { theme.marker_warning } else { theme.marker_success };
        let accepted = reply["accepted"].as_array().map_or(0, Vec::len);
        let mut head = format!("{} error{}, {} warning{}", n("errors"), if n("errors") == 1 { "" } else { "s" }, n("warnings"), if n("warnings") == 1 { "" } else { "s" });
        if accepted > 0 {
            head.push_str(&format!(", {accepted} accepted"));
        }
        ui.label(egui::RichText::new(head).size(type_scale::TITLE).color(colour));
        if let Some(c) = reply["colour_coverage"].as_object() {
            let g = |k: &str| c.get(k).and_then(Value::as_u64).unwrap_or(0);
            ui.label(egui::RichText::new(format!("Colour: {} shot{} of footage and {} of generated graphics measured, {} not read", g("footage"), if g("footage") == 1 { "" } else { "s" }, g("graphics"), g("unread"))).size(type_scale::CAPTION).color(theme.text_dim));
        }
        if let Some(next) = reply["next"].as_str() {
            ui.label(egui::RichText::new(next).size(type_scale::BODY).color(theme.text_secondary));
        }
        if self.stale {
            ui.label(egui::RichText::new("A decision has been saved since this check: Check again to see the lists follow it.").size(type_scale::BODY).color(theme.marker_info));
        }
    }

    fn rows(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, rows: &[Row], action: &mut DeliveryAction) {
        if rows.is_empty() {
            ui.label(egui::RichText::new("Nothing found that needs a decision.").size(type_scale::BODY).color(theme.text_secondary));
        }
        let mut last = "";
        for r in rows {
            if r.severity != last {
                section(ui, theme, match r.severity.as_str() { "error" => "ERRORS", "warning" => "WARNINGS", _ => "NOTES" });
                last = &r.severity;
            }
            let colour = match r.severity.as_str() { "error" => theme.marker_error, "warning" => theme.marker_warning, _ => theme.text_secondary };
            ui.horizontal_wrapped(|ui| {
                if let Some(at) = r.at_secs {
                    if ui.link(egui::RichText::new(format!("{at:.2} s")).size(type_scale::BODY).color(theme.text_accent)).on_hover_text("Put the playhead here").clicked() {
                        *action = DeliveryAction::Seek(at);
                    }
                }
                ui.label(egui::RichText::new(&r.what).size(type_scale::BODY).color(colour)).on_hover_text(&r.hint);
            });
            if let Some(note) = &r.to_fix {
                ui.label(egui::RichText::new(if note.is_empty() { "To fix".to_string() } else { format!("To fix: {note}") }).size(type_scale::CAPTION).color(theme.marker_info));
            }
            self.decide(ui, &r.id, r.to_fix.is_some(), action);
            ui.add_space(2.0);
        }
    }

    /// The decisions a row offers, in a menu drawn as the app's menus are,
    /// and the reason or note being typed for one.
    fn decide(&mut self, ui: &mut egui::Ui, id: &str, reviewed: bool, action: &mut DeliveryAction) {
        let busy = self.busy();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                ui.menu_rows(egui::RichText::new("Decide").size(type_scale::CAPTION), |ui| {
                    crate::widgets::menu::menu_body(ui, |ui| {
                        if ui.button("Accept on purpose…").clicked() {
                            self.draft = Some(Draft { id: id.to_string(), decision: "accepted", text: String::new() });
                            ui.close_menu();
                        }
                        if ui.button("To fix…").clicked() {
                            self.draft = Some(Draft { id: id.to_string(), decision: "to_fix", text: String::new() });
                            ui.close_menu();
                        }
                        if reviewed && ui.button("Clear the decision").clicked() {
                            if let Ok(request) = review_request(id, "clear", "") {
                                self.running_review = true;
                                *action = DeliveryAction::Run { request, check: false };
                            }
                            ui.close_menu();
                        }
                    });
                });
            });
        });
        // Only this row's draft; another row's stays where it is.
        if self.draft.as_ref().is_none_or(|d| d.id != id) {
            return;
        }
        let Some(mut draft) = self.draft.take() else { return };
        let accepting = draft.decision == "accepted";
        let mut keep = true;
        ui.horizontal(|ui| {
            let hint = if accepting { "Why it is kept on purpose" } else { "What the fix is (optional)" };
            ui.add(egui::TextEdit::singleline(&mut draft.text).hint_text(hint).desired_width(ui.available_width() - 120.0));
            let ready = review_request(id, draft.decision, &draft.text);
            let save = ui.add_enabled(ready.is_ok() && !busy, egui::Button::new(if accepting { "Accept" } else { "Mark" }));
            let save = match &ready {
                Err(why) => save.on_disabled_hover_text(*why),
                Ok(_) => save,
            };
            if save.clicked() {
                if let Ok(request) = ready {
                    self.running_review = true;
                    *action = DeliveryAction::Run { request, check: false };
                }
            }
            if ui.button("Cancel").clicked() {
                keep = false;
            }
        });
        if keep {
            self.draft = Some(draft);
        }
    }

    fn accepted(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, accepted: &[Accepted], action: &mut DeliveryAction) {
        if accepted.is_empty() {
            return;
        }
        ui.add_space(6.0);
        egui::CollapsingHeader::new(egui::RichText::new(format!("ACCEPTED ON PURPOSE ({})", accepted.len())).size(type_scale::CAPTION).color(theme.text_dim))
            .default_open(true)
            .show(ui, |ui| {
                for a in accepted {
                    ui.horizontal_wrapped(|ui| {
                        if let Some(at) = a.at_secs {
                            if ui.link(egui::RichText::new(format!("{at:.2} s")).size(type_scale::BODY).color(theme.text_accent)).clicked() {
                                *action = DeliveryAction::Seek(at);
                            }
                        }
                        ui.label(egui::RichText::new(format!("[{}] {}", a.severity, a.what)).size(type_scale::BODY).color(theme.text_secondary));
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(format!("Why: {} (revision {})", a.rationale, a.revision)).size(type_scale::CAPTION).color(theme.text_primary));
                        if ui.add_enabled(!self.busy(), egui::Button::new(egui::RichText::new("Clear").size(type_scale::CAPTION))).on_hover_text("Take the decision back: the finding is counted again").clicked() {
                            if let Ok(request) = review_request(&a.id, "clear", "") {
                                self.running_review = true;
                                *action = DeliveryAction::Run { request, check: false };
                            }
                        }
                    });
                }
            });
    }

    fn lapsed(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, lapsed: &[Lapsed], action: &mut DeliveryAction) {
        if lapsed.is_empty() {
            return;
        }
        ui.add_space(6.0);
        egui::CollapsingHeader::new(egui::RichText::new(format!("NO LONGER HOLDING ({})", lapsed.len())).size(type_scale::CAPTION).color(theme.text_dim))
            .default_open(false)
            .show(ui, |ui| {
                ui.label(egui::RichText::new("What these decisions were about has changed or is gone, so the finding, if it is still there, is counted again.").size(type_scale::CAPTION).color(theme.text_secondary));
                for l in lapsed {
                    ui.horizontal_wrapped(|ui| {
                        let at = l.at_secs.map(|t| format!("{t:.2} s  ")).unwrap_or_default();
                        let was = if l.decision == "accepted" { "accepted" } else { "to fix" };
                        ui.label(egui::RichText::new(format!("{at}was {was}: {}", l.rationale)).size(type_scale::BODY).color(theme.text_secondary)).on_hover_text(&l.id);
                        if ui.add_enabled(!self.busy(), egui::Button::new(egui::RichText::new("Clear").size(type_scale::CAPTION))).clicked() {
                            if let Ok(request) = review_request(&l.id, "clear", "") {
                                self.running_review = true;
                                *action = DeliveryAction::Run { request, check: false };
                            }
                        }
                    });
                }
            });
    }
}

fn section(ui: &mut egui::Ui, theme: &KlippTheme, title: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(title).size(type_scale::CAPTION).color(theme.text_dim));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> Value {
        json!({
            "errors": 1, "warnings": 1, "next": "Fix the errors",
            "findings": [
                { "id": "check/flash/25/aaaaaaaaaaaa", "severity": "warning", "at_secs": 1.0, "what": "a flash", "hint": "lengthen it", "review": { "decision": "to_fix", "rationale": "trim it", "revision": 4 } },
                { "id": "check/no_brief/-/bbbbbbbbbbbb", "severity": "note", "at_secs": null, "what": "no brief", "hint": "set one", "review": null }
            ],
            "color_findings": [ { "id": "colour/black_point/x/0/cccccccccccc", "severity": "error", "at_secs": 3.0, "what": "black point L* 36 [black_point]", "hint": "lift", "review": null } ],
            "sound_findings": [],
            "accepted": [ { "id": "colour/cast/x/0/dddddddddddd", "severity": "warning", "at_secs": 5.0, "what": "a warm cast", "rationale": "sunset", "revision": 3 } ],
            "lapsed_reviews": [ { "finding_id": "check/flash/50/eeeeeeeeeeee", "decision": "accepted", "rationale": "on the beat", "at_secs": 2.0 } ]
        })
    }

    /// #1978: a check's reply sorts into the counted findings (the worst
    /// first, a to-fix one with its note), the accepted ones with their
    /// reasons, and the reviews no longer holding.
    #[test]
    fn a_reply_sorts_into_counted_accepted_and_lapsed() {
        let s = sort_reply(&reply());
        assert_eq!(s.rows.iter().map(|r| r.severity.as_str()).collect::<Vec<_>>(), vec!["error", "warning", "note"]);
        assert_eq!(s.rows[1].to_fix.as_deref(), Some("trim it"));
        assert_eq!(s.rows[0].to_fix, None);
        assert_eq!(s.accepted.len(), 1);
        assert_eq!((s.accepted[0].rationale.as_str(), s.accepted[0].revision), ("sunset", 3));
        assert_eq!(s.lapsed.len(), 1);
        assert_eq!(s.lapsed[0].id, "check/flash/50/eeeeeeeeeeee");
    }

    /// Accepting needs a reason, as the request does; a note to fix and a
    /// clear do not, and each is the request an assistant would send.
    #[test]
    fn a_decision_is_the_request_an_assistant_sends() {
        assert!(review_request("check/flash/25/aaaaaaaaaaaa", "accepted", "  ").is_err());
        let v = review_request("check/flash/25/aaaaaaaaaaaa", "accepted", "on the beat").unwrap();
        assert_eq!(v, json!({ "method": "review_finding", "finding_id": "check/flash/25/aaaaaaaaaaaa", "decision": "accepted", "rationale": "on the beat" }));
        assert_eq!(review_request("x", "to_fix", "").unwrap(), json!({ "method": "review_finding", "finding_id": "x", "decision": "to_fix" }));
        assert_eq!(review_request("x", "clear", "").unwrap()["decision"], "clear");
        assert_eq!(check_request(true, false), json!({ "method": "check", "color": true, "loudness": false }));
    }

    /// The progress line says what is being worked through.
    #[test]
    fn the_progress_line_says_what_runs() {
        assert_eq!(progress_line(false, false, 2.4), "Checking the cut against the craft's rules and the brief — 2 s");
        let all = progress_line(true, true, 61.0);
        assert!(all.contains("measured for colour") && all.contains("and the whole mix metered") && all.ends_with("61 s"), "{all}");
    }

    /// A saved decision re-runs a quick check so the lists follow it, and
    /// leaves a slow one alone, said as out of date.
    #[test]
    fn a_saved_decision_refreshes_a_quick_check_only() {
        let mut p = DeliveryCheckPanel::default();
        p.answer(true, Ok(reply()));
        p.running_review = true;
        let next = p.answer(false, Ok(json!({ "text": "Accepted" })));
        assert!(matches!(&next, DeliveryAction::Run { check: true, request } if request["color"] == false), "{next:?}");
        assert!(!p.running_review && !p.stale);
        let mut slow = DeliveryCheckPanel { running_check: Some((Instant::now(), true, false)), ..Default::default() };
        slow.answer(true, Ok(reply()));
        assert_eq!(slow.answer(false, Ok(json!({}))), DeliveryAction::None);
        assert!(slow.stale);
        // A refusal is said, and the check is not re-run.
        assert_eq!(slow.answer(false, Err("no".into())), DeliveryAction::None);
        assert_eq!(slow.error.as_deref(), Some("no"));
    }

    /// Drawn from a reply, the window shows its three lists; before any
    /// check it says what the check covers.
    #[test]
    fn the_window_draws_its_lists_and_an_empty_state() {
        let theme = KlippTheme::default();
        let ctx = egui::Context::default();
        let texts = |p: &mut DeliveryCheckPanel| {
            let mut seen = Vec::new();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                let mut open = true;
                p.show(ctx, &theme, true, false, &mut open);
            });
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                let mut open = true;
                p.show(ctx, &theme, true, false, &mut open);
            });
            for shape in out.shapes {
                collect_text(&shape.shape, &mut seen);
            }
            seen.join("\n")
        };
        let mut empty = DeliveryCheckPanel::default();
        let shown = texts(&mut empty);
        assert!(shown.contains("Nothing checked yet"), "{shown}");
        let mut full = DeliveryCheckPanel::default();
        full.answer(true, Ok(reply()));
        let shown = texts(&mut full);
        for want in ["ERRORS", "WARNINGS", "NOTES", "To fix: trim it", "ACCEPTED ON PURPOSE (1)", "Why: sunset", "NO LONGER HOLDING (1)"] {
            assert!(shown.contains(want), "{want} missing from:\n{shown}");
        }
    }

    fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| collect_text(s, out)),
            _ => {}
        }
    }
}
