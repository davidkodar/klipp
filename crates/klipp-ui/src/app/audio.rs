//! A clip's sound: its levels and fades, its volume and pan curves, ducking,
//! its audio effect chain and the denoiser's learned profile (#826).

use super::*;

impl KlippApp {
    fn auto_duck_audio(
        &mut self,
        target_clip_id: &ClipId,
        sidechain_track_idx: &usize,
        duck_level: &f32,
        attack_secs: &f32,
        release_secs: &f32,
        threshold: &f32,
    ) {
        // 1. Capture old keyframe state for undo
        let mut old_state = None;
        let mut target_timeline_start = 0.0_f64;
        let mut target_timeline_end = 0.0_f64;
        let mut target_clip_dur_secs = 0.0_f64;
        {
            let seq = &self.state.sequences[self.state.active_sequence_idx];
            if let Some(c) = seq.all_clips().find(|c| c.id == *target_clip_id) {
                old_state = Some(KeyframeSnapshot::from_clip(c));
                target_timeline_start = c.timeline_in.to_secs_f64();
                target_timeline_end = c.timeline_out.to_secs_f64();
                target_clip_dur_secs = target_timeline_end - target_timeline_start;
            }
        }
        if old_state.is_none() || target_clip_dur_secs <= 0.0 {
            return;
        }

        // 2. Detect active regions on the sidechain track using PCM data or clip boundaries
        let active_regions: Vec<(f64, f64)> = {
            let seq = &self.state.sequences[self.state.active_sequence_idx];
            let sidechain_track_idx = *sidechain_track_idx;
            if sidechain_track_idx >= seq.tracks.len() {
                return;
            }
            let track = &seq.tracks[sidechain_track_idx];

            // Try PCM-based analysis first
            let mut pcm_regions: Vec<(f64, f64)> = Vec::new();
            let mut used_pcm = false;

            for sc_clip in &track.clips {
                let sc_start = sc_clip.timeline_in.to_secs_f64();
                let sc_end = sc_clip.timeline_out.to_secs_f64();
                // Skip clips that don't overlap the target
                if sc_end <= target_timeline_start || sc_start >= target_timeline_end {
                    return;
                }

                // Try to read PCM data for this clip
                let pcm_data = sc_clip.media_id()
                    .and_then(|mid| self.state.media_pool.get(&mid))
                    .and_then(|item| item.audio_cache.as_ref())
                    .and_then(|cache| {
                        std::fs::read(&cache.pcm_path).ok().map(|bytes| (bytes, cache.sample_rate, cache.channels))
                    });

                if let Some((pcm_bytes, sample_rate, channels)) = pcm_data {
                    used_pcm = true;
                    // Reinterpret the cache bytes as f32 samples. Using
                    // bytemuck rather than slice::from_raw_parts: the
                    // Vec<u8> from fs::read is only byte-aligned, and a
                    // raw cast would produce an unaligned &[f32] — UB on
                    // any target that enforces alignment on f32 loads.
                    // bytemuck::try_cast_slice checks alignment and
                    // length at runtime; on a misaligned cache we fall
                    // back to a chunk-decode copy so the analysis still
                    // runs instead of silently corrupting its input.
                    let owned_samples: Vec<f32>;
                    let samples: &[f32] = match bytemuck::try_cast_slice::<u8, f32>(&pcm_bytes) {
                        Ok(s) => s,
                        Err(_) => {
                            owned_samples = pcm_bytes
                                .chunks_exact(std::mem::size_of::<f32>())
                                .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                                .collect();
                            &owned_samples
                        }
                    };
                    let window_samples = (sample_rate as f64 * 0.05) as usize * channels as usize; // 50ms windows
                    if window_samples == 0 { continue; }

                    let src_in_secs = sc_clip.source_in.to_secs_f64();
                    let total_mono_samples = samples.len() / channels as usize;
                    let total_dur = total_mono_samples as f64 / sample_rate as f64;

                    // Analyze overlapping region
                    let analyze_start = target_timeline_start.max(sc_start);
                    let analyze_end = target_timeline_end.min(sc_end);

                    let window_secs = 0.05_f64;
                    let mut t = analyze_start;
                    let mut region_start: Option<f64> = None;

                    while t < analyze_end {
                        // Map timeline time to source time
                        let source_t = src_in_secs + (t - sc_start);
                        if source_t < 0.0 || source_t >= total_dur {
                            t += window_secs;
                            continue;
                        }
                        let sample_idx = (source_t * sample_rate as f64) as usize * channels as usize;
                        let end_idx = ((sample_idx + window_samples).min(samples.len())).max(sample_idx);

                        // Compute RMS
                        let window = &samples[sample_idx..end_idx];
                        let rms = if window.is_empty() {
                            0.0
                        } else {
                            let sum: f64 = window.iter().map(|&s| (s as f64) * (s as f64)).sum();
                            (sum / window.len() as f64).sqrt() as f32
                        };

                        if rms > *threshold {
                            if region_start.is_none() {
                                region_start = Some(t);
                            }
                        } else if let Some(rs) = region_start.take() {
                            pcm_regions.push((rs, t));
                        }
                        t += window_secs;
                    }
                    // Close any open region
                    if let Some(rs) = region_start {
                        pcm_regions.push((rs, analyze_end));
                    }
                } else {
                    // Fallback: use clip boundaries as active region
                    let region_start = sc_start.max(target_timeline_start);
                    let region_end = sc_end.min(target_timeline_end);
                    if region_end > region_start {
                        pcm_regions.push((region_start, region_end));
                    }
                }
            }

            // Merge adjacent/overlapping regions (with a small gap tolerance)
            if used_pcm && !pcm_regions.is_empty() {
                pcm_regions.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                let mut merged: Vec<(f64, f64)> = Vec::new();
                for r in pcm_regions {
                    if let Some(last) = merged.last_mut() {
                        if r.0 <= last.1 + 0.05 { // 50ms merge gap
                            last.1 = last.1.max(r.1);
                            return;
                        }
                    }
                    merged.push(r);
                }
                merged
            } else {
                pcm_regions
            }
        };

        if active_regions.is_empty() {
            return;
        }

        // 3. Generate volume keyframes on the target clip
        let attack = *attack_secs as f64;
        let release = *release_secs as f64;
        let duck = *duck_level;
        let mut keyframes: Vec<klipp_core::Keyframe> = Vec::new();

        for &(region_start, region_end) in &active_regions {
            // Convert to clip-local time (relative to target clip's timeline_in)
            let ramp_down_start = (region_start - target_timeline_start - attack).max(0.0);
            let duck_start = (region_start - target_timeline_start).max(0.0);
            let duck_end = (region_end - target_timeline_start).min(target_clip_dur_secs);
            let ramp_up_end = (region_end - target_timeline_start + release).min(target_clip_dur_secs);

            // Before duck: normal volume
            if ramp_down_start < duck_start {
                keyframes.push(klipp_core::Keyframe {
                    time_secs: ramp_down_start,
                    value: 1.0,
                    interpolation: klipp_core::Interpolation::Bezier,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (attack / 3.0, 0.0),
                });
            }

            // Duck start
            keyframes.push(klipp_core::Keyframe {
                time_secs: duck_start,
                value: duck,
                interpolation: klipp_core::Interpolation::Bezier,
                tangent_in: (-attack / 3.0, 0.0),
                tangent_out: (0.0, 0.0),
            });

            // Duck end
            keyframes.push(klipp_core::Keyframe {
                time_secs: duck_end,
                value: duck,
                interpolation: klipp_core::Interpolation::Bezier,
                tangent_in: (0.0, 0.0),
                tangent_out: (release / 3.0, 0.0),
            });

            // After duck: normal volume
            if ramp_up_end > duck_end {
                keyframes.push(klipp_core::Keyframe {
                    time_secs: ramp_up_end,
                    value: 1.0,
                    interpolation: klipp_core::Interpolation::Bezier,
                    tangent_in: (-release / 3.0, 0.0),
                    tangent_out: (0.0, 0.0),
                });
            }
        }

        // Remove duplicate times (keep the last keyframe at each time)
        keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
        keyframes.dedup_by(|a, b| (a.time_secs - b.time_secs).abs() < klipp_core::keyframe::KEYFRAME_TIME_EPSILON);

        // 4. Apply keyframes to the target clip
        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
            if let Some(clip) = track.find_clip_mut(*target_clip_id) {
                clip.volume_keyframes = keyframes;
                break;
            }
        }

