//! A recipe from the Library's list, built through the same op an agent
//! sends: the control code finds the recipe by name and builds it, and the
//! edit comes back as one undo step.

use super::KlippApp;

/// What the list asks of the control code for `name` on `clip`.
pub(super) fn recipe_request(clip: klipp_core::ClipId, name: &str, slots: &[(String, String)]) -> serde_json::Value {
    let slots: serde_json::Map<String, serde_json::Value> = slots.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
    serde_json::json!({
        "method": "apply",
        "ops": [{ "op": "recipe", "clip_id": clip.0.to_string(), "name": name, "slots": slots }],
        "label": "Recipe",
    })
}

/// The op that sets a placed recipe's control: the one an agent sends.
pub(super) fn recipe_control_op(clip: klipp_core::ClipId, control: &str, value: f64) -> klipp_document::ops::Op {
    klipp_document::ops::Op::TemplateLink {
        clip_id: klipp_document::ops::ClipRef::Id(clip),
        control: Some(control.to_string()),
        target: None,
        action: None,
        value: Some(value),
    }
}

/// What the person is told when the build answers.
pub(super) fn recipe_said(name: &str, reply: &Result<serde_json::Value, String>) -> String {
    match reply {
        Ok(v) => v["text"].as_str().filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| format!("\"{name}\" applied")),
        Err(why) => format!("\"{name}\" was not applied: {why}"),
    }
}

impl KlippApp {
    /// `name` built into `clip`'s picture, on a thread: the build reads
    /// frames and files. One at a time.
    pub(super) fn apply_recipe(&mut self, clip: klipp_core::ClipId, name: String, slots: &[(String, String)]) {
        let Some(run) = self.local_control.clone() else {
            self.state.toast.show("Recipes can't be applied in this window");
            return;
        };
        if let Some((running, _)) = &self.recipe_reply {
            self.state.toast.show(format!("\"{running}\" is still being applied"));
            return;
        }
        let request = recipe_request(clip, &name, slots);
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("klipp-recipe".into()).spawn(move || {
            let _ = tx.send(run(request));
        });
        match spawned {
            Ok(_) => self.recipe_reply = Some((name, rx)),
            Err(e) => self.state.toast.show(format!("\"{name}\" could not start: {e}")),
        }
    }

    /// A placed recipe's control set, as the `template_link` op sets it,
    /// one undo step; why not, said.
    pub(super) fn set_recipe_control(&mut self, clip: klipp_core::ClipId, control: &str, value: f64) {
        let op = recipe_control_op(clip, control, value);
        let plan = klipp_document::ops::resolve(self.state.sequence(), &self.state.media_pool, std::slice::from_ref(&op));
        match plan {
            Ok(plan) => {
                let edits: Vec<_> = plan.steps.into_iter().map(|s| s.edit).collect();
                self.apply_edits_as_one(&edits, "Recipe Control");
            }
            Err(refused) => self.state.toast.show(refused.why),
        }
    }

    /// The build's answer, said when it comes.
    pub(super) fn take_recipe_reply(&mut self, ctx: &egui::Context) {
        let Some((name, rx)) = &self.recipe_reply else { return };
        let reply = match rx.try_recv() {
            Ok(reply) => reply,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("the build stopped without an answer".into()),
        };
        let said = recipe_said(name, &reply);
        self.recipe_reply = None;
        self.state.toast.show(said);
    }
}

#[cfg(test)]
mod tests {
    /// The list sends the op an agent sends, which the control code reads.
    #[test]
    fn the_list_asks_for_the_recipe_op() {
        let clip = klipp_core::ClipId::new();
        let r = super::recipe_request(clip, "Glitch", &[("screen".into(), "#00ff00".into())]);
        assert_eq!(r["method"], "apply");
        assert_eq!(r["ops"][0]["op"], "recipe");
        assert_eq!(r["ops"][0]["name"], "Glitch");
        assert_eq!(r["ops"][0]["clip_id"], clip.0.to_string());
        assert_eq!(r["ops"][0]["slots"]["screen"], "#00ff00");
        let op: klipp_document::ops::Op = serde_json::from_value(r["ops"][0].clone()).expect("an op the document reads");
        assert!(matches!(op, klipp_document::ops::Op::Recipe { .. }));
        assert_eq!(super::recipe_said("Glitch", &Err("no clip".into())), "\"Glitch\" was not applied: no clip");
    }
}
