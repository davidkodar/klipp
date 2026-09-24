//! What a reader gets for a media item: every stored result, and the
//! text a model reads.

use crate::{audio::AudioEnvelope, beatsheet::BeatSheet, motion::Motion, shots::Shots};
use std::fmt::Write as _;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaAnalysis {
    pub shots: Option<Shots>,
    pub audio: Option<AudioEnvelope>,
    pub motion: Option<Motion>,
    pub beatsheet: Option<BeatSheet>,
    #[cfg(feature = "transcript")]
    pub transcript: Option<crate::transcript::Transcript>,
    #[cfg(feature = "framing")]
    pub framing: Option<crate::framing::Framing>,
    #[cfg(feature = "ocr")]
    pub screentext: Option<crate::screentext::ScreenText>,
}

impl MediaAnalysis {
    /// Whatever the store holds for `path` at the current schema
    /// versions; empty fields for what has not been made.
    pub fn load(path: &std::path::Path) -> Self {
        let Some(key) = crate::store::key_for(path) else { return Self::default() };
        #[allow(unused_mut)]
        let mut report = Self::load_key(&key);
        // A library track or effect has no speech: a transcript made of
        // it before that was known is a hallucination, and is not read.
        #[cfg(feature = "transcript")]
        if crate::is_licensed_audio(path) {
            report.transcript = None;
        }
        report
    }

    pub fn load_key(key: &str) -> Self {
        let shots = crate::store::load(key, crate::shots::ID, crate::shots::VERSION).and_then(|v| serde_json::from_value(v).ok());
        let audio = crate::store::load(key, crate::audio::ID, crate::audio::VERSION).and_then(|v| serde_json::from_value(v).ok());
        let motion = crate::store::load(key, crate::motion::ID, crate::motion::VERSION).and_then(|v| serde_json::from_value(v).ok());
        let beatsheet = crate::store::load(key, crate::beatsheet::ID, crate::beatsheet::VERSION).and_then(|v| serde_json::from_value(v).ok());
        #[cfg(feature = "transcript")]
        let transcript = crate::store::load(key, crate::transcript::ID, crate::transcript::VERSION).and_then(|v| serde_json::from_value(v).ok());
        #[cfg(feature = "framing")]
        let framing = crate::store::load(key, crate::framing::ID, crate::framing::VERSION).and_then(|v| serde_json::from_value(v).ok());
        #[cfg(feature = "ocr")]
        let screentext = crate::store::load(key, crate::screentext::ID, crate::screentext::VERSION).and_then(|v| serde_json::from_value(v).ok());
        Self {
            shots,
            audio,
            motion,
            beatsheet,
            #[cfg(feature = "transcript")]
            transcript,
            #[cfg(feature = "framing")]
            framing,
            #[cfg(feature = "ocr")]
            screentext,
        }
    }

    pub fn is_empty(&self) -> bool {
        #[cfg(feature = "transcript")]
        if self.transcript.is_some() {
            return false;
        }
        #[cfg(feature = "framing")]
        if self.framing.is_some() {
            return false;
        }
        self.shots.is_none() && self.audio.is_none() && self.motion.is_none()
    }

    /// One line about the part of the file `from..to` (seconds), for a
    /// clip's line in the timeline view: shot count and where the cuts
    /// fall, loudness, silence. Empty when nothing is known.
    pub fn clip_summary(&self, from: f64, to: f64) -> String {
        self.clip_summary_with(from, to, true)
    }

