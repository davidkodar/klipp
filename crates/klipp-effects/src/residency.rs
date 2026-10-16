//! Bounded picture-fallback diagnostics. CPU reference renders outside a frame
//! scope are deliberately excluded. Nested scopes keep the outer frame identity.
use serde::Serialize;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    marker::PhantomData,
    rc::Rc,
    sync::{Mutex, OnceLock},
};

/// The kind of work crossing the GPU-first boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Work {
    Fallback,
    Preparation,
}
/// One route, aggregated within a frame (clip IDs never enter cumulative keys).
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub feature: &'static str,
    pub reason: &'static str,
    pub clip: Option<uuid::Uuid>,
    pub work: Work,
    pub count: u64,
}
/// A completed render frame, including nested composition and failed GPU attempts.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Frame {
    pub scope: &'static str,
    pub seconds: f64,
    pub events: Vec<Event>,
    pub overflow: u64,
}
impl Frame {
    pub fn fallback_count(&self) -> u64 {
        self.events
            .iter()
            .filter(|e| e.work == Work::Fallback)
            .map(|e| e.count)
            .sum::<u64>()
            + self.overflow
    }
}
#[derive(Default)]
struct Local {
    depth: usize,
    frame: Frame,
    clip: Option<uuid::Uuid>,
    last: Frame,
}
thread_local! { static LOCAL: RefCell<Local> = RefCell::new(Local::default()); }
#[derive(Default)]
struct Totals {
    counts: BTreeMap<&'static str, u64>,
    latest: BTreeMap<&'static str, Frame>,
    overflow: u64,
}
static TOTALS: OnceLock<Mutex<Totals>> = OnceLock::new();
/// Process totals and latest completed viewer frame; exports cannot overwrite it.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub by_feature: BTreeMap<&'static str, u64>,
    pub overflow: u64,
    pub latest_viewer_frame: Option<Frame>,
}
pub fn snapshot() -> Snapshot {
    let t = TOTALS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    Snapshot {
        by_feature: t.counts.clone(),
        overflow: t.overflow,
        latest_viewer_frame: t.latest.get("viewer").cloned(),
    }
}
/// Last frame on this thread, for deterministic tests without process-global races.
pub fn last_frame() -> Frame {
    LOCAL.with(|l| l.borrow().last.clone())
}
/// Scoped frame collection; stays on the rendering thread.
pub struct FrameScope(PhantomData<Rc<()>>);
pub fn frame(scope: &'static str, seconds: f64) -> FrameScope {
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        if l.depth == 0 {
            l.frame = Frame {
                scope,
                seconds,
                ..Default::default()
            };
            l.clip = None;
        }
        l.depth += 1;
    });
    FrameScope(PhantomData)
}
impl Drop for FrameScope {
    fn drop(&mut self) {
        let done = LOCAL.with(|l| {
            let mut l = l.borrow_mut();
            l.depth -= 1;
            if l.depth != 0 {
                return None;
            }
            let f = std::mem::take(&mut l.frame);
            l.last = f.clone();
            Some(f)
        });
        if let Some(f) = done {
            let mut t = TOTALS
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for e in &f.events {
                if e.work != Work::Fallback {
                    continue;
                }
                if t.counts.len() < 256 || t.counts.contains_key(e.feature) {
                    let count = t.counts.entry(e.feature).or_default();
                    #[cfg(debug_assertions)]
                    if *count == 0 {
                        tracing::warn!(feature=e.feature,reason=e.reason,clip=?e.clip,"GPU picture fallback");
                    }
                    *count = count.saturating_add(e.count);
                } else {
                    t.overflow = t.overflow.saturating_add(e.count);
                }
            }
            t.overflow = t.overflow.saturating_add(f.overflow);
            if t.latest.len() < 8 || t.latest.contains_key(f.scope) {
                t.latest.insert(f.scope, f);
            }
        }
    }
}
/// Clip context inherited by effect kernels and restored after nested renders.
pub struct ClipScope(Option<uuid::Uuid>, PhantomData<Rc<()>>);
pub fn clip(id: uuid::Uuid) -> ClipScope {
    ClipScope(LOCAL.with(|l| l.borrow_mut().clip.replace(id)), PhantomData)
}
impl Drop for ClipScope {
    fn drop(&mut self) {
        LOCAL.with(|l| l.borrow_mut().clip = self.0);
    }
}
/// The single recording boundary. Counts work even when final composition succeeds on GPU.
pub fn record(feature: &'static str, reason: &'static str, work: Work) {
    LOCAL.with(|l| {
        let mut l = l.borrow_mut();
        if l.depth == 0 {
            return;
        }
        let clip = l.clip;
        if let Some(e) = l.frame.events.iter_mut().find(|e| {
            e.feature == feature && e.reason == reason && e.clip == clip && e.work == work
        }) {
            e.count = e.count.saturating_add(1);
        } else if l.frame.events.len() < 128 {
            l.frame.events.push(Event {
                feature,
                reason,
                clip,
                work,
                count: 1,
            });
        } else {
            l.frame.overflow = l.frame.overflow.saturating_add(1);
        }
    });
}
/// Registry identity, without serializing authored values or allocating strings.
pub fn effect_id(fx: &crate::BuiltinEffect) -> &'static str {
    use std::{
        collections::HashMap,
        mem::{discriminant, Discriminant},
    };
    static IDS: OnceLock<HashMap<Discriminant<crate::BuiltinEffect>, &'static str>> =
        OnceLock::new();
    IDS.get_or_init(|| {
        crate::registry::effect_types()
            .map(|t| (discriminant(&(t.sample)()), t.desc.id))
            .collect()
    })
    .get(&discriminant(fx))
    .copied()
    .unwrap_or("effect.unregistered")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_keep_outer_frame_and_clip_and_clear_next_frame() {
        {
            let _f = frame("test", 1.0);
            let id = uuid::Uuid::new_v4();
            let _c = clip(id);
            {
                let _n = frame("nested", 2.0);
                record("test.effect", "declined", Work::Fallback);
            }
            record("test.cache", "miss", Work::Preparation);
        }
        let f = last_frame();
        assert_eq!(f.scope, "test");
        assert_eq!(f.seconds, 1.0);
        assert_eq!(f.fallback_count(), 1);
        assert!(f.events[0].clip.is_some());
        {
            let _f = frame("test", 2.0);
        }
        assert_eq!(last_frame().fallback_count(), 0);
    }
    #[test]
    fn exports_do_not_replace_the_viewer_sample() {
        {
            let _f = frame("viewer", 123.5);
            record("test.viewer", "control", Work::Fallback);
        }
        {
            let _f = frame("export", 321.0);
            record("test.export", "control", Work::Fallback);
        }
        let s = snapshot();
        let f = s.latest_viewer_frame.unwrap();
        assert_eq!(f.seconds, 123.5);
        assert_eq!(f.events[0].feature, "test.viewer");
        assert!(s.by_feature["test.export"] >= 1);
    }
    #[test]
    fn capacity_overflow_is_visible() {
        {
            let _f = frame("overflow", 0.0);
            for _ in 0..130 {
                let _c = clip(uuid::Uuid::new_v4());
                record("test.effect", "declined", Work::Fallback);
            }
        }
        let f = last_frame();
        assert_eq!(f.events.len(), 128);
        assert_eq!(f.overflow, 2);
        assert_eq!(f.fallback_count(), 130);
    }
}
