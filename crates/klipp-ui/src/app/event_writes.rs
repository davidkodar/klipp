//! Which events write the document: a background job's result, landing
//! from a drained event and not through the dispatch boundary, where an
//! open path drag is closed before any writer (`closes_path_drag`). The
//! drains close one before each event named here as writing (#1327), so
//! the result is a step of its own, never taken into the drag's.
//!
//! Every event is named, with no catch-all: a new one doesn't compile
//! until it is put on one side. One not known to write nothing is with
//! the writers, as in `command_writes`: closing a drag only splits it,
//! while a result left inside one would be undone with it.

use klipp_core::AppEvent;

/// Whether a handler of `event` can write the picture, the saved project
/// or the history.
pub(crate) fn event_writes(event: &AppEvent) -> bool {
    match event {
        // A job's result, or a sample a tool writes (the qualifier's pick):
        // each lands on a clip, a media item or the history.
        AppEvent::NodeInputSampled { .. }
        | AppEvent::MediaLoaded(..)
        | AppEvent::MediaFileChanged { .. }
        | AppEvent::ProxyGenerationComplete { .. }
        | AppEvent::AudioConformComplete { .. }
        | AppEvent::SkimTranscodeComplete { .. }
        | AppEvent::DeflickerComplete { .. }
        | AppEvent::StabilizationComplete { .. }
        | AppEvent::SceneDetectComplete { .. }
        | AppEvent::TrackingComplete { .. }
        | AppEvent::SurfaceTracked { .. }
        | AppEvent::SurfaceCorrected { .. }
        | AppEvent::SurfaceSnapped { .. }
        | AppEvent::MediaSyncComplete { .. }
        | AppEvent::MulticamSyncReady { .. }
        | AppEvent::AudioSyncComplete { .. }
        | AppEvent::DenoiserProfileLearned { .. }
        | AppEvent::NormalizeMeasured { .. }
        | AppEvent::WaterMeasured { .. } => true,
        // What the viewer, the meters and the panels show, a job's progress,
        // start or failure, and the engine's own echoes, which write nothing.
        AppEvent::PlaybackStarted
        | AppEvent::PlaybackPaused
        | AppEvent::RenderRangeProgress { .. }
        | AppEvent::RenderRangeFinished { .. }
        | AppEvent::PlaybackStopped
        | AppEvent::PlayheadMoved(..)
        | AppEvent::LoopedBack
        | AppEvent::SourcePlaybackStarted
        | AppEvent::SourcePlaybackPaused
        | AppEvent::SourcePlayheadMoved(..)
        | AppEvent::MediaLoadFailed { .. }
        | AppEvent::PlaybackGaveUp { .. }
        | AppEvent::FrameReady
        | AppEvent::ClipAppended { .. }
        | AppEvent::ClipRemoved { .. }
        | AppEvent::ClipMoved { .. }
        | AppEvent::ClipTrimmed { .. }
        | AppEvent::ClipSelected(..)
        | AppEvent::ProjectSaved { .. }
        | AppEvent::ProjectLoaded { .. }
        | AppEvent::ProjectError { .. }
        | AppEvent::ExportProgress { .. }
        | AppEvent::ExportComplete { .. }
        | AppEvent::ExportError { .. }
        | AppEvent::SourceFrameReady
        | AppEvent::SourceMediaLoaded { .. }
        | AppEvent::ProxyGenerationStarted { .. }
        | AppEvent::ProxyGenerationFailed { .. }
        | AppEvent::ThumbnailReady { .. }
        | AppEvent::MatteMaskReady { .. }
        | AppEvent::AudioConformStarted { .. }
        | AppEvent::AudioConformFailed { .. }
        | AppEvent::TrackPeakLevels { .. }
        | AppEvent::BusPeakLevels { .. }
        | AppEvent::BusesHeldAtCap(..)
        | AppEvent::MasterPeakLevels { .. }
        | AppEvent::LufsReadings { .. }
        | AppEvent::DeliveryLoudness { .. }
        | AppEvent::EngineStats { .. }
        | AppEvent::SkimTranscodeProgress { .. }
        | AppEvent::SkimTranscodeFailed { .. }
        | AppEvent::BrowserSkimFrameReady { .. }
        | AppEvent::TimelineSkimShown { .. }
        | AppEvent::StabilizationProgress { .. }
        | AppEvent::DeflickerProgress { .. }
        | AppEvent::DeflickerFailed { .. }
        | AppEvent::StabilizationFailed { .. }
        | AppEvent::SceneDetectProgress { .. }
        | AppEvent::SceneDetectFailed { .. }
        | AppEvent::TrackingProgress { .. }
        | AppEvent::TrackingFailed { .. }
        | AppEvent::SurfaceTrackFailed { .. }
        | AppEvent::MediaSyncFailed { .. }
        | AppEvent::AudioSyncFailed { .. }
        | AppEvent::DenoiserProfileFailed { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::event_writes;
    use klipp_core::AppEvent;

    /// The events that come many times a second never close a drag: a
    /// frame, the playhead, the meters.
    #[test]
    fn frequent_events_leave_a_drag_open() {
        assert!(!event_writes(&AppEvent::FrameReady));
        assert!(!event_writes(&AppEvent::MasterPeakLevels { left: 0.1, right: 0.1 }));
        assert!(!event_writes(&AppEvent::EngineStats { dropped_frames: 0, hw_decode: None }));
    }
}
