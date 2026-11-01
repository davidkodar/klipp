//! #3305: the user guide's template and recipe examples are files Klipp
//! reads: the title template as a `.klipptitle`, the recipes as
//! `.klipptemplate`s.

fn example(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/src/reference/template-examples").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_guides_title_template_reads() {
    let t = klipp_timeline::TemplateFile::parse(&example("name_strap.klipptitle")).unwrap_or_else(|why| panic!("{why}"));
    let design = t.design.as_ref().expect("designed");
    assert_eq!((t.name.as_str(), design.fields.as_slice(), t.data.objects.len()), ("Name Strap", &["Name".to_string(), "Role".to_string()][..], 2));
}

#[test]
fn the_guides_recipe_reads() {
    let r = klipp_timeline::composition_template::CompositionTemplate::read(&example("gentle_twirl.klipptemplate")).unwrap_or_else(|why| panic!("{why}"));
    assert_eq!((r.name.as_str(), r.parameters.len(), r.bindings.len()), ("Gentle Twirl", 2, 2));
}