    /// [`Self::clip_summary`], with the picture's facts left out for a
    /// clip that carries only the sound.
    pub fn clip_summary_with(&self, from: f64, to: f64, with_picture: bool) -> String {
        let mut s = String::new();
        if let Some(shots) = self.shots.as_ref().filter(|_| with_picture) {
            let within = shots.within(from, to);
            let cuts: Vec<String> = shots
                .boundaries
                .iter()
                .filter(|b| **b > from && **b < to)
                .take(8)
                .map(|b| format!("{:.1}", b - from))
                .collect();
            let _ = write!(s, "shots {}", within.len());
            if !cuts.is_empty() {
                let more = shots.boundaries.iter().filter(|b| **b > from && **b < to).count().saturating_sub(cuts.len());
                let _ = write!(s, " (cuts at +{} s{})", cuts.join(", +"), if more > 0 { format!(", {more} more") } else { String::new() });
            }
        }
        if let Some(m) = self.motion.as_ref().filter(|_| with_picture) {
            if let Some(class) = m.class_within(from, to) {
                if !s.is_empty() {
                    s.push_str("  ");
                }
                let _ = write!(s, "motion {class}");
            }
        }
        #[cfg(feature = "framing")]
        if let Some(f) = self.framing.as_ref().filter(|_| with_picture) {
            let shots = f.within(from, to);
            if let Some(first) = shots.first() {
                if !s.is_empty() {
                    s.push_str("  ");
                }
                // The first shot's framing, and how many framings the range holds.
                let distinct: std::collections::BTreeSet<&str> = shots.iter().map(|x| x.framing.as_str()).collect();
                let _ = write!(s, "framing {}", first.framing);
                if first.people > 0 {
                    let _ = write!(s, " {} {}{}", first.people, if first.people == 1 { "person" } else { "people" }, if first.position.is_empty() { String::new() } else { format!(" {}", first.position) });
                }
                if !first.objects.is_empty() {
                    let _ = write!(s, " with {}", first.objects.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
                }
                if distinct.len() > 1 {
                    let _ = write!(s, " (+{} other framings)", distinct.len() - 1);
                }
            }
        }
        if let Some(audio) = &self.audio {
            if !s.is_empty() {
                s.push_str("  ");
            }
            match audio.integrated_lufs {
                Some(l) => {
                    let _ = write!(s, "{l:.1} LUFS");
                }
                None => s.push_str("no loudness"),
            }
            if let Some(p) = audio.true_peak_dbtp {
                let _ = write!(s, " peak {p:.1} dBTP");
            }
            let silence = audio.silence_within(from, to);
            let span = (to - from).max(0.001);
            if silence > 0.0 {
                let _ = write!(s, " silence {:.0}%", 100.0 * silence / span);
            }
        }
        #[cfg(feature = "transcript")]
        if let Some(t) = &self.transcript {
            let words = t.words_within(from, to);
            if !s.is_empty() {
                s.push_str("  ");
            }
            if words.is_empty() {
                s.push_str("no speech");
            } else {
                let spoken: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
                let mut shown = String::new();
                for w in &spoken {
                    if shown.chars().count() + w.chars().count() > 70 {
                        shown.push('…');
                        break;
                    }
                    if !shown.is_empty() {
                        shown.push(' ');
                    }
                    shown.push_str(w);
                }
                let _ = write!(s, "speech {} words{}: \"{}\"", words.len(), t.language.as_deref().map(|l| format!(" ({l})")).unwrap_or_default(), shown);
            }
        }
        s
    }

    /// The whole item as text: the shot list and the silences, with
    /// times from the file's start.
    pub fn text(&self, budget_chars: usize) -> String {
        let mut out = String::new();
        if let Some(shots) = &self.shots {
            let _ = writeln!(out, "Shots: {} over {:.2} s", shots.shots.len(), shots.duration_secs);
            for (i, sh) in shots.shots.iter().enumerate() {
                let _ = writeln!(out, "  shot {}  {:.2}-{:.2} s  ({:.1} s)", i + 1, sh.start_secs, sh.end_secs, sh.end_secs - sh.start_secs);
                if out.len() > budget_chars {
                    let _ = writeln!(out, "  … {} more shots", shots.shots.len() - i - 1);
                    break;
                }
            }
        }
        if let Some(m) = &self.motion {
            let _ = writeln!(out, "Motion: {} overall (mean {:.1}, p95 {:.1} on ffmpeg's YDIF scale); per second: {}",
                crate::motion::class_for(m.mean), m.mean, m.p95,
                m.per_second.iter().take(120).map(|v| if crate::motion::valid_sample(*v) { format!("{v:.0}") } else { "?".into() }).collect::<Vec<_>>().join(" "));
        }
        if let Some(bs) = &self.beatsheet {
            out.push_str(&bs.summary());
        }
        #[cfg(feature = "ocr")]
        if let Some(st) = &self.screentext {
            if let Some(bs) = &self.beatsheet {
                out.push_str(&crate::screentext::labelled_beats_text(bs, &crate::screentext::label_beats(bs, st)));
            }
            out.push_str(&st.summary());
        }
        if let Some(a) = &self.audio {
            let _ = writeln!(
                out,
                "Audio: integrated {} LUFS, range {} LU, true peak {} dBTP; silence below {} dB for {} spans, {:.1} s in all",
                a.integrated_lufs.map_or("?".to_string(), |v| format!("{v:.1}")),
                a.loudness_range_lu.map_or("?".to_string(), |v| format!("{v:.1}")),
                a.true_peak_dbtp.map_or("?".to_string(), |v| format!("{v:.1}")),
                a.silence_threshold_db,
                a.silences.len(),
                a.silence_within(0.0, a.duration_secs),
            );
            if let Some(t) = a.loudest_secs {
                let _ = writeln!(out, "  loudest at {t:.2} s (where a hit or a riser lands: put that on the cut)");
            }
            if let (Some(rms), Some(floor)) = (a.rms_db, a.noise_floor_db) {
                let _ = writeln!(out, "  level {rms:.0} dB RMS, noise floor {floor:.0} dB; bands under the whole: low {}, hum {}, high {}",
                    a.low_db.map_or("?".to_string(), |v| format!("{:+.0} dB", v - rms)),
                    a.hum_db.map_or("?".to_string(), |v| format!("{:+.0} dB", v - rms)),
                    a.high_db.map_or("?".to_string(), |v| format!("{:+.0} dB", v - rms)),
                );
            }
            for (what, fix) in a.faults() {
                let _ = writeln!(out, "  sound fault: {what} — {fix}");
            }
            for (i, sp) in a.silences.iter().enumerate() {
                let _ = writeln!(out, "  silence {:.2}-{:.2} s", sp.start_secs, sp.end_secs);
                if out.len() > budget_chars {
                    let _ = writeln!(out, "  … {} more", a.silences.len() - i - 1);
                    break;
                }
            }
        }
        #[cfg(feature = "transcript")]
        if let Some(t) = &self.transcript {
            let _ = writeln!(
                out,
                "Transcript ({}): {} segments, {} words",
                t.language.as_deref().unwrap_or("language unknown"),
                t.segments.len(),
                t.words.len()
            );
            for (i, seg) in t.segments.iter().enumerate() {
                let _ = writeln!(out, "  {:.2}-{:.2} s  {}", seg.start_secs, seg.end_secs, seg.text);
                if out.len() > budget_chars {
                    let _ = writeln!(out, "  … {} more segments", t.segments.len() - i - 1);
                    break;
                }
            }
        }
        #[cfg(feature = "framing")]
        if let Some(f) = &self.framing {
            let _ = writeln!(out, "Framing: {} shots looked at ({} frames)", f.shots.len(), f.frames_looked_at);
            for (i, sh) in f.shots.iter().enumerate() {
                let who = if sh.people == 0 { String::new() } else { format!(", {} {} {}", sh.people, if sh.people == 1 { "person" } else { "people" }, sh.position) };
                let what = if sh.objects.is_empty() { String::new() } else { format!("; {}", sh.objects.join(", ")) };
                let _ = writeln!(out, "  {:.2}-{:.2} s  {}{}{}", sh.start_secs, sh.end_secs, sh.framing, who, what);
                if out.len() > budget_chars {
                    let _ = writeln!(out, "  … {} more shots", f.shots.len() - i - 1);
                    break;
                }
            }
        }
        if out.is_empty() {
            out.push_str("No analysis yet.\n");
        }
        out
    }
}
