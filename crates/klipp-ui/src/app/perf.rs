//! Performance: the overlay's stats and the perf recording (#890).

use super::*;

impl KlippApp {
    pub(super) fn update_perf_stats(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.unstable_dt) as f64;

        // Track frame times for FPS calculation
        self.state.perf_frame_times.push(dt);
        if self.state.perf_frame_times.len() > 120 {
            self.state.perf_frame_times.remove(0);
        }

        // Update stats every 250ms
        let now = std::time::Instant::now();
        if now.duration_since(self.state.perf_last_update).as_millis() >= self.prefs.advanced.perf_overlay_interval_ms as u128 {
            let interval_s = now.duration_since(self.state.perf_last_update).as_secs_f64();
            self.state.perf_last_update = now;

            // Delivered video throughput, measured from FrameReady
            // deliveries rather than inferred from stage timings. The
            // stage table only sums what it tracks, so it read 5.2 ms
            // against a 16.7 ms budget while playback was managing
            // 22 fps of a 59.94 fps target.
            if interval_s > 0.0 {
                let delivered = self
                    .state
                    .perf_frame_count
                    .saturating_sub(self.state.perf_last_frame_count);
                self.state.perf_video_fps = delivered as f64 / interval_s;
            }
            self.state.perf_last_frame_count = self.state.perf_frame_count;

            // Readback cost per frame, straight from the engine counters.
            {
                use std::sync::atomic::Ordering::Relaxed;
                let c = klipp_engine::CPU_READBACK_COUNT.load(Relaxed);
                let us = klipp_engine::CPU_READBACK_US.load(Relaxed);
                let dc = c.saturating_sub(self.state.perf_last_readback_count);
                let dus = us.saturating_sub(self.state.perf_last_readback_us);
                self.state.perf_readback_ms = if dc > 0 {
                    (dus as f64 / dc as f64) / 1000.0
                } else {
                    0.0
                };
                self.state.perf_last_readback_count = c;
                self.state.perf_last_readback_us = us;
            }

            // FPS from average frame time
            if !self.state.perf_frame_times.is_empty() {
                let avg_dt: f64 = self.state.perf_frame_times.iter().sum::<f64>()
                    / self.state.perf_frame_times.len() as f64;
                self.state.perf_fps = if avg_dt > 0.0 { 1.0 / avg_dt } else { 0.0 };
            }

            // Memory from /proc/self/statm (Linux)
            if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
                if let Some(rss_pages) = statm.split_whitespace().nth(1) {
                    if let Ok(pages) = rss_pages.parse::<u64>() {
                        self.state.perf_memory_mb = (pages * 4096) as f64 / (1024.0 * 1024.0);
                    }
                }
            }

            // CPU usage from /proc/self/stat (Linux)
            // Fields 14+15 = utime+stime in clock ticks (typically 100 Hz)
            if let Ok(stat) = std::fs::read_to_string("/proc/self/stat") {
                // Parse past the comm field (which may contain spaces/parens)
                if let Some(close_paren) = stat.rfind(')') {
                    let fields_after: Vec<&str> = stat[close_paren + 2..].split_whitespace().collect();
                    // fields_after[11] = utime (field 14), fields_after[12] = stime (field 15)
                    if fields_after.len() > 12 {
                        let utime: u64 = fields_after[11].parse().unwrap_or(0);
                        let stime: u64 = fields_after[12].parse().unwrap_or(0);
                        let total_ticks = utime + stime;

                        let wall_elapsed = self.state.perf_prev_wall_time.elapsed().as_secs_f64();
                        if wall_elapsed > 0.01 && self.state.perf_prev_cpu_ticks > 0 {
                            let tick_delta = total_ticks - self.state.perf_prev_cpu_ticks;
                            let cpu_secs = tick_delta as f64 / 100.0; // 100 = CLK_TCK
                            let instant = (cpu_secs / wall_elapsed) * 100.0;
                            // Exponential moving average (α=0.15) for smooth readout like DAW meters
                            let alpha = 0.15;
                            self.state.perf_cpu_percent =
                                self.state.perf_cpu_percent * (1.0 - alpha) + instant * alpha;
                        }
                        self.state.perf_prev_cpu_ticks = total_ticks;
                        self.state.perf_prev_wall_time = now;
                    }
                }
            }
        }

        // Frame count increments on each FrameReady event (tracked in drain_events)
        // HW decode is set when we get a VAAPI log — for now just read from frame_buffer existence
        // We'll set hw_decode from engine events if available

        // Snapshot the engine's per-frame perf ring for the overlay.
        // Lock once, read latest + averages + drops-in-last-second,
        // drop the lock before painting. Tiny mutex window.
        if self.state.show_perf_overlay {
            if let Ok(ring) = self.perf_ring.lock() {
                // Average over the last ~2 seconds at 60 fps = 120
                // samples; or whatever's in the ring if shorter.
                self.state.perf_averages = ring.averages(120);
                self.state.perf_drops_last_sec = ring.drops_in_last(1.0);
                self.state.perf_latest_sample = ring.latest().copied();
            }
        }
    }

    /// Start / stop a perf telemetry recording. When stopping, the
    /// captured samples are written as JSON to
    /// `<cache_dir>/klipp/perf/session_<unix_ts>.json` so the user
    /// can attach the file to a bug report or share it for analysis.
    pub(super) fn toggle_perf_recording(&mut self) {
        if self.state.perf_recording_active {
            // Stop + dump.
            let samples = match self.perf_ring.lock() {
                Ok(mut ring) => ring.end_recording(),
                Err(_) => Vec::new(),
            };
            self.state.perf_recording_active = false;
            if samples.is_empty() {
                self.state.toast.show("Perf recording: no samples captured");
                return;
            }
            // Unix timestamp — good enough for filename uniqueness.
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("klipp/perf");
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("session_{ts}.json"));
            let json = klipp_engine::perf::samples_to_json(&samples);
            match std::fs::write(&path, json) {
                Ok(_) => {
                    self.state.toast.show(format!(
                        "Perf session saved ({} samples): {}",
                        samples.len(),
                        path.display(),
                    ));
                    self.state.perf_last_session_path = Some(path);
                }
                Err(e) => {
                    self.state.toast.show(format!("Failed to write perf session: {e}"));
                }
            }
        } else {
            // Start.
            if let Ok(mut ring) = self.perf_ring.lock() {
                ring.begin_recording();
            }
            self.state.perf_recording_active = true;
            // Auto-show the overlay so the user sees the REC badge.
            self.state.show_perf_overlay = true;
            self.state.toast.show("Perf recording started · Ctrl+Shift+P to stop");
        }
    }
}
