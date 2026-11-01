//! The Delivery Check window's requests (#1978), run as the Colour
//! Check's tools are: on a worker, through the app's own control runner,
//! since the check renders and measures frames through this UI.

use super::KlippApp;
use crate::panels::delivery_check::DeliveryAction;

impl KlippApp {
    /// Do what the window asked: seek, or start a request on a worker.
    pub(super) fn act_on_delivery_check(&mut self, action: DeliveryAction) {
        match action {
            DeliveryAction::None => {}
            DeliveryAction::Seek(at) => {
                let rate = self.state.sequence().fps.rate();
                self.send_commands(vec![klipp_core::AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(at.max(0.0), rate))]);
            }
            DeliveryAction::ReuseWarns(warns) => set_reuse_warns(&mut self.state, warns),
            DeliveryAction::Run { request, check } => {
                let Some(run) = self.local_control.clone() else { return };
                if self.delivery_reply.is_some() {
                    return;
                }
                let (tx, rx) = std::sync::mpsc::channel();
                let spawned = std::thread::Builder::new().name("klipp-delivery-check".into()).spawn(move || {
                    let _ = tx.send(run(request));
                });
                match spawned {
                    Ok(_) => self.delivery_reply = Some((check, rx)),
                    Err(e) => {
                        let _ = self.delivery_check.answer(check, Err(format!("could not start: {e}")));
                    }
                }
            }
        }
    }

    /// The running request's answer, put in the window when it comes; a
    /// saved decision may start a fresh check.
    pub(super) fn take_delivery_reply(&mut self, ctx: &egui::Context) {
        let Some((check, rx)) = &self.delivery_reply else { return };
        let check = *check;
        let reply = match rx.try_recv() {
            Ok(reply) => reply,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the request stopped without an answer".into()),
        };
        self.delivery_reply = None;
        let next = self.delivery_check.answer(check, reply);
        self.act_on_delivery_check(next);
    }
}

/// The brief's "reused footage is a warning" set, one undo step; the rest
/// of the brief kept, and one made when there was none.
fn set_reuse_warns(state: &mut crate::state::AppState, warns: bool) {
    let seq = &mut state.sequences[state.active_sequence_idx];
    let old = seq.brief.clone();
    let brief = klipp_timeline::Brief { reused_footage_warns: warns, ..old.clone().unwrap_or_default() };
    seq.brief = Some(brief.clone());
    state.push_history(crate::history::EditCommand::SetBrief { old: old.map(Box::new), new: Some(Box::new(brief)) });
    state.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #3272: the switch writes the brief, keeping what else it says, and
    /// undo takes it back to no brief.
    #[test]
    fn reuse_warns_is_a_brief_edit_undone_in_one_step() {
        let mut s = crate::state::AppState::default();
        s.sequences[0].brief = None;
        set_reuse_warns(&mut s, true);
        assert!(s.sequence().brief.as_ref().is_some_and(|b| b.reused_footage_warns));
        s.sequences[0].brief.as_mut().unwrap().language = Some("sv".into());
        set_reuse_warns(&mut s, false);
        let b = s.sequence().brief.clone().unwrap();
        assert_eq!((b.reused_footage_warns, b.language.as_deref()), (false, Some("sv")));
        for _ in 0..2 {
            let entry = s.take_undo().expect("a step to undo");
            let active = s.active_sequence_idx;
            klipp_document::apply::DocumentView { sequences: &mut s.sequences, active, media_pool: &mut s.media_pool, fps: &mut s.fps }.apply_inverse(&entry.command);
        }
        assert!(s.sequence().brief.is_none());
    }
}
