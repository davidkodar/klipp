//! Whether the GPU driver has been refusing work this session, said in
//! a person's words (#2394). A driver out of channels or memory refuses
//! new work, sometimes until a reboot. Klipp goes on in software, and
//! this says so, so the slowdown isn't taken for a Klipp bug.

/// What the GPU refused this session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refusals {
    /// Streams that went on in software after the GPU refused their frames.
    pub streams_to_software: u64,
    /// GPU decoder opens that ran out of time.
    pub opens_timed_out: u64,
    /// Out-of-memory errors from the GPU device.
    pub out_of_memory: u64,
}

impl Refusals {
    /// The counts so far.
    pub fn now() -> Self {
        Self {
            streams_to_software: crate::video_decoder::streams_moved_to_software(),
            opens_timed_out: crate::video_decoder::hw_opens_timed_out(),
            out_of_memory: klipp_effects::gpu_context::out_of_memory_errors(),
        }
    }

    /// The warning to show, or `None` when the GPU refused nothing.
    pub fn warning(&self) -> Option<String> {
        let mut what = Vec::new();
        let n = |count: u64, one: &str, many: &str| if count == 1 { format!("1 {one}") } else { format!("{count} {many}") };
        if self.streams_to_software > 0 {
            what.push(n(self.streams_to_software, "video moved to software decode", "videos moved to software decode"));
        }
        if self.opens_timed_out > 0 {
            what.push(n(self.opens_timed_out, "GPU decoder that would not open", "GPU decoders that would not open"));
        }
        if self.out_of_memory > 0 {
            what.push(n(self.out_of_memory, "GPU out-of-memory error", "GPU out-of-memory errors"));
        }
        if what.is_empty() {
            return None;
        }
        Some(format!("The GPU driver is refusing new work ({}); Klipp goes on in software, slower. A reboot may be needed.", what.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_refused_says_nothing() {
        assert_eq!(Refusals::default().warning(), None);
    }

    #[test]
    fn the_warning_names_what_was_refused() {
        let w = Refusals { streams_to_software: 1, opens_timed_out: 0, out_of_memory: 3 }.warning().unwrap();
        assert!(w.contains("refusing new work"), "{w}");
        assert!(w.contains("1 video moved to software decode"), "{w}");
        assert!(w.contains("3 GPU out-of-memory errors"), "{w}");
        assert!(!w.contains("would not open"), "{w}");
        assert!(w.contains("reboot"), "{w}");
    }
}
