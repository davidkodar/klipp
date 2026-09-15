//! The pool's metadata as a sheet: CSV for a spreadsheet, ALE for the
//! tools that read Avid Log Exchange. Out go every clip's logged fields
//! and what the camera recorded; in come the logged fields, matched to
//! clips by file name, so a script supervisor's or DIT's sheet can be
//! written outside and read back.

use klipp_core::{Field, Fps};
use klipp_media::{FieldValue, MediaItem};

/// The sheet's columns, in order: the name first, the fields an editor
/// writes, then what the camera recorded.
pub const COLUMNS: [(&str, Column); 28] = [
    ("Name", Column::Field(Field::Name)),
    ("Scene", Column::Field(Field::Scene)),
    ("Shot", Column::Field(Field::Shot)),
    ("Take", Column::Field(Field::Take)),
    ("Description", Column::Field(Field::Description)),
    ("Comments", Column::Field(Field::Comments)),
    ("Keywords", Column::Field(Field::Keywords)),
    ("Good Take", Column::GoodTake),
    ("Reel", Column::Field(Field::Reel)),
    ("Rating", Column::Rating),
    ("Tags", Column::Field(Field::Tags)),
    ("Label", Column::Field(Field::ColourLabel)),
    ("Start TC", Column::Field(Field::Timecode)),
    ("Duration", Column::Field(Field::Duration)),
    ("FPS", Column::Field(Field::Fps)),
    ("Resolution", Column::Field(Field::Resolution)),
    ("Codec", Column::Field(Field::Codec)),
    ("Camera", Column::Field(Field::Camera)),
    ("Lens", Column::Field(Field::Lens)),
    ("ISO", Column::Iso),
    ("Shutter", Column::Shutter),
    ("Aperture", Column::Aperture),
    ("Focal Length", Column::FocalLength),
    ("White Balance", Column::WhiteBalance),
    ("Gamma", Column::Gamma),
    ("Gamut", Column::Gamut),
    ("Shot On", Column::Field(Field::ShootDate)),
    ("File", Column::Path),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Field(Field),
    GoodTake,
    Rating,
    Iso,
    Shutter,
    Aperture,
    FocalLength,
    WhiteBalance,
    Gamma,
    Gamut,
    Path,
}

impl Column {
    fn text(self, item: &MediaItem) -> String {
        match self {
            Column::Field(f) => f.text(item),
            Column::GoodTake => if item.log.good_take { "Y".to_string() } else { String::new() },
            Column::Rating => if item.rating == 0 { String::new() } else { item.rating.to_string() },
            Column::Iso => item.shot.iso.map(|v| v.to_string()).unwrap_or_default(),
            Column::Shutter => item.shot.shutter.clone().unwrap_or_default(),
            Column::Aperture => item.shot.aperture.map(|v| format!("{v}")).unwrap_or_default(),
            Column::FocalLength => item.shot.focal_length_mm.map(|v| format!("{v}")).unwrap_or_default(),
            Column::WhiteBalance => item.shot.white_balance.clone().unwrap_or_default(),
            Column::Gamma => item.shot.gamma.clone().unwrap_or_default(),
            Column::Gamut => item.shot.colour_gamut.clone().unwrap_or_default(),
            Column::Path => item.path.display().to_string(),
        }
    }
}

