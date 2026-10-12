//! Which commands write the document: the picture, the saved project or
//! the undo history. An open path drag (a parameter dragged, its steps
//! written live) is closed only by a command that does, so that edit is
//! its own step and never folded into the drag's.
//!
//! Every command is named, with no catch-all: a new one doesn't compile
//! until it is put in one list or the other. One not known to write
//! nothing belongs with the writers, since closing a drag only splits it
//! in two steps, while leaving it open across a writer would merge two
//! edits into one. Each was put here from its handler.

use klipp_core::AppCommand;

/// Whether `cmd` can write the picture, the saved project or the history,
/// or starts a history step of its own.
pub(crate) fn writes(cmd: &AppCommand) -> bool {
    match cmd {
        // Write none of them: transport, a selection, the viewer and its
        // scopes, a panel, a dialog, a preference, a job whose result
        // arrives later.
        AppCommand::SetSplitCompareNode { .. }
        | AppCommand::ShowPanel { .. }
        | AppCommand::ToggleGraphDock
        | AppCommand::RevealGraph { .. }
        | AppCommand::SetGraphDockRoot { .. }
        | AppCommand::Play
        | AppCommand::Pause
        | AppCommand::Stop
        | AppCommand::SeekTo(..)
        | AppCommand::SeekRelative(..)
        | AppCommand::SetPlayheadFromTimecode { .. }
        | AppCommand::RequestQualifierSample { .. }
        | AppCommand::RequestHueCurveSample { .. }
        | AppCommand::BeginTitleEdit { .. }
        | AppCommand::EndTitleEdit
        | AppCommand::RenderRange { .. }
        | AppCommand::CancelRenderRange
        | AppCommand::ClearRenderCache { .. }
        | AppCommand::ToggleLoopPlayback
        | AppCommand::SetLoopPlayback(..)
        | AppCommand::PlayInToOut
        | AppCommand::ToggleAudioSkim
        | AppCommand::SetScrubAudioEnabled(..)
        | AppCommand::SetPlayhead(..)
        | AppCommand::SetSpeed(..)
        | AppCommand::SelectClip(..)
        | AppCommand::CenterTimelineOnClip(..)
        | AppCommand::ResetEngine
        | AppCommand::SetProjectsFolder { .. }
        | AppCommand::RemoveRecentProject { .. }
        | AppCommand::OpenSequenceSettings { .. }
        | AppCommand::RegenerateFilmstripsForAll
        | AppCommand::CreateWorkspace { .. }
        | AppCommand::SequenceUpdated
        | AppCommand::SetAntiClickFade(..)
        | AppCommand::ShowExportDialog
        | AppCommand::ExportSequence { .. }
        | AppCommand::CancelExport
        | AppCommand::CancelExportJob { .. }
        | AppCommand::DeleteExportPreset { .. }
        | AppCommand::RenameExportPreset { .. }
        | AppCommand::DuplicateExportPreset { .. }
        | AppCommand::OpenClipProperties { .. }
        | AppCommand::JogFrames { .. }
        | AppCommand::ApplyBuiltinWorkspaceLayout { .. }
        | AppCommand::ApplyCustomWorkspaceLayout { .. }
        | AppCommand::DeleteWorkspaceLayout { .. }
        | AppCommand::RenameWorkspaceLayout { .. }
        | AppCommand::SetMasterVolume(..)
        | AppCommand::SetMasterMute(..)
        | AppCommand::ResetLufsMeter
        | AppCommand::SetClipVolume(..)
        | AppCommand::SetClipPan(..)
        | AppCommand::SetClipGain(..)
        | AppCommand::SetClipChannelConfig(..)
        | AppCommand::SetRubberBandParam(..)
        | AppCommand::SetVideoRubberBandParam(..)
        | AppCommand::CopyClip { .. }
        | AppCommand::CopyAttributes { .. }
        | AppCommand::SelectAllClips
        | AppCommand::SelectClipsForward
        | AppCommand::SelectClipsBackward
        | AppCommand::SelectClipsIn { .. }
        | AppCommand::SelectClipsOnSelectedTrack { .. }
        | AppCommand::SelectInverse
        | AppCommand::SelectSameSource
        | AppCommand::SelectByColour { .. }
        | AppCommand::GoToNextGap
        | AppCommand::OpenSpeedDialog { .. }
        | AppCommand::OpenEditHelper { .. }
        | AppCommand::FrameGrab
        | AppCommand::ExportSubtitles { .. }
        | AppCommand::ExportFcpxml { .. }
        | AppCommand::ExportKdenlive { .. }
        | AppCommand::ExportEdl { .. }
        | AppCommand::ExportYouTubeChapters { .. }
        | AppCommand::OpenExportDialogForSelectedClips
        | AppCommand::GenerateRenderScript { .. }
        | AppCommand::FitTimelineToWindow
        | AppCommand::SetTimelineZoom { .. }
        | AppCommand::ZoomTimelineToFit
        | AppCommand::NextEditPoint
        | AppCommand::PrevEditPoint
        | AppCommand::SeekSource(..)
        | AppCommand::MatchFrameProgramToSource
        | AppCommand::OpenModelDownloadDialog { .. }
        | AppCommand::ShowAiToolsPanel { .. }
        | AppCommand::RunAiToolsInference
        | AppCommand::CancelAiToolsInference
        | AppCommand::SetAiToolsTask(..)
        | AppCommand::SelectAiToolsModel { .. }
        | AppCommand::MatchFrameSourceToProgram
        | AppCommand::GenerateProxy { .. }
        | AppCommand::CancelProxy { .. }
        | AppCommand::SetRenderCacheEnabled(..)
        | AppCommand::CleanOrphanedCaches
        | AppCommand::ClearCacheCategory { .. }
        | AppCommand::RefreshCacheManager
        | AppCommand::OpenCacheManager
        | AppCommand::OpenAiModelsDialog
        | AppCommand::OpenAudioPluginsDialog
        | AppCommand::RescanAudioPlugins
        | AppCommand::ToggleAudioPluginDisabled { .. }
        | AppCommand::ClearAllAudioPluginsDisabled
        | AppCommand::OpenSystemProbe
        | AppCommand::OpenProjectDoctor
        | AppCommand::BeginScrub
        | AppCommand::EndScrub
        | AppCommand::TranscribeClip { .. }
        | AppCommand::ExportMetadataSheet { .. }
        | AppCommand::ShowRelinkDialog
        | AppCommand::SetNodeSolo { .. }
        | AppCommand::SetNodeViewBookmark { .. }
        | AppCommand::JumpNodeViewBookmark { .. }
        | AppCommand::CopyColorNodes { .. }
        | AppCommand::SampleNodeInput { .. }
        | AppCommand::SetScopeRenderMode { .. }
        | AppCommand::SetActiveScopeModes { .. }
        | AppCommand::SetScopeSizes { .. }
        | AppCommand::SetScopeSettings { .. }
        | AppCommand::SetScopeBloom { .. }
        | AppCommand::SetScopeLowPass { .. }
        | AppCommand::SetScopesVisible(..)
        | AppCommand::RenderScopesForFrame { .. }
        | AppCommand::RenderScopeSnapshot { .. }
        | AppCommand::SetVisibleNodeThumbs { .. }
        | AppCommand::RenderNodeThumbsFromSource { .. }
        | AppCommand::CaptureColorMatchReference
        | AppCommand::UseClipAsColorMatchReference { .. }
        | AppCommand::FocusMaskInspector
        | AppCommand::ToggleColorPanel
        | AppCommand::ShowEffectsBrowser
        | AppCommand::ToggleBeforeAfter
        | AppCommand::ToggleBrowserPanel
        | AppCommand::ToggleInspectorPanel
        | AppCommand::ToggleStabilizationCompare
        | AppCommand::SetMattePreview(..)
        | AppCommand::SetHdrMonitor(..)
        | AppCommand::SetHdrReferenceOutput(..)
        | AppCommand::SetInspectorMattePreviewTarget(..)
        | AppCommand::SetMeasureOnlyClip(..)
        | AppCommand::MediaDecodeChanged
        | AppCommand::PickWhiteBalance { .. }
        | AppCommand::PickQualifierColour { .. }
        | AppCommand::PickHueCurve { .. }
        | AppCommand::SetLutTetrahedral(..)
        | AppCommand::SetSdrTransfer(..)
        | AppCommand::SaveBuiltinEffectPreset { .. }
        | AppCommand::DeleteBuiltinEffectPreset { .. }
        | AppCommand::ShowRenderQueue
        | AppCommand::ClearCompletedJobs
        | AppCommand::SetSnapMaster(..)
        | AppCommand::SetSnapTarget { .. }
        | AppCommand::SetGridInterval(..)
        | AppCommand::SetSnapThresholdPx(..)
        | AppCommand::SetZoomStep(..)
        | AppCommand::SetMagneticTimeline(..)
        | AppCommand::SetDefaultSequenceResolutionIdx(..)
        | AppCommand::SetDefaultTransitionFrames(..)
        | AppCommand::SetShiftStepFrames(..)
        | AppCommand::SetMulticamShowAngles(..)
        | AppCommand::SetRippleEnabled(..)
        | AppCommand::SetAppendBringsAudio(..)
        | AppCommand::RefineFilmstrip { .. }
        | AppCommand::SaveTitleTemplate { .. }
        | AppCommand::SelectTitleObject { .. }
        | AppCommand::TitlePathError { .. }
        | AppCommand::SetTitleFadeDuration { .. }
        | AppCommand::ClearTitleOverlay
        | AppCommand::JumpToMarker { .. }
        | AppCommand::ZoomToSelection
        | AppCommand::SelectNearestEditPoint
        | AppCommand::CycleEditSide
        | AppCommand::BeginDynamicTrim
        | AppCommand::CancelDynamicTrim
        | AppCommand::PlayAroundEdit
        | AppCommand::SyncAutoCreateTracks(..)
        | AppCommand::CopyTrackAttributes { .. }
        | AppCommand::ResetWaveformZoom { .. }
        | AppCommand::SetAudioReference { .. }
        | AppCommand::SetAudioReferenceByMedia { .. }
        | AppCommand::OpenAutoAlignAudio { .. }
        | AppCommand::StartRecording
        | AppCommand::RevealInFileManager { .. }
        | AppCommand::OpenInExternalApp { .. }
        | AppCommand::RevealMediaInBrowser { .. }
        | AppCommand::ForceExportSequence { .. }
        | AppCommand::ForceExportSequenceOverwrite { .. }
        | AppCommand::OpenBatchRenameDialog { .. }
        | AppCommand::RegenerateFilmstrip { .. }
        | AppCommand::BrowserSkimFrame { .. }
        | AppCommand::BrowserSkimAudioStart { .. }
        | AppCommand::BrowserSkimAudioStartFromPath { .. }
        | AppCommand::BrowserSkimAudioUpdate { .. }
        | AppCommand::BrowserSkimAudioEnd
        | AppCommand::AuditionSeek { .. }
        | AppCommand::AuditionStart { .. }
        | AppCommand::AuditionEnd
        | AppCommand::AuditionSetVolume(..)
        | AppCommand::SetViewerMode(..)
        | AppCommand::SetActiveTool(..)
        | AppCommand::ToggleFullscreen
        | AppCommand::ShowPreferences
        | AppCommand::CaptureWindow { .. }
        | AppCommand::Quit
        | AppCommand::CopyAudioKeyframes { .. }
        | AppCommand::CopyVideoKeyframes { .. }
        | AppCommand::CopyKeysAt { .. }
        | AppCommand::BakeGradeToLut { .. }
        | AppCommand::SetBakeLutSize(..)
        | AppCommand::AnalyzeStabilization { .. }
        | AppCommand::CancelStabilization { .. }
        | AppCommand::AnalyzeDeflicker { .. }
        | AppCommand::CancelDeflicker { .. }
        | AppCommand::DetectScenes { .. }
        | AppCommand::CancelSceneDetect { .. }
        | AppCommand::TrackSurface { .. }
        | AppCommand::OpenSurfaceTool { .. }
        | AppCommand::SnapSurface { .. }
        | AppCommand::AnalyzeTrackPoint { .. }
        | AppCommand::CancelTrackAnalysis { .. }
        | AppCommand::ToggleTrackingMode
        | AppCommand::SetActiveGradeStill { .. }
        | AppCommand::ToggleGradeStillCompare
        | AppCommand::SetSourceMedia { .. }
        | AppCommand::PlaySource
        | AppCommand::PauseSource
        | AppCommand::StepSource(..)
        | AppCommand::StopSource
        | AppCommand::CopyAudioEffectKeyframes { .. }
        | AppCommand::OpenSaveHyperzoomPresetDialog { .. }
        | AppCommand::ExportOtio { .. } => false,
        // Write the picture, the project or the history, start a step of
        // their own, or aren't known to write nothing. The path edits are
        // among them; a path drag takes its own (`closes_path_drag`).
        AppCommand::BeginNodeLayoutEdit { .. }
        | AppCommand::EndNodeLayoutEdit { .. }
        | AppCommand::AbandonNodeLayoutEdit { .. }
        | AppCommand::SetNodePositionAt { .. }
        | AppCommand::GraphOps { .. }
        | AppCommand::AddNodeAt { .. }
        | AppCommand::RemoveNodeAt { .. }
        | AppCommand::MoveNodeAt { .. }
        | AppCommand::SetNodeEnabledAt { .. }
        | AppCommand::SetPayloadAt { .. }
        | AppCommand::SetMixAt { .. }
        | AppCommand::ConnectMaskAt { .. }
        | AppCommand::SetMaskChannelAt { .. }
        | AppCommand::SetInterpKeyAt { .. }
        | AppCommand::PasteKeysAt { .. }
        | AppCommand::ReverseKeysAt { .. }
        | AppCommand::RequestWhiteBalanceSample { .. }
        | AppCommand::LoadMediaFile { .. }
        | AppCommand::ImportMedia { .. }
        | AppCommand::ImportMediaTo { .. }
        | AppCommand::PlaceMedia { .. }
        | AppCommand::ImportMediaFolder { .. }
        | AppCommand::SetTrackColourLabel { .. }
        | AppCommand::MoveTrack { .. }
        | AppCommand::SplitToMono { .. }
        | AppCommand::SetTrackChannelWaveforms { .. }
        | AppCommand::AppendClip { .. }
        | AppCommand::AppendClipToTrack { .. }
        | AppCommand::AppendMediaBatchToTrack { .. }
        | AppCommand::InsertAudioMediaClip { .. }
        | AppCommand::RemoveClip { .. }
        | AppCommand::AttachLinkedAudio { .. }
        | AppCommand::MoveClip { .. }
        | AppCommand::MoveClipToTrack { .. }
        | AppCommand::SwapAdjacentClips { .. }
        | AppCommand::TrimClip { .. }
        | AppCommand::RollTrimClip { .. }
        | AppCommand::NewProject
        | AppCommand::CloseProject
        | AppCommand::SaveProject
        | AppCommand::SaveProjectAs { .. }
        | AppCommand::OpenProject { .. }
        | AppCommand::OpenProjectNow { .. }
        | AppCommand::CreateProject { .. }
        | AppCommand::DeleteProject { .. }
        | AppCommand::RenameProjectOnDisk { .. }
        | AppCommand::SetProjectThumbnailFromViewer
        | AppCommand::SetProjectThumbnailFromFile { .. }
        | AppCommand::AddSequence { .. }
        | AppCommand::AddSequenceAndOpen { .. }
        | AppCommand::DuplicateSequence { .. }
        | AppCommand::SetActiveSequence { .. }
        | AppCommand::RenameSequence { .. }
        | AppCommand::DeleteSequence { .. }
        | AppCommand::ReorderSequences { .. }
        | AppCommand::SetSequenceResolution { .. }
        | AppCommand::SetSequenceFps { .. }
        | AppCommand::RegenerateAudioCacheForAll
        | AppCommand::RegenerateSkimTranscodesForAll
        | AppCommand::Undo
        | AppCommand::SquashUndo { .. }
        | AppCommand::Redo
        | AppCommand::BeginHistoryBatch
        | AppCommand::EndHistoryBatch
        | AppCommand::SetTrackOpacity { .. }
        | AppCommand::SetTrackVisibility { .. }
        | AppCommand::SetTrackVolume { .. }
        | AppCommand::SetTrackMute { .. }
        | AppCommand::SetTrackSolo { .. }
        | AppCommand::SetTrackPan { .. }
        | AppCommand::BladeClip { .. }
        | AppCommand::CutClip { .. }
        | AppCommand::PasteClip
        | AppCommand::PasteClipInsert
        | AppCommand::DuplicateClip { .. }
        | AppCommand::PasteAttributes { .. }
        | AppCommand::SetClipEnabled { .. }
        | AppCommand::SetClipSolo { .. }
        | AppCommand::CloseAllGaps { .. }
        | AppCommand::SetClipKeepPitch { .. }
        | AppCommand::SubclipFromClip { .. }
        | AppCommand::RunEditHelper
        | AppCommand::SyncSelectedClips { .. }
        | AppCommand::ExtractFrameToBin
        | AppCommand::ImportSubtitles { .. }
        | AppCommand::ImportKdenlive { .. }
        | AppCommand::ImportOtio { .. }
        | AppCommand::ImportEdl { .. }
        | AppCommand::ImportFcpxml { .. }
        | AppCommand::ConsolidateProject { .. }
        | AppCommand::AddSubtitleAtPlayhead { .. }
        | AppCommand::SaveCaptionStyle { .. }
        | AppCommand::ClearCaptionStyle { .. }
        | AppCommand::ReplaceClipMedia { .. }
        | AppCommand::CreateCompoundClip { .. }
        | AppCommand::CreateMulticamClip
        | AppCommand::CreateSyncedMulticam { .. }
        | AppCommand::SwitchMulticamAngle { .. }
        | AppCommand::SwitchMulticamAngleAt { .. }
        | AppCommand::SetMulticamSound { .. }
        | AppCommand::SetPanLaw(..)
        | AppCommand::ApplyMotionPreset { .. }
        | AppCommand::CreateAdjustmentLayer
        | AppCommand::CreateGeneratorClip { .. }
        | AppCommand::UpdateGeneratorData { .. }
        | AppCommand::InsertFreezeFrame { .. }
        | AppCommand::OpenCompoundClip { .. }
        | AppCommand::CloseCompoundClip
        | AppCommand::DecomposeCompoundClip { .. }
        | AppCommand::CloseGap { .. }
        | AppCommand::LoadSourceMedia { .. }
        | AppCommand::SetSourceInPoint(..)
        | AppCommand::SetSourceOutPoint(..)
        | AppCommand::InsertSourceAtPlayhead
        | AppCommand::CreateSubclipFromZone { .. }
        | AppCommand::SetSequenceInPoint(..)
        | AppCommand::SetSequenceOutPoint(..)
        | AppCommand::ClearSequenceInPoint
        | AppCommand::ClearSequenceOutPoint
        | AppCommand::SetUseProxies(..)
        | AppCommand::RelinkMedia { .. }
        | AppCommand::RelinkMediaByHand { .. }
        | AppCommand::RefreshMediaMetadata { .. }
        | AppCommand::ImportMetadataSheet { .. }
        | AppCommand::RelinkAllFromDirectory { .. }
        | AppCommand::AddMediaTag { .. }
        | AppCommand::RemoveMediaTag { .. }
        | AppCommand::AddNode { .. }
        | AppCommand::RemoveNode { .. }
        | AppCommand::GroupIntoCompound { .. }
        | AppCommand::DescendIntoCompound { .. }
        | AppCommand::UngroupCompound { .. }
        | AppCommand::UpdateActiveGradeVersion { .. }
        | AppCommand::ConnectNodes { .. }
        | AppCommand::DisconnectEdge { .. }
        | AppCommand::SetNodeOutputLink { .. }
        | AppCommand::SetNodeInputLink { .. }
        | AppCommand::MoveNode { .. }
        | AppCommand::ToggleNodeEnabled { .. }
        | AppCommand::AutoLayoutNodeGraph { .. }
        | AppCommand::CreateNodeFrame { .. }
        | AppCommand::RenameNodeFrame { .. }
        | AppCommand::SetNodeFrameDescription { .. }
        | AppCommand::CreateStickyNote { .. }
        | AppCommand::SetStickyNoteText { .. }
        | AppCommand::MoveStickyNote { .. }
        | AppCommand::DeleteStickyNote { .. }
        | AppCommand::RecolorNodeFrame { .. }
        | AppCommand::DeleteNodeFrame { .. }
        | AppCommand::AddToNodeFrame { .. }
        | AppCommand::RemoveFromNodeFrame { .. }
        | AppCommand::PasteColorNodes { .. }
        | AppCommand::RenameNode { .. }
        | AppCommand::SetNodeLut { .. }
        | AppCommand::CorrectTranscriptWord { .. }
        | AppCommand::RetranscribeClip { .. }
        | AppCommand::SetTranscriptLanguage { .. }
        | AppCommand::SetNodeParam { .. }
        | AppCommand::PrinterLight { .. }
        | AppCommand::BeginNodeParamEdit { .. }
        | AppCommand::EndNodeParamEdit { .. }
        | AppCommand::AbandonNodeParamEdit { .. }
        | AppCommand::SetNodeCurves { .. }
        | AppCommand::SetNodeHueSatCurves { .. }
        | AppCommand::SetNodeQualifier { .. }
        | AppCommand::SetNodeLumaKey { .. }
        | AppCommand::SetNodeDifferenceKey { .. }
        | AppCommand::SetNodeDifferenceKeyPlate { .. }
        | AppCommand::SetNodeSoftClip { .. }
        | AppCommand::SetNodeFilmGrain { .. }
        | AppCommand::SetNodeMasks { .. }
        | AppCommand::SetNodeColorMatch { .. }
        | AppCommand::SetExternalMattePath { .. }
        | AppCommand::SetExternalMatteChannel { .. }
        | AppCommand::SetMatteCombineOp { .. }
        | AppCommand::SetNodeLayerMixerInput { .. }
        | AppCommand::MatchClipToColorReference { .. }
        | AppCommand::AddMaskShape { .. }
        | AppCommand::RemoveMaskShape { .. }
        | AppCommand::ApplyColorMatch { .. }
        | AppCommand::AddMaskKeyframe { .. }
        | AppCommand::RemoveMaskKeyframe { .. }
        | AppCommand::ClearMaskKeyframes { .. }
        | AppCommand::AddKeyframe { .. }
        | AppCommand::RemoveKeyframe { .. }
        | AppCommand::ClearKeyframes { .. }
        | AppCommand::SetKeyframeInterpolation { .. }
        | AppCommand::SetKeyframeTangents { .. }
        | AppCommand::RefreshGrade
        | AppCommand::SetClipLogFormat { .. }
        | AppCommand::AutoWhiteBalance { .. }
        | AppCommand::LoadGlobalLut { .. }
        | AppCommand::ClearGlobalLut
        | AppCommand::SetGlobalLutOpacity(..)
        | AppCommand::SetGlobalLutEnabled(..)
        | AppCommand::AddEffect { .. }
        | AppCommand::RemoveEffect { .. }
        | AppCommand::SetEffectEnabled { .. }
        | AppCommand::ReorderEffect { .. }
        | AppCommand::SetEffectParam { .. }
        | AppCommand::AddBuiltinEffect { .. }
        | AppCommand::RemoveBuiltinEffect { .. }
        | AppCommand::ToggleBuiltinEffectEnabled { .. }
        | AppCommand::UpdateBuiltinEffect { .. }
        | AppCommand::ReorderBuiltinEffect { .. }
        | AppCommand::SlipClip { .. }
        | AppCommand::SlideClip { .. }
        | AppCommand::InsertEdit
        | AppCommand::OverwriteEdit
        | AppCommand::FitToFillEdit
        | AppCommand::PlaceOnTopEdit
        | AppCommand::ReplaceEdit
        | AppCommand::RippleOverwriteEdit
        | AppCommand::LiftEdit
        | AppCommand::ExtractEdit
        | AppCommand::AddTransition { .. }
        | AppCommand::RemoveTransition { .. }
        | AppCommand::SetTransitionDuration { .. }
        | AppCommand::AddDefaultTransition { .. }
        | AppCommand::SetTransitionKind { .. }
        | AppCommand::SetTransitionParam { .. }
        | AppCommand::SetTransitionAlignment { .. }
        | AppCommand::CreateTitleClip { .. }
        | AppCommand::AddTitleAtPlayhead { .. }
        | AppCommand::InsertCompositionTemplate { .. }
        | AppCommand::SetTemplateOverride { .. }
        | AppCommand::SaveCompositionTemplate { .. }
        | AppCommand::AddTitleImageObject { .. }
        | AppCommand::SetTitleImagePath { .. }
        | AppCommand::SetTitleImageFit { .. }
        | AppCommand::CreateTitleAsset { .. }
        | AppCommand::DetachTitleAsset { .. }
        | AppCommand::RenameTitleAsset { .. }
        | AppCommand::UpdateTitleData { .. }
        | AppCommand::RevertTitleToStyle { .. }
        | AppCommand::SetTitleAlignment { .. }
        | AppCommand::SetTitlePosition { .. }
        | AppCommand::SetTitleObjectTransform { .. }
        | AppCommand::AddTitleObject { .. }
        | AppCommand::DeleteTitleObject { .. }
        | AppCommand::SetTitleObjectVisible { .. }
        | AppCommand::SetTitleObjectLocked { .. }
        | AppCommand::SetTitleObjectOpacity { .. }
        | AppCommand::AddTitleObjectOpacityKeyframe { .. }
        | AppCommand::RemoveTitleObjectOpacityKeyframe { .. }
        | AppCommand::RenameTitleObject { .. }
        | AppCommand::ReorderTitleObjects { .. }
        | AppCommand::BeginTitlePathDrag { .. }
        | AppCommand::EndTitlePathDrag { .. }
        | AppCommand::SetTitleVectorPath { .. }
        | AppCommand::SetTitleShapeFill { .. }
        | AppCommand::SetTitleShapeStroke { .. }
        | AppCommand::SetTitleShapeCornerRadius { .. }
        | AppCommand::SetTitleShapeFit { .. }
        | AppCommand::SetTitleObjectText { .. }
        | AppCommand::SetTitleObjectTransformAndFont { .. }
        | AppCommand::SetTitleBackground { .. }
        | AppCommand::SetTitleShadow { .. }
        | AppCommand::SetTitleShadows { .. }
        | AppCommand::SetTitleShadowLayer { .. }
        | AppCommand::SetTitleShadowStackEnabled { .. }
        | AppCommand::SetTitleOutline { .. }
        | AppCommand::SetTitleGlow { .. }
        | AppCommand::SetTitleGlows { .. }
        | AppCommand::SetTitleGlowLayer { .. }
        | AppCommand::SetTitleGlowStackEnabled { .. }
        | AppCommand::SetTitleGradient { .. }
        | AppCommand::SetTitleOutlines { .. }
        | AppCommand::SetTitleOutlineLayer { .. }
        | AppCommand::SetTitleOutlineStackEnabled { .. }
        | AppCommand::SetTitleInnerShadow { .. }
        | AppCommand::SetTitleInnerShadows { .. }
        | AppCommand::SetTitleInnerShadowLayer { .. }
        | AppCommand::SetTitleInnerShadowStackEnabled { .. }
        | AppCommand::SetTitlePattern { .. }
        | AppCommand::SetTitleTracking { .. }
        | AppCommand::SetTitleMotion { .. }
        | AppCommand::SetTitleWriteOn { .. }
        | AppCommand::SetTitleLineSpacing { .. }
        | AppCommand::SetTitleUnderline { .. }
        | AppCommand::SetTitleStrikethrough { .. }
        | AppCommand::SetTitlePatternEnabled { .. }
        | AppCommand::ApplyTitlePreset { .. }
        | AppCommand::SetTitleTextureEnabled { .. }
        | AppCommand::SetTitleCaseTransform { .. }
        | AppCommand::SetTitleBaselineShift { .. }
        | AppCommand::SetTitleDropCap { .. }
        | AppCommand::SetTitleChromaticAberration { .. }
        | AppCommand::SetTitleGlitch { .. }
        | AppCommand::SetTitleNeonPreset { .. }
        | AppCommand::SetTitleColumns { .. }
        | AppCommand::SetTitleVerticalLayout { .. }
        | AppCommand::SetTitleDirection { .. }
        | AppCommand::SetTitleExtrusion { .. }
        | AppCommand::SetTitleTextPath { .. }
        | AppCommand::SetTitleTextureFill { .. }
        | AppCommand::SetTitleTexturePreset { .. }
        | AppCommand::SetTitleTextureParams { .. }
        | AppCommand::SetTitleMotionBlur { .. }
        | AppCommand::ApplyTitleStyle { .. }
        | AppCommand::ClearTitleStyleRuns { .. }
        | AppCommand::SetTitleLoops { .. }
        | AppCommand::SetTitleEdgeFade { .. }
        | AppCommand::ImportMediaLinked { .. }
        | AppCommand::UnlinkClip { .. }
        | AppCommand::LinkClips { .. }
        | AppCommand::AddMarker { .. }
        | AppCommand::AddMarkerWith { .. }
        | AppCommand::RemoveMarker { .. }
        | AppCommand::UpdateMarkerLabel { .. }
        | AppCommand::UpdateMarkerColor { .. }
        | AppCommand::UpdateMarkerComment { .. }
        | AppCommand::UpdateMarkerKind { .. }
        | AppCommand::SetMarkerDuration { .. }
        | AppCommand::MoveMarker { .. }
        | AppCommand::ConnectToPrimary { .. }
        | AppCommand::DisconnectFromPrimary { .. }
        | AppCommand::ToggleStoryline { .. }
        | AppCommand::TrimEditPoints { .. }
        | AppCommand::ExtendEditPoints
        | AppCommand::CommitDynamicTrim
        | AppCommand::NudgeClips { .. }
        | AppCommand::MoveClipsToTrack { .. }
        | AppCommand::AddClipMarker { .. }
        | AppCommand::RemoveClipMarker { .. }
        | AppCommand::AddVideoTrack
        | AppCommand::AddAudioTrack
        | AppCommand::AddSubtitleTrack { .. }
        | AppCommand::AddTrackAbove { .. }
        | AppCommand::AddTrackBelow { .. }
        | AppCommand::RemoveTrack { .. }
        | AppCommand::RenameTrack { .. }
        | AppCommand::SetTrackSyncLock { .. }
        | AppCommand::SetTrackTargeted { .. }
        | AppCommand::SetTrackSourcePatch { .. }
        | AppCommand::PasteTrackAttributes { .. }
        | AppCommand::AddTrackVideoEffect { .. }
        | AppCommand::AddTrackAudioEffect { .. }
        | AppCommand::ToggleTrackEffect { .. }
        | AppCommand::RemoveTrackEffect { .. }
        | AppCommand::ClearTrackEffects { .. }
        | AppCommand::AlignToAudioReference { .. }
        | AppCommand::AlignToAudioReferenceByMedia { .. }
        | AppCommand::AutoAlignAudio { .. }
        | AppCommand::ArmTrack { .. }
        | AppCommand::ToggleAudioAutomationLane { .. }
        | AppCommand::SetAudioAutomationLaneParam { .. }
        | AppCommand::StopRecording
        | AppCommand::RemoveFromPool { .. }
        | AppCommand::ConfirmRemoveFromPool { .. }
        | AppCommand::SetMediaRating { .. }
        | AppCommand::SetMediaLog { .. }
        | AppCommand::AddMediaMarker { .. }
        | AppCommand::RemoveMediaMarker { .. }
        | AppCommand::RenameMediaMarker { .. }
        | AppCommand::SyncMediaPair { .. }
        | AppCommand::ClearSyncedAudio { .. }
        | AppCommand::SetMediaTags { .. }
        | AppCommand::SetMediaColorLabel { .. }
        | AppCommand::DeleteUnusedMedia
        | AppCommand::ConfirmDeleteUnusedMedia
        | AppCommand::CreateBin { .. }
        | AppCommand::SaveSmartBin { .. }
        | AppCommand::DeleteSmartBin { .. }
        | AppCommand::CreateSubBin { .. }
        | AppCommand::ImportImageSequence { .. }
        | AppCommand::DeleteBin { .. }
        | AppCommand::MoveItemToBin { .. }
        | AppCommand::RenameBin { .. }
        | AppCommand::RenameMediaItem { .. }
        | AppCommand::BatchRenameMediaItems { .. }
        | AppCommand::StartSkimTranscode { .. }
        | AppCommand::RegenerateSkimTranscode { .. }
        | AppCommand::SetTrackLocked { .. }
        | AppCommand::SetTrackCustomHeight { .. }
        | AppCommand::SetTrackAutomationLaneHeight { .. }
        | AppCommand::GroupClips { .. }
        | AppCommand::UngroupClips { .. }
        | AppCommand::UngroupClip { .. }
        | AppCommand::AddModifier { .. }
        | AppCommand::RemoveModifier { .. }
        | AppCommand::UpdateModifier { .. }
        | AppCommand::ToggleModifier { .. }
        | AppCommand::EditAudioKeyframeAtFrame { .. }
        | AppCommand::AddAudioKeyframe { .. }
        | AppCommand::RemoveAudioKeyframe { .. }
        | AppCommand::MoveAudioKeyframe { .. }
        | AppCommand::SetAudioKeyframeInterpolation { .. }
        | AppCommand::SetAudioKeyframeTangent { .. }
        | AppCommand::AddVideoKeyframe { .. }
        | AppCommand::RemoveVideoKeyframe { .. }
        | AppCommand::MoveVideoKeyframe { .. }
        | AppCommand::SetVideoKeyframeInterpolation { .. }
        | AppCommand::SetVideoKeyframeTangent { .. }
        | AppCommand::AddTitleObjectEffectKeyframe { .. }
        | AppCommand::RemoveTitleObjectEffectKeyframe { .. }
        | AppCommand::MoveTitleObjectEffectKeyframe { .. }
        | AppCommand::SetTitleObjectEffectKeyframeTangent { .. }
        | AppCommand::SetTitleObjectEffectKeyframeInterpolation { .. }
        | AppCommand::AddTitleObjectPositionKeyframe { .. }
        | AppCommand::AddTitleObjectTransformKeyframe { .. }
        | AppCommand::RemoveTitleObjectTransformKeyframe { .. }
        | AppCommand::RemoveTitleObjectPositionKeyframe { .. }
        | AppCommand::MoveTitleObjectPositionKeyframe { .. }
        | AppCommand::SetTitleObjectPositionKeyframeTangent { .. }
        | AppCommand::SetTitleObjectPositionKeyframeInterpolation { .. }
        | AppCommand::PasteAudioKeyframes { .. }
        | AppCommand::PasteVideoKeyframes { .. }
        | AppCommand::ReverseAudioKeyframes { .. }
        | AppCommand::ReverseVideoKeyframes { .. }
        | AppCommand::AddEffectKeyframe { .. }
        | AppCommand::RemoveEffectKeyframe { .. }
        | AppCommand::MoveEffectKeyframe { .. }
        | AppCommand::SetEffectKeyframeInterpolation { .. }
        | AppCommand::SetClipRole { .. }
        | AppCommand::SetClipDepth { .. }
        | AppCommand::RenameClips { .. }
        | AppCommand::SetClipColourLabels { .. }
        | AppCommand::CreateGradeGroup { .. }
        | AppCommand::SetClipsGradeGroup { .. }
        | AppCommand::RenameGradeGroup { .. }
        | AppCommand::DeleteGradeGroup { .. }
        | AppCommand::SetSequenceShutter { .. }
        | AppCommand::SetSequenceCameraKey { .. }
        | AppCommand::RemoveSequenceCameraKey { .. }
        | AppCommand::MoveSequenceCameraKey { .. }
        | AppCommand::SetSequenceCameraKeyInterpolation { .. }
        | AppCommand::SetSequenceCameraKeyTangent { .. }
        | AppCommand::EaseSequenceCameraKey { .. }
        | AppCommand::BeginSequenceCameraDrag { .. }
        | AppCommand::EndSequenceCameraDrag { .. }
        | AppCommand::SetClipSpeed { .. }
        | AppCommand::AddSpeedKeyframe { .. }
        | AppCommand::RemoveSpeedKeyframe { .. }
        | AppCommand::UpdateSpeedKeyframe { .. }
        | AppCommand::SetSpeedKeyframes { .. }
        | AppCommand::BeginSpeedKeyframeDrag { .. }
        | AppCommand::EndSpeedKeyframeDrag { .. }
        | AppCommand::SetSourceRotation { .. }
        | AppCommand::UpdateClipAudio { .. }
        | AppCommand::BeginAudioDrag { .. }
        | AppCommand::EndAudioDrag { .. }
        | AppCommand::ScaleAudioKeyframeTiming { .. }
        | AppCommand::ScaleVideoKeyframeTiming { .. }
        | AppCommand::InsertAudioKeyframeBump { .. }
        | AppCommand::InsertVideoKeyframeBump { .. }
        | AppCommand::BeginKeyframeDrag { .. }
        | AppCommand::EndKeyframeDrag { .. }
        | AppCommand::ApplyKenBurns { .. }
        | AppCommand::ClearTransformKeyframes { .. }
        | AppCommand::ClearVideoParamKeyframes { .. }
        | AppCommand::AutoDuckAudio { .. }
        | AppCommand::RippleTrimHead { .. }
        | AppCommand::RippleTrimTail { .. }
        | AppCommand::InsertGapAtPlayhead
        | AppCommand::InsertGapOnTrack { .. }
        | AppCommand::TrimOutToPlayhead
        | AppCommand::TrimInToPlayhead
        | AppCommand::AddAudioEffect { .. }
        | AppCommand::RemoveAudioEffect { .. }
        | AppCommand::UpdateAudioEffectParam { .. }
        | AppCommand::ToggleAudioEffect { .. }
        | AppCommand::SetAudioEffectWetDry { .. }
        | AppCommand::ReorderAudioEffect { .. }
        | AppCommand::LearnDenoiserProfile { .. }
        | AppCommand::SetAudioEffectParams { .. }
        | AppCommand::BeginEffectDrag { .. }
        | AppCommand::EndEffectDrag { .. }
        | AppCommand::AbandonEffectDrag { .. }
        | AppCommand::DetectSceneCuts { .. }
        | AppCommand::JoinThroughEdit { .. }
        | AppCommand::SplitAtSceneMarkers { .. }
        | AppCommand::AddTrackPoint { .. }
        | AppCommand::TrackMaskRegion { .. }
        | AppCommand::RemoveSurface { .. }
        | AppCommand::RemoveMaskTracking { .. }
        | AppCommand::RemoveTrackPoint { .. }
        | AppCommand::GenerateKeyframesFromTrack { .. }
        | AppCommand::SaveGradeVersion { .. }
        | AppCommand::SwitchGradeVersion { .. }
        | AppCommand::DeleteGradeVersion { .. }
        | AppCommand::RenameGradeVersion { .. }
        | AppCommand::SaveLook { .. }
        | AppCommand::ApplyLook { .. }
        | AppCommand::DeleteLook { .. }
        | AppCommand::RenameLook { .. }
        | AppCommand::CaptureGradeStill { .. }
        | AppCommand::DeleteGradeStill { .. }
        | AppCommand::ApplyGradeFromStill { .. }
        | AppCommand::AddAudioEffectKeyframe { .. }
        | AppCommand::RemoveAudioEffectKeyframe { .. }
        | AppCommand::MoveAudioEffectKeyframe { .. }
        | AppCommand::SetAudioEffectKeyframeInterpolation { .. }
        | AppCommand::SetAudioEffectKeyframeTangent { .. }
        | AppCommand::PasteAudioEffectKeyframes { .. }
        | AppCommand::ReverseAudioEffectKeyframes { .. }
        | AppCommand::ScaleAudioEffectKeyframeTiming { .. }
        | AppCommand::InsertAudioEffectKeyframeBump { .. }
        | AppCommand::BeginAudioChainDrag { .. }
        | AppCommand::EndAudioChainDrag { .. }
        | AppCommand::SetClipBlendMode { .. }
        | AppCommand::SetClipMatte { .. }
        | AppCommand::ClearClipMatte { .. }
        | AppCommand::SetClipTransformLink { .. }
        | AppCommand::SetClipTransformLinkTracker { .. }
        | AppCommand::ClearClipTransformLink { .. }
        | AppCommand::ApplyHyperzoom { .. }
        | AppCommand::AutoLinkClipsBeneathSource { .. }
        | AppCommand::SetClipConsumedAsMatte { .. }
        | AppCommand::SetClipStencil { .. }
        | AppCommand::ClearClipStencil { .. }
        | AppCommand::UseClipAboveAsMatte { .. }
        | AppCommand::ToggleClipStencil { .. }
        | AppCommand::ImportMediaWithAnalysis { .. }
        | AppCommand::BeginParamEditAt { .. }
        | AppCommand::EndParamEditAt { .. }
        | AppCommand::AbandonParamEditAt { .. }
        | AppCommand::SetParamAt { .. }
        | AppCommand::MoveKeyAt { .. }
        | AppCommand::AddKeyAt { .. }
        | AppCommand::RemoveKeyAt { .. } => true,
    }
}
