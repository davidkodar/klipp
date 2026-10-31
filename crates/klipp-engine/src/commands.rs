//! Which [`AppCommand`]s the playback engine acts on, and the only way to
//! send it one (#437).
//!
//! The engine's channel carries the whole `AppCommand` enum, most of which
//! the UI's own dispatcher handles; the engine's `handle_command` ends in a
//! catch-all that drops the rest. Five commands were sent there over time
//! that the engine ignores — the white-balance and qualifier eyedroppers,
//! a clip move, a matte, a source load — and each did nothing, without a
//! word. [`EngineSender`] is that channel's only sender: a command the
//! engine does not act on is refused there, said in the log by name, and
//! fails a debug build or a test on the spot.
//!
//! [`NAMES`] is kept to what the engine's own code names; a test fails when
//! the two part.

use klipp_core::AppCommand;

macro_rules! engine_commands {
    ($($v:ident),* $(,)?) => {
        /// Whether the engine acts on `cmd`.
        pub fn handles(cmd: &AppCommand) -> bool {
            matches!(cmd, $(AppCommand::$v { .. })|*)
        }

        /// The commands the engine acts on, by name.
        pub const NAMES: &[&str] = &[$(stringify!($v)),*];
    };
}

engine_commands!(
    BeginNodeParamEdit, BeginScrub, SkimTimeline, CleanOrphanedCaches, ClearGlobalLut, ClearRenderCache, CancelRenderRange,
    AbandonNodeParamEdit, EndNodeParamEdit, EndScrub, LoadGlobalLut, LoadMediaFile, MediaDecodeChanged, Pause,
    PauseSource, Play, PlaySource, Quit, RenderNodeThumbsFromSource,
    RenderScopesForFrame, RenderScopeSnapshot, ResetEngine, ResetLufsMeter,
    RenderRange, SampleNodeInput, SeekRelative, SeekSource, SeekTo, SequenceUpdated, RefreshPreview,
    SetActiveScopeModes, SetAntiClickFade, SetDeliveryTarget, SetClipChannelConfig, SetClipGain,
    SetClipPan, SetClipVolume, SetGlobalLutEnabled, SetGlobalLutOpacity, SetHdrMonitor,
    SetHdrReferenceOutput, SetInspectorMattePreviewTarget, SetMeasureOnlyClip, SetLoopPlayback,
    SetLutTetrahedral, SetMasterVolume, SetMattePreview, SetNodeSolo, SetPanLaw,
    SetPlayhead, SetRenderCacheEnabled, SetRenderCacheLimitGb, ClearRenderCacheAll, SetScopeBloom, SetScopeLowPass, SetScopeLut,
    SetScopeRenderMode, SetScopeSettings, SetScopeSizes, SetScopesVisible,
    SetScrubAudioEnabled, SetSdrTransfer, SetSourceMedia, SetSpeed,
    SetSplitCompareNode, SetTrackMute, SetTrackPan, SetTrackSolo, SetTrackVolume, OverrideTrackAutomation,
    SetUseProxies, SetViewChannel, SetVisibleNodeThumbs, StepSource, Stop, StopSource,
    ToggleBeforeAfter, ToggleStabilizationCompare, InjectPanic, SetFreezeFolder, SetFrozenTracks, SetFrozenStems
);

/// A command's variant name, for saying which one went astray.
pub fn variant_name(cmd: &AppCommand) -> String {
    let debug = format!("{cmd:?}");
    debug.split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("").to_string()
}

/// Why a command did not go to the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotSent {
    /// The engine does not act on it: it belongs to the UI's dispatcher.
    NotEngine(String),
    /// The engine has stopped.
    Closed,
}

/// The engine's command channel, taking only what the engine acts on.
#[derive(Clone)]
pub struct EngineSender(crossbeam_channel::Sender<AppCommand>);

impl EngineSender {
    pub fn new(tx: crossbeam_channel::Sender<AppCommand>) -> Self {
        Self(tx)
    }

    /// Send `cmd` to the engine. One it does not act on is refused, named
    /// in the log, and stops a debug build or a test: sent there it would
    /// have done nothing at all.
    pub fn send(&self, cmd: AppCommand) -> Result<(), NotSent> {
        if !handles(&cmd) {
            let name = variant_name(&cmd);
            tracing::error!("{name} was sent to the playback engine, which does not act on it; it did nothing. It belongs to the UI's dispatcher (send_commands).");
            debug_assert!(false, "{name} was sent to the playback engine, which does not act on it (#437)");
            return Err(NotSent::NotEngine(name));
        }
        self.0.send(cmd).map_err(|_| NotSent::Closed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's code names exactly the commands in [`NAMES`]: a new arm
    /// in `handle_command` without its name here, or a name here the engine
    /// no longer acts on, fails this (#437).
    #[test]
    fn the_list_is_what_the_engine_handles() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![std::fs::read_to_string(dir.join("engine.rs")).unwrap()];
        for entry in std::fs::read_dir(dir.join("engine")).unwrap() {
            sources.push(std::fs::read_to_string(entry.unwrap().path()).unwrap());
        }
        let mut named = std::collections::BTreeSet::new();
        // The whole source, tests too: they send the engine what it acts
        // on, and a name only a test used would show up here as unlisted.
        for text in &sources {
            let mut rest = text.as_str();
            while let Some(at) = rest.find("AppCommand::") {
                rest = &rest[at + "AppCommand::".len()..];
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if name.starts_with(|c: char| c.is_ascii_uppercase()) {
                    named.insert(name);
                }
            }
        }
        let listed: std::collections::BTreeSet<String> = NAMES.iter().map(|s| s.to_string()).collect();
        let unlisted: Vec<_> = named.difference(&listed).collect();
        let unhandled: Vec<_> = listed.difference(&named).collect();
        assert!(unlisted.is_empty() && unhandled.is_empty(), "the engine names {unlisted:?} that are not in NAMES, and NAMES has {unhandled:?} the engine does not name");
    }

    /// A command the engine acts on goes; one it does not is refused, by
    /// name, and never reaches the channel.
    #[test]
    fn only_engine_commands_are_sent() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sender = EngineSender::new(tx);
        assert_eq!(sender.send(AppCommand::Play), Ok(()));
        assert!(matches!(rx.try_recv(), Ok(AppCommand::Play)));
        assert!(handles(&AppCommand::SeekTo(klipp_core::Timecode::zero())));
        assert!(!handles(&AppCommand::ShowPreferences), "a UI command");
        assert_eq!(variant_name(&AppCommand::SeekTo(klipp_core::Timecode::zero())), "SeekTo");
        let refused = std::panic::catch_unwind(|| sender.send(AppCommand::ShowPreferences));
        // A debug build stops on it; a release build refuses it.
        match refused {
            Err(_) => assert!(cfg!(debug_assertions)),
            Ok(r) => assert_eq!(r, Err(NotSent::NotEngine("ShowPreferences".into()))),
        }
        assert!(rx.try_recv().is_err(), "nothing went to the engine");
    }
}