/// The sheet as CSV, one row per item, with a header.
pub fn export_csv(items: &[&MediaItem]) -> String {
    let mut out = String::new();
    out.push_str(&COLUMNS.iter().map(|(name, _)| csv_field(name)).collect::<Vec<_>>().join(","));
    out.push('\n');
    for item in items {
        out.push_str(&COLUMNS.iter().map(|(_, c)| csv_field(&c.text(item))).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

/// The sheet as Avid Log Exchange: a heading, the column names, then
/// tab-separated rows. Tabs and line breaks inside a value become
/// spaces, since ALE has no quoting.
pub fn export_ale(items: &[&MediaItem], fps: Fps) -> String {
    let mut out = String::new();
    out.push_str("Heading\nFIELD_DELIM\tTABS\nVIDEO_FORMAT\t1080\nAUDIO_FORMAT\t48khz\n");
    out.push_str(&format!("FPS\t{}\n\nColumn\n", ale_fps(fps)));
    out.push_str(&COLUMNS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join("\t"));
    out.push_str("\n\nData\n");
    for item in items {
        out.push_str(&COLUMNS.iter().map(|(_, c)| ale_field(&c.text(item))).collect::<Vec<_>>().join("\t"));
        out.push('\n');
    }
    out
}

fn ale_fps(fps: Fps) -> String {
    let f = fps.as_f64();
    if (f - f.round()).abs() < 1e-3 { format!("{}", f.round() as u32) } else { format!("{f:.2}") }
}

fn csv_field(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

fn ale_field(v: &str) -> String {
    v.replace(['\t', '\n', '\r'], " ")
}

/// One row read from a sheet: the clip's name and every other column
/// by its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetRow {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

impl SheetRow {
    fn get(&self, column: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(column)).map(|(_, v)| v.as_str())
    }
}

/// Read a sheet, ALE or CSV as it turns out to be. The name column is
/// "Name" or, failing that, the first column.
pub fn parse_sheet(text: &str) -> Result<Vec<SheetRow>, String> {
    let text = text.trim_start_matches('\u{FEFF}');
    let (header, records): (Vec<String>, Vec<Vec<String>>) = if text.trim_start().starts_with("Heading") {
        parse_ale(text)?
    } else {
        parse_csv(text)?
    };
    let name_col = header.iter().position(|h| h.eq_ignore_ascii_case("name")).unwrap_or(0);
    Ok(records
        .into_iter()
        .filter(|r| r.iter().any(|v| !v.trim().is_empty()))
        .map(|r| SheetRow {
            name: r.get(name_col).cloned().unwrap_or_default().trim().to_string(),
            fields: header.iter().cloned().zip(r.into_iter().map(|v| v.trim().to_string())).collect(),
        })
        .collect())
}

fn parse_ale(text: &str) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let mut lines = text.lines();
    let mut header: Option<Vec<String>> = None;
    let mut records = Vec::new();
    let mut in_data = false;
    while let Some(line) = lines.next() {
        let t = line.trim();
        if t.eq_ignore_ascii_case("Column") {
            header = lines.next().map(|h| h.split('\t').map(|s| s.trim().to_string()).collect());
        } else if t.eq_ignore_ascii_case("Data") {
            in_data = true;
        } else if in_data && !t.is_empty() {
            records.push(line.split('\t').map(|s| s.to_string()).collect());
        }
    }
    let header = header.ok_or_else(|| "ALE: no Column line".to_string())?;
    Ok((header, records))
}

fn parse_csv(text: &str) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
        } else {
            match c {
                '"' => quoted = true,
                ',' => row.push(std::mem::take(&mut field)),
                '\r' => {}
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    if quoted {
        return Err("CSV: a quoted value is never closed".to_string());
    }
    let mut it = rows.into_iter();
    let header = it.next().ok_or_else(|| "CSV: empty".to_string())?.into_iter().map(|h| h.trim().to_string()).collect();
    Ok((header, it.collect()))
}

/// Whether a row names this clip: its file name, or the name without
/// its extension, in any case.
pub fn row_names(item: &MediaItem, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    let stem = std::path::Path::new(&item.filename).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let row_stem = std::path::Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    item.filename.eq_ignore_ascii_case(name) || stem.eq_ignore_ascii_case(row_stem)
}

/// Take the editor's fields from a row into the clip: scene, shot,
/// take, description, comments, keywords, good take, reel, rating and
/// tags. A column the sheet does not have is left alone; a present but
/// empty one clears the field, as a sheet is a statement of the whole.
/// Whether anything changed.
pub fn apply_row(item: &mut MediaItem, row: &SheetRow) -> bool {
    let before_log = item.log.clone();
    let (before_rating, before_tags) = (item.rating, item.tags.clone());
    if let Some(v) = row.get("Scene") { item.log.scene = v.to_string(); }
    if let Some(v) = row.get("Shot") { item.log.shot = v.to_string(); }
    if let Some(v) = row.get("Take") { item.log.take = v.to_string(); }
    if let Some(v) = row.get("Description") { item.log.description = v.to_string(); }
    if let Some(v) = row.get("Comments") { item.log.comments = v.to_string(); }
    if let Some(v) = row.get("Keywords") { item.log.set_keywords_line(v); }
    if let Some(v) = row.get("Reel") { item.log.reel = v.to_string(); }
    if let Some(v) = row.get("Good Take") {
        item.log.good_take = matches!(v.trim().to_ascii_lowercase().as_str(), "y" | "yes" | "true" | "1" | "x" | "\u{2713}");
    }
    if let Some(v) = row.get("Rating") {
        let stars = v.chars().filter(|c| *c == '\u{2605}').count() as u8;
        item.rating = if stars > 0 { stars.min(5) } else { v.trim().parse::<u8>().map(|r| r.min(5)).unwrap_or(0) };
    }
    if let Some(v) = row.get("Tags") {
        item.tags = v.split(',').map(|t| t.trim().to_lowercase()).filter(|t| !t.is_empty()).collect();
    }
    item.log != before_log || item.rating != before_rating || item.tags != before_tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_media::MediaItemKind;

    fn item(name: &str) -> MediaItem {
        let mut m = MediaItem::synthetic_for_test(format!("/footage/{name}").into(), None);
        m.kind = MediaItemKind::File;
        m.filename = name.into();
        m.fps = Fps::new(25, 1);
        m
    }

    fn logged() -> MediaItem {
        let mut m = item("A001_C003.mov");
        m.log.scene = "12".into();
        m.log.take = "2".into();
        m.log.description = "Wide, \"the\" hero shot, with a comma".into();
        m.log.keywords = vec!["interview".into(), "wide".into()];
        m.log.good_take = true;
        m.rating = 4;
        m.tags = vec!["day1".into()];
        m.shot.iso = Some(800);
        m.shot.start_timecode = Some("01:02:03:04".into());
        m
    }

    #[test]
    fn csv_round_trips_the_logged_fields_with_quotes_and_commas_intact() {
        let a = logged();
        let mut b = item("B001_C001.mov");
        b.log.comments = "line one\nline two".into();
        let csv = export_csv(&[&a, &b]);
        assert!(csv.starts_with("Name,Scene,Shot,Take,"));
        assert!(csv.contains("\"Wide, \"\"the\"\" hero shot, with a comma\""));
        let rows = parse_sheet(&csv).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "A001_C003.mov");
        assert_eq!(rows[0].get("ISO"), Some("800"));
        let mut fresh = item("a001_c003.MOV");
        assert!(row_names(&fresh, &rows[0].name), "matched by name whatever the case or extension");
        assert!(apply_row(&mut fresh, &rows[0]));
        assert_eq!(fresh.log, a.log);
        assert_eq!((fresh.rating, fresh.tags.clone()), (4, vec!["day1".to_string()]));
        assert!(!apply_row(&mut fresh, &rows[0]), "nothing changes the second time");
        let mut fresh_b = item("B001_C001.mov");
        apply_row(&mut fresh_b, &rows[1]);
        assert_eq!(fresh_b.log.comments, "line one\nline two");
    }

    #[test]
    fn ale_reads_back_what_it_wrote_and_a_sheet_from_elsewhere_is_taken_by_its_headers() {
        let a = logged();
        let ale = export_ale(&[&a], Fps::new(25, 1));
        assert!(ale.starts_with("Heading\nFIELD_DELIM\tTABS\n"));
        assert!(ale.contains("FPS\t25\n"));
        let rows = parse_sheet(&ale).unwrap();
        assert_eq!(rows[0].get("Scene"), Some("12"));
        assert_eq!(rows[0].get("Start TC"), Some("01:02:03:04"));
        let mut fresh = item("A001_C003.mov");
        apply_row(&mut fresh, &rows[0]);
        assert_eq!(fresh.log.keywords, vec!["interview", "wide"]);
        assert!(fresh.log.good_take);

        // A sheet someone else made: only its columns apply, rating as stars.
        let foreign = "Name,Scene,Rating,Good Take\nA001_C003,7,\u{2605}\u{2605}\u{2605},yes\n";
        let rows = parse_sheet(foreign).unwrap();
        let mut fresh = logged();
        assert!(apply_row(&mut fresh, &rows[0]));
        assert_eq!(fresh.log.scene, "7");
        assert_eq!(fresh.rating, 3);
        assert_eq!(fresh.log.take, "2", "a column the sheet lacks is left alone");
        assert!(parse_sheet("Name,Scene\n\"open").is_err());
    }
}
