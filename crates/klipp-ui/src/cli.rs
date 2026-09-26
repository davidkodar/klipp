//! What `klipp` does with its command line: `--version` and `--help`
//! print and exit, which is what the bug report form asks
//! `klipp --version` for. Paths are read too; opening them at startup
//! is not wired up yet (#178), so for now they are accepted and left.
//!
//! Deliberately a few matches, not a framework: there is nothing else to
//! parse.

use std::path::PathBuf;

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cli {
    /// Start, opening these paths (none: start as the preferences say).
    Run(Vec<PathBuf>),
    Version,
    Help,
    /// An option Klipp does not have, to say so and exit.
    Unknown(String),
}

/// Read the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Cli {
    let mut paths = Vec::new();
    let mut only_paths = false;
    for arg in args {
        if !only_paths {
            match arg.to_str() {
                Some("--version" | "-V") => return Cli::Version,
                Some("--help" | "-h") => return Cli::Help,
                // Everything after `--` is a path, even one starting "-".
                Some("--") => {
                    only_paths = true;
                    continue;
                }
                Some(s) if s.starts_with('-') && s.len() > 1 => return Cli::Unknown(s.to_string()),
                _ => {}
            }
        }
        paths.push(PathBuf::from(arg));
    }
    Cli::Run(paths)
}

pub const USAGE: &str = "\
Usage: klipp [OPTIONS]

Options:
  -V, --version   Print the version and build, and exit
  -h, --help      Print this help, and exit";

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<std::ffi::OsString> {
        a.iter().map(Into::into).collect()
    }

    #[test]
    fn version_and_help_win_wherever_they_are() {
        assert_eq!(parse(args(&["--version"])), Cli::Version);
        assert_eq!(parse(args(&["-V"])), Cli::Version);
        assert_eq!(parse(args(&["a.mov", "--help"])), Cli::Help);
    }

    #[test]
    fn paths_are_kept_in_order_and_dashes_after_double_dash_are_paths() {
        assert_eq!(parse(args(&[])), Cli::Run(vec![]));
        assert_eq!(parse(args(&["x.klipp", "a.mov"])), Cli::Run(vec!["x.klipp".into(), "a.mov".into()]));
        assert_eq!(parse(args(&["--", "-odd.mov"])), Cli::Run(vec!["-odd.mov".into()]));
        assert_eq!(parse(args(&["-"])), Cli::Run(vec!["-".into()]), "a lone dash is a name, not an option");
    }

    #[test]
    fn an_option_klipp_does_not_have_is_named() {
        assert_eq!(parse(args(&["--frobnicate"])), Cli::Unknown("--frobnicate".into()));
    }
}
