//! The step to revision 4 (M4 step F): one conversion of every project
//! for picture revision 4's changes (#1391, #1406, #1409, #1386, #1394,
//! #1321), each made by its own row's change before this step converts
//! with them (`Klipp-notes` design, M4 plan v2 §4).
//!
//! [`convert`] takes a project file's text at revision 3 and gives its
//! revision-4 text, with the declared changes it made (the rows that
//! change what a project draws). The rows land one by one; until each
//! has, the step refuses the files it would change, and until all have,
//! it refuses every file ([`NOT_YET`]): the readiness gate's Format
//! family holds each row to its expected result from the start.

/// A project's revision-4 text, and what it declares.
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    pub text: String,
    pub declared: Vec<Declared>,
}

/// A declared change: the row (its issue) and what of the project it
/// changes, for the conversion's record and the readiness report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub row: &'static str,
    pub what: String,
}

/// Why a file isn't converted yet.
pub const NOT_YET: &str = "revision 4 isn't written yet (M4 step F)";

/// `project`'s text at revision 4, or why it isn't converted.
pub fn convert(project: &str) -> Result<Converted, String> {
    let _ = project;
    Err(NOT_YET.into())
}
