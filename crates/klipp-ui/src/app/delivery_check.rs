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
