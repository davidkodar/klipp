//! The post-export hook: a shell command the user sets in Preferences,
//! run after an export with the file's path and folder.
//!
//! The path is never written into the command's text. `{file}` and
//! `{dir}` become quoted references to `KLIPP_EXPORT_FILE` and
//! `KLIPP_EXPORT_DIR`, and the paths travel in those variables: bash
//! expands a variable's value as a word, never as code. Substituting the
//! path itself let a file name with `$( )`, backticks or a quote run as
//! shell, even inside double quotes (#820).

use std::path::Path;
use std::process::Command;

/// The variables the hook's paths travel in.
pub const FILE_VAR: &str = "KLIPP_EXPORT_FILE";
pub const DIR_VAR: &str = "KLIPP_EXPORT_DIR";

/// `template` with its placeholders as variable references, each quoted
/// for where it stands, so the path is one word wherever the user put it:
/// bare, `"${VAR}"`; inside double quotes, `${VAR}` (already quoted);
/// inside single quotes, where bash expands nothing, the quotes close
/// around `"${VAR}"` and reopen. The template is read as bash reads
/// quotes and backslashes to know which.
pub fn script(template: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }
    let mut out = String::with_capacity(template.len() + 32);
    let mut quote = Quote::None;
    let mut rest = template;
    while let Some(c) = rest.chars().next() {
        let var = if rest.starts_with("{file}") {
            Some(("{file}", FILE_VAR))
        } else if rest.starts_with("{dir}") {
            Some(("{dir}", DIR_VAR))
        } else {
            None
        };
        if let Some((placeholder, name)) = var {
            match quote {
                Quote::None => out.push_str(&format!("\"${{{name}}}\"")),
                Quote::Double => out.push_str(&format!("${{{name}}}")),
                Quote::Single => out.push_str(&format!("'\"${{{name}}}\"'")),
            }
            rest = &rest[placeholder.len()..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
        match (quote, c) {
            (Quote::None, '\'') => quote = Quote::Single,
            (Quote::Single, '\'') => quote = Quote::None,
            (Quote::None, '"') => quote = Quote::Double,
            (Quote::Double, '"') => quote = Quote::None,
            // A backslash outside single quotes escapes the next character.
            (Quote::None | Quote::Double, '\\') => {
                if let Some(next) = rest.chars().next() {
                    out.push(next);
                    rest = &rest[next.len_utf8()..];
                }
            }
            _ => {}
        }
    }
    out
}

/// The hook for an export written to `output`, ready to spawn, or `None`
/// when no hook is set.
pub fn command(template: &str, output: &Path) -> Option<Command> {
    let template = template.trim();
    if template.is_empty() {
        return None;
    }
    let dir = output.parent().unwrap_or(Path::new(""));
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(script(template)).env(FILE_VAR, output).env(DIR_VAR, dir);
    Some(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the hook really receives, run through bash: each hook writes
    /// the path it was given to a file.
    fn received(template: &str, output: &Path, out: &Path) -> String {
        // Run from the output's folder, so a name that did run would leave
        // its canary there rather than wherever the tests run.
        let status = command(template, output).unwrap().env("OUT", out).current_dir(output.parent().unwrap()).status().unwrap();
        assert!(status.success(), "{template}");
        std::fs::read_to_string(out).unwrap()
    }

    /// #820: a file name with shell syntax in it reaches the hook as its
    /// own text, whether the placeholder is bare, double-quoted or
    /// single-quoted, and nothing in it runs.
    #[test]
    fn a_hostile_file_name_reaches_the_hook_as_text() {
        let dir = std::env::temp_dir().join(format!("klipp-hook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let canary = dir.join("ran");
        let name = "a $(touch ran) `touch ran` \"q\" 'x'.mp4";
        let output = dir.join(name);
        let out = dir.join("received");
        for template in [
            r#"printf %s {file} > "$OUT""#,
            r#"printf %s "{file}" > "$OUT""#,
            r#"printf %s '{file}' > "$OUT""#,
        ] {
            assert_eq!(received(template, &output, &out), output.to_string_lossy(), "{template}");
            assert!(!canary.exists(), "{template} ran the file name");
        }
        assert_eq!(received(r#"printf %s {dir} > "$OUT""#, &output, &out), dir.to_string_lossy());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An ordinary hook with both placeholders works as before, the paths
    /// one word each even with spaces, inside a longer quoted string too.
    #[test]
    fn an_ordinary_hook_gets_both_paths() {
        let dir = std::env::temp_dir().join(format!("klipp hook plain {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("my film.mp4");
        let out = dir.join("received");
        assert_eq!(received(r#"echo -n "{file} -> {dir}/archive" > "$OUT""#, &output, &out), format!("{} -> {}/archive", output.display(), dir.display()));
        assert_eq!(received(r#"echo -n 'saved: {file}' > "$OUT""#, &output, &out), format!("saved: {}", output.display()));
        assert_eq!(received(r#"printf '%s|' {file} {dir} > "$OUT""#, &output, &out), format!("{}|{}|", output.display(), dir.display()));
        assert!(command("   ", &output).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
