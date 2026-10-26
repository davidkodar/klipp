//! The measured sweeps' report: one line per item measured, into
//! `target/sweep/<part>.txt`, and one summary line printed.
//!
//! An item that fails is new, unless the part lists it as known with the
//! issue that holds it: then it is reported and passes. A known item that
//! now measures right fails too, so the fix's PR takes it off the list
//! and the sweep holds the fix from then on.

use std::fmt::Write as _;

/// One sweep part's results.
pub struct Sweep {
    part: &'static str,
    known: &'static [(&'static str, u32)],
    lines: Vec<String>,
    ok: usize,
    known_bad: Vec<(String, u32)>,
    new_bad: Vec<String>,
    fixed: Vec<(String, u32)>,
}

impl Sweep {
    /// A sweep named `part` (its report file), with the items known to
    /// fail and each one's issue (0 for one found but not filed yet).
    pub fn new(part: &'static str, known: &'static [(&'static str, u32)]) -> Self {
        Self { part, known, lines: Vec::new(), ok: 0, known_bad: Vec::new(), new_bad: Vec::new(), fixed: Vec::new() }
    }

    /// Record `item`: what was measured, and whether it is as stated.
    pub fn check(&mut self, item: &str, measured: impl Into<String>, pass: bool) {
        let measured = measured.into();
        let issue = self.known.iter().find(|(k, _)| *k == item).map(|(_, i)| *i);
        let verdict = match (pass, issue) {
            (true, None) => {
                self.ok += 1;
                "ok".to_string()
            }
            (false, Some(i)) => {
                self.known_bad.push((item.to_string(), i));
                if i == 0 { "known, not filed".to_string() } else { format!("known #{i}") }
            }
            (false, None) => {
                self.new_bad.push(item.to_string());
                "NEW".to_string()
            }
            (true, Some(i)) => {
                self.fixed.push((item.to_string(), i));
                format!("FIXED #{i}: take it off the known list")
            }
        };
        self.lines.push(format!("{verdict:<10} {item:<36} {measured}"));
    }

    /// Record `item` as not measured here, with why.
    pub fn skip(&mut self, item: &str, why: &str) {
        self.lines.push(format!("{:<10} {item:<36} {why}", "skipped"));
    }

    /// Write the report, print the summary, and fail on anything new or
    /// fixed.
    pub fn finish(self) {
        let mut issues: Vec<u32> = self.known_bad.iter().map(|(_, i)| *i).collect();
        issues.sort_unstable();
        issues.dedup();
        let held = issues.iter().map(|i| if *i == 0 { "not filed".to_string() } else { format!("#{i}") }).collect::<Vec<_>>().join(" ");
        let mut summary = format!("sweep_{}: {} ok, {} known", self.part, self.ok, self.known_bad.len());
        if !held.is_empty() {
            let _ = write!(summary, " ({held})");
        }
        let _ = write!(summary, ", {} new", self.new_bad.len());
        if !self.fixed.is_empty() {
            let _ = write!(summary, ", {} fixed", self.fixed.len());
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/sweep");
        let file = dir.join(format!("{}.txt", self.part));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&file, format!("{summary}\n\n{}\n", self.lines.join("\n")));
        println!("{summary}  ({})", file.display());
        assert!(
            self.new_bad.is_empty() && self.fixed.is_empty(),
            "{summary}\nnew: {:?}\nfixed, take off the known list: {:?}\n{}",
            self.new_bad,
            self.fixed,
            self.lines.join("\n")
        );
    }
}