        // 5. Push undo history
        if let Some(old_snap) = old_state {
            let seq = &self.state.sequences[self.state.active_sequence_idx];
            let new_state = seq.all_clips().find(|c| c.id == *target_clip_id).map(KeyframeSnapshot::from_clip);
            if let Some(new_snap) = new_state {
                self.state.push_history(EditCommand::EditKeyframes {
                        clip_id: *target_clip_id,
                        linked_clip_id: None,
                        description: "Auto Duck Audio",
                        old_state: old_snap,
                        new_state: new_snap,
                        old_linked_state: None,
                        new_linked_state: None,
                    });
            }
        }
        self.sync_sequence_to_engine();
        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
        self.state.dirty = true;
    }

    fn handle_audio_keyframe_curves(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        let mut resolved;
        let (cmd, frame_time) = if let AppCommand::EditAudioKeyframeAtFrame { clip_id, param, time_secs, value } = cmd {
            resolved = match value {
                Some(value) => AppCommand::AddAudioKeyframe { clip_id: *clip_id, param: *param, time_secs: *time_secs, value: *value },
                None => AppCommand::RemoveAudioKeyframe { clip_id: *clip_id, param: *param, time_secs: *time_secs },
            };
            crate::inspector::keyframe_selection::resolve_command(self.state.sequence(), &mut resolved);
            (&resolved, Some(*time_secs))
        } else { (cmd, None) };
        let tolerance = if frame_time.is_some() { crate::inspector::keyframe_selection::frame_tolerance(self.state.sequence()) } else { 0.0 };
        match cmd {
                AppCommand::AddAudioKeyframe { clip_id, param, time_secs, value } => {
                    let mut linked_partner_id = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    // Capture old state for linked partner before main clip mutation
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            klipp_core::keyframe::upsert_keyframe(keyframes, *time_secs, *value);
                            break;
                        }
                    }
                    // Also add to linked partner clip
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut partner.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut partner.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut partner.gain_keyframes,
                                };
                                edit_linked_audio_key(keyframes, frame_time.unwrap_or(*time_secs), tolerance, *time_secs, Some(*value));
                                break;
                            }
                        }
                    }
                    // Capture new state and push history
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.push_history(EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Add Audio Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::RemoveAudioKeyframe { clip_id, param, time_secs } => {
                    let mut linked_partner_id = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            klipp_core::keyframe::remove_keyframe_at(keyframes, *time_secs);
                            break;
                        }
                    }
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut partner.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut partner.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut partner.gain_keyframes,
                                };
                                edit_linked_audio_key(keyframes, frame_time.unwrap_or(*time_secs), tolerance, *time_secs, None);
                                break;
                            }
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.push_history(EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Remove Audio Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::ScaleAudioKeyframeTiming { clip_id, param, factor } => {
                    let mut linked_partner_id = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    let all_ids: Vec<klipp_core::ClipId> = {
                        let mut ids = vec![*clip_id];
                        if let Some(lid) = linked_partner_id { ids.push(lid); }
                        ids
                    };
                    for target_id in &all_ids {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(clip) = track.find_clip_mut(*target_id) {
                                let clip_dur = clip.duration().to_secs_f64();
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                                };
                                for kf in keyframes.iter_mut() {
                                    kf.time_secs *= factor;
                                    // Scale bezier tangent time components
                                    kf.tangent_in.0 *= *factor;
                                    kf.tangent_out.0 *= *factor;
                                }
                                // Clamp and remove out-of-range
                                keyframes.retain(|kf| kf.time_secs >= 0.0 && kf.time_secs <= clip_dur);
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                break;
                            }
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.push_history(EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Scale Audio Keyframe Timing",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::InsertAudioKeyframeBump { clip_id, param, center_time, bump_value, base_value, duration_secs, ramp_secs } => {
                    let mut linked_partner_id = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    let bump_kfs: Vec<(f64, f32)> = vec![
                        (center_time - duration_secs / 2.0 - ramp_secs, *base_value),
                        (center_time - duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0 + ramp_secs, *base_value),
                    ];
                    let all_ids: Vec<klipp_core::ClipId> = {
                        let mut ids = vec![*clip_id];
                        if let Some(lid) = linked_partner_id { ids.push(lid); }
                        ids
                    };
                    for target_id in &all_ids {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(clip) = track.find_clip_mut(*target_id) {
                                let clip_dur = clip.duration().to_secs_f64();
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                                };
                                for &(t, v) in &bump_kfs {
                                    if t >= 0.0 && t <= clip_dur {
                                        keyframes.push(klipp_core::Keyframe {
                                            time_secs: t,
                                            value: v,
                                            interpolation: klipp_core::Interpolation::Linear,
                                            tangent_out: (0.0, 0.0),
                                            tangent_in: (0.0, 0.0),
                                        });
                                    }
                                }
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                break;
                            }
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.push_history(EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Insert Audio Keyframe Bump",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::CopyAudioKeyframes { clip_id, param } => {
                    if let Some(clip) = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id)
                    {
                        let kfs = match param {
                            klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                            klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                            klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                        };
                        self.state.keyframe_clipboard = Some(crate::state::KeyframeClipboard {
                            keyframes: kfs.clone(),
                            source_duration_secs: clip.duration().to_secs_f64(),
                        });
                    }
                    true
                }
                AppCommand::PasteAudioKeyframes { clip_id, param } => {
                    if let Some(ref cb) = self.state.keyframe_clipboard.clone() {
                        let old_state = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let target_dur = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(|c| c.duration().to_secs_f64());
                        if let Some(target_dur) = target_dur {
                            let scale = if cb.source_duration_secs > 0.0 { target_dur / cb.source_duration_secs } else { 1.0 };
                            let mut new_kfs: Vec<klipp_core::Keyframe> = cb.keyframes.iter().map(|kf| {
                                let mut k = kf.clone();
                                k.time_secs *= scale;
                                k
                            }).collect();
                            new_kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                                if let Some(clip) = track.find_clip_mut(*clip_id) {
                                    let target = match param {
                                        klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                        klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                        klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                                    };
                                    *target = new_kfs;
                                    break;
                                }
                            }
                            if let Some(old_snap) = old_state {
                                let new_snap = self.state.sequences[self.state.active_sequence_idx]
                                    .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                                if let Some(new_snap) = new_snap {
                                    self.state.push_history(EditCommand::EditKeyframes {
                                            clip_id: *clip_id,
                                            linked_clip_id: None,
                                            description: "Paste Audio Keyframes",
                                            old_state: old_snap,
                                            new_state: new_snap,
                                            old_linked_state: None,
                                            new_linked_state: None,
                                        });
                                }
                            }
                            self.sync_sequence_to_engine();
                            let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::ReverseAudioKeyframes { clip_id, param } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            if keyframes.len() >= 2 {
                                let max_time = keyframes.last().map(|k| k.time_secs).unwrap_or(0.0);
                                for kf in keyframes.iter_mut() {
                                    kf.time_secs = max_time - kf.time_secs;
                                    let old_in = kf.tangent_in;
                                    let old_out = kf.tangent_out;
                                    kf.tangent_in = (-old_out.0, old_out.1);
                                    kf.tangent_out = (-old_in.0, old_in.1);
                                }
                                keyframes.reverse();
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            }
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let new_snap = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        if let Some(new_snap) = new_snap {
                            self.state.push_history(EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Reverse Audio Keyframes",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::MoveAudioKeyframe { clip_id, param, index, new_time_secs, new_value } => {
                    let mut linked_partner_id = None;
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            linked_partner_id = clip.linked_clip_id;
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            if let Some(kf) = keyframes.get_mut(*index) {
                                kf.time_secs = *new_time_secs;
                                kf.value = *new_value;
                            }
                            keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    // Also move on linked partner clip
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut partner.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut partner.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut partner.gain_keyframes,
                                };
                                if let Some(kf) = keyframes.get_mut(*index) {
                                    kf.time_secs = *new_time_secs;
                                    kf.value = *new_value;
                                }
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                break;
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::SetAudioKeyframeInterpolation { clip_id, param, keyframe_index, interpolation } => {
                    // Capture old state for undo
                    let mut linked_partner_id_interp = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id_interp = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id_interp {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    // Apply to target clip AND linked partner
                    let target_ids: Vec<klipp_core::ClipId> = {
                        let mut ids = vec![*clip_id];
                        if let Some(lid) = linked_partner_id_interp {
                            ids.push(lid);
                        }
                        ids
                    };
                    for target_id in &target_ids {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*target_id) {
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            let ki = *keyframe_index;
                            let len = keyframes.len();
                            if ki < len {
                                keyframes[ki].interpolation = *interpolation;
                            }
                            if ki > 0 {
                                keyframes[ki - 1].interpolation = *interpolation;
                            }
                            if *interpolation == klipp_core::Interpolation::Bezier {
                                for idx in [ki.saturating_sub(1), ki] {
                                    if idx >= len { continue; }
                                    let prev_t = if idx > 0 { keyframes[idx - 1].time_secs } else { keyframes[idx].time_secs - 1.0 };
                                    let next_t = if idx + 1 < len { keyframes[idx + 1].time_secs } else { keyframes[idx].time_secs + 1.0 };
                                    let span_out = (next_t - keyframes[idx].time_secs).abs() * 0.33;
                                    let span_in = (keyframes[idx].time_secs - prev_t).abs() * 0.33;
                                    let kf = &mut keyframes[idx];
                                    if kf.tangent_out == (0.0, 0.0) || kf.tangent_out.0.abs() < 0.5 {
                                        kf.tangent_out = (span_out, 0.0);
                                    }
                                    if kf.tangent_in == (0.0, 0.0) || kf.tangent_in.0.abs() < 0.5 {
                                        kf.tangent_in = (-span_in, 0.0);
                                    }
                                }
                            }
                            break;
                        }
                    }
                    } // end for target_id
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id_interp.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.push_history(EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id_interp,
                                    description: "Set Keyframe Interpolation",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
                AppCommand::SetAudioKeyframeTangent { clip_id, param, keyframe_index, tangent_in, tangent_out } => {
                    let mut linked_partner_id = None;
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            linked_partner_id = clip.linked_clip_id;
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            if let Some(kf) = keyframes.get_mut(*keyframe_index) {
                                if let Some(ti) = tangent_in {
                                    kf.tangent_in = *ti;
                                }
                                if let Some(to) = tangent_out {
                                    kf.tangent_out = *to;
                                }
                            }
                            break;
                        }
                    }
                    // Also set tangent on linked partner clip
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut partner.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut partner.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut partner.gain_keyframes,
                                };
                                if let Some(kf) = keyframes.get_mut(*keyframe_index) {
                                    if let Some(ti) = tangent_in {
                                        kf.tangent_in = *ti;
                                    }
                                    if let Some(to) = tangent_out {
                                        kf.tangent_out = *to;
                                    }
                                }
                                break;
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    true
                }
            _ => false,
        }
    }

    fn handle_audio_clip_props(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::AutoDuckAudio { target_clip_id, sidechain_track_idx, duck_level, attack_secs, release_secs, threshold } => {
                    self.auto_duck_audio(target_clip_id, sidechain_track_idx, duck_level, attack_secs, release_secs, threshold);
                    true
                }
                AppCommand::UpdateClipAudio { clip_id, gain, volume, pan, fade_in_secs, fade_out_secs, fade_in_curve, fade_out_curve, channel_config } => {
                    let mut old_vals = None;
                    let mut linked_partner_id = None;
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            old_vals = Some((clip.gain, clip.volume, clip.pan, clip.fade_in_secs, clip.fade_out_secs, clip.channel_config));
                            linked_partner_id = clip.linked_clip_id;
                            clip.gain = *gain;
                            clip.volume = *volume;
                            clip.pan = *pan;
                            clip.fade_in_secs = *fade_in_secs;
                            clip.fade_out_secs = *fade_out_secs;
                            clip.fade_in_curve = *fade_in_curve;
                            clip.fade_out_curve = *fade_out_curve;
                            clip.channel_config = *channel_config;
                            break;
                        }
                    }
                    // Also sync audio params to the linked partner clip
                    // so both video and audio clips have the same values
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                partner.gain = *gain;
                                partner.volume = *volume;
                                partner.pan = *pan;
                                partner.fade_in_secs = *fade_in_secs;
                                partner.fade_out_secs = *fade_out_secs;
                                partner.fade_in_curve = *fade_in_curve;
                                partner.fade_out_curve = *fade_out_curve;
                                partner.channel_config = *channel_config;
                                break;
                            }
                        }
                    }
                    if let Some((og, ov, op, ofi, ofo, occ)) = old_vals {
                        let drag_key = format!("{:?}:audio", clip_id);
                        if !self.state.active_history().is_dragging(&drag_key) {
                            self.state.push_history(EditCommand::EditClipAudio {
                                    clip_id: *clip_id,
                                    old_gain: og,
                                    old_volume: ov,
                                    old_pan: op,
                                    old_fade_in: ofi,
                                    old_fade_out: ofo,
                                    old_channel_config: occ,
                                    new_gain: *gain,
                                    new_volume: *volume,
                                    new_pan: *pan,
                                    new_fade_in: *fade_in_secs,
                                    new_fade_out: *fade_out_secs,
                                    new_channel_config: *channel_config,
                                });
                        }
                    }
                    self.sync_sequence_to_engine();
                    // Instant clip gain + volume + pan + channel config — bypass command channel latency
                    let _ = self.cmd_tx.send(AppCommand::SetClipGain(*gain));
                    let _ = self.cmd_tx.send(AppCommand::SetClipVolume(*volume));
                    let _ = self.cmd_tx.send(AppCommand::SetClipPan(*pan));
                    let _ = self.cmd_tx.send(AppCommand::SetClipChannelConfig(channel_config_to_u32(*channel_config)));
                    // Also sync sequence for persistence + paused frame refresh
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    true
                }
            _ => false,
        }
    }

    fn handle_audio_drag_lifecycle(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::BeginAudioDrag { ref key, clip_id, old_gain, old_volume, old_pan, old_fade_in, old_fade_out } => {
                    // Look up current channel_config from the clip (drag commands don't carry it)
                    let cc = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id)
                        .map(|c| c.channel_config)
                        .unwrap_or_default();
                    self.state.active_history_mut().begin_drag(
                        key.clone(),
                        HistoryEntry {
                            command: EditCommand::EditClipAudio {
                                clip_id: *clip_id,
                                old_gain: *old_gain,
                                old_volume: *old_volume,
                                old_pan: *old_pan,
                                old_fade_in: *old_fade_in,
                                old_fade_out: *old_fade_out,
                                old_channel_config: cc,
                                new_gain: *old_gain,
                                new_volume: *old_volume,
                                new_pan: *old_pan,
                                new_fade_in: *old_fade_in,
                                new_fade_out: *old_fade_out,
                                new_channel_config: cc,
                            },
                        },
                    );
                    true
                }
                AppCommand::EndAudioDrag { ref key, clip_id, new_gain, new_volume, new_pan, new_fade_in, new_fade_out } => {
                    let _ = key;
                    let cc = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id)
                        .map(|c| c.channel_config)
                        .unwrap_or_default();
                    let (og, ov, op, ofi, ofo, occ) = self.state.active_history().pending_old_entry_ref()
                        .and_then(|e| match &e.command {
                            EditCommand::EditClipAudio { old_gain, old_volume, old_pan, old_fade_in, old_fade_out, old_channel_config, .. } => {
                                Some((*old_gain, *old_volume, *old_pan, *old_fade_in, *old_fade_out, *old_channel_config))
                            }
                            _ => None,
                        })
                        .unwrap_or((*new_gain, *new_volume, *new_pan, *new_fade_in, *new_fade_out, cc));
                    self.state.active_history_mut().end_drag(HistoryEntry {
                        command: EditCommand::EditClipAudio {
                            clip_id: *clip_id,
                            old_gain: og,
                            old_volume: ov,
                            old_pan: op,
                            old_fade_in: ofi,
                            old_fade_out: ofo,
                            old_channel_config: occ,
                            new_gain: *new_gain,
                            new_volume: *new_volume,
                            new_pan: *new_pan,
                            new_fade_in: *new_fade_in,
                            new_fade_out: *new_fade_out,
                            new_channel_config: cc,
                        },
                    });
                    true
                }
            _ => false,
        }
    }

    pub(super) fn handle_audio_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        if self.handle_audio_keyframe_curves(cmd, needs_refresh) { return true; }
        if self.handle_audio_clip_props(cmd, needs_refresh) { return true; }
        if self.handle_audio_drag_lifecycle(cmd, needs_refresh) { return true; }
        false
    }

    /// Push an `EditAudioChain` history entry capturing the before/after
    /// state of a clip's audio chain. Called by every audio-chain
    /// mutator (Add/Remove/Update/Toggle/Wet-Dry/Reorder). Skipped when
    /// the snapshot pair would be identical (defensive — should never
    /// happen if the caller actually mutated state, but it keeps the
    /// undo stack tidy in case a no-op slips through).
    /// Store several params on one audio effect as a single undo step and
    /// rebuild the running chain.
    pub(super) fn set_audio_effect_params(
        &mut self,
        clip_id: klipp_core::ClipId,
        effect_index: usize,
        params: &[(String, f32)],
        description: &'static str,
    ) {
        let old_state = self.state.sequences[self.state.active_sequence_idx]
            .all_clips()
            .find(|c| c.id == clip_id)
            .map(crate::history::AudioChainSnapshot::from_clip);
        let mut applied = false;
        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
            if let Some(clip) = track.find_clip_mut(clip_id) {
                if let Some(inst) = clip.audio_effects.get_mut(effect_index) {
                    for (name, value) in params {
                        if let Some(p) = inst.params.iter_mut().find(|(k, _)| k == name) {
                            p.1 = *value;
                        } else {
                            inst.params.push((name.clone(), *value));
                        }
                    }
                    applied = true;
                }
                break;
            }
        }
        if !applied {
            return;
        }
        self.push_audio_chain_history(clip_id, description, old_state);
        self.sync_sequence_to_engine();
        self.state.dirty = true;
        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
    }

    /// Learn a Broadband Denoiser profile from the clip's conformed audio
    /// on a worker thread. The result arrives as
    /// `AppEvent::DenoiserProfileLearned` and is stored in the effect's
    /// params, so it is part of the project and export uses it too.
    pub(super) fn learn_denoiser_profile(
        &mut self,
        clip_id: klipp_core::ClipId,
        effect_index: usize,
        at_clip_secs: Option<f64>,
    ) {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let Some(clip) = seq.all_clips().find(|c| c.id == clip_id) else { return };
        let window_secs = clip
            .audio_effects
            .get(effect_index)
            .and_then(|inst| inst.params.iter().find(|(k, _)| k == "profile_seconds"))
            .map(|(_, v)| *v as f64)
            .unwrap_or(0.5)
            .clamp(0.1, 5.0);
        let source_in = clip.source_in.to_secs_f64();
        let source_out = clip.source_out.to_secs_f64().max(source_in);
        let pcm_path = clip
            .media_id()
            .and_then(|id| self.state.media_pool.get(&id))
            .and_then(|m| m.audio_cache.as_ref())
            .map(|c| c.pcm_path.clone());
        let Some(pcm_path) = pcm_path else {
            self.state.toast.show("Denoiser: this clip's audio is not ready yet. Try again in a moment.".to_string());
            return;
        };
        let tx = self.proxy_event_tx.clone();
        self.state.toast.show("Learning noise profile\u{2026}".to_string());
        let spawned = std::thread::Builder::new()
            .name("klipp-denoiser-learn".into())
            .spawn(move || {
                const SR: u32 = 48_000;
                let (start, end) = match at_clip_secs {
                    Some(t) => {
                        let start = (source_in + t.max(0.0)).min(source_out);
                        (start, (start + window_secs).min(source_out))
                    }
                    None => (source_in, source_out),
                };
                let mono = match read_pcm_mono_range(&pcm_path, start, end, SR) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.send(AppEvent::DenoiserProfileFailed { error: format!("read audio: {e}") });
                        return;
                    }
                };
                let (offset, slice) = if at_clip_secs.is_some() {
                    (0, &mono[..])
                } else {
                    match klipp_audio::dsp::denoiser::quietest_window(&mono, SR, window_secs as f32) {
                        Some(o) => {
                            let len = ((window_secs * SR as f64) as usize).min(mono.len() - o);
                            (o, &mono[o..o + len])
                        }
                        None => (0, &mono[..]),
                    }
                };
                let Some(bands) = klipp_audio::dsp::denoiser::learn_noise_profile(slice, SR) else {
                    let _ = tx.send(AppEvent::DenoiserProfileFailed {
                        error: "that section is too short or silent to learn from".to_string(),
                    });
                    return;
                };
                let source_secs = start + offset as f64 / SR as f64;
                let mut params: Vec<(String, f32)> = bands
                    .iter()
                    .enumerate()
                    .map(|(b, db)| (klipp_audio::dsp::denoiser::band_param_name(b), *db))
                    .collect();
                params.push(("profile_learned".into(), 1.0));
                params.push(("profile_at".into(), (source_secs - source_in) as f32));
                let _ = tx.send(AppEvent::DenoiserProfileLearned { clip_id, effect_index, params, source_secs });
            });
        if let Err(e) = spawned {
            self.state.toast.show(format!("Denoiser: could not start learning: {e}"));
        }
    }

    pub(super) fn push_audio_chain_history(
        &mut self,
        clip_id: klipp_core::ClipId,
        description: &'static str,
        old_state: Option<crate::history::AudioChainSnapshot>,
    ) {
        let Some(old_state) = old_state else { return };
        let Some(new_state) = self.state.sequences[self.state.active_sequence_idx]
            .all_clips()
            .find(|c| c.id == clip_id)
            .map(crate::history::AudioChainSnapshot::from_clip)
        else {
            return;
        };
        self.state.push_history(EditCommand::EditAudioChain {
                clip_id,
                description,
                old_state,
                new_state,
            });
    }
}
