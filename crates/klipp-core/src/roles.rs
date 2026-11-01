//! Roles (#3211): what a clip is for, used across the timeline, the mixer
//! and export. A project has its own set: main roles, each with a few
//! sub-roles, editable, each a colour from the palette unless one is
//! chosen. A clip names its role as a [`RoleRef`], `dialogue` or
//! `dialogue.interview`; the clip's role is what counts, a track's only
//! the one new clips on it start with.

use serde::{Deserialize, Serialize};

use crate::ClipRole;

/// The most main roles a project has, and sub-roles a main role has.
pub const MAX_ROLES: usize = 32;

/// The longest a role's name is, in characters.
pub const MAX_NAME_CHARS: usize = 40;

/// A main role: its id, its name, the colour chosen for it (the
/// palette's when none) and its sub-roles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleDef {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<[u8; 4]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subs: Vec<SubRole>,
}

/// A sub-role: its id within its main role, its name, and the colour
/// chosen for it (its main role's when none).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubRole {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<[u8; 4]>,
}

/// The main roles Klipp starts every project with, which stems and
/// subtitle files are made by: none of them can be removed.
pub const BUILT_IN: [&str; 5] = ["dialogue", "music", "effects", "titles", "subtitles"];

/// A clip's role, `main` or `main.sub`, by id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RoleRef(String);

impl RoleRef {
    /// `text` as a role, `main` or `main.sub`, each part an id; `None`
    /// when it isn't one.
    pub fn parse(text: &str) -> Option<RoleRef> {
        let text = text.trim().to_ascii_lowercase();
        let mut parts = text.split('.');
        let (main, sub) = (parts.next()?, parts.next());
        (parts.next().is_none() && is_id(main) && sub.is_none_or(is_id)).then_some(RoleRef(text))
    }

    pub fn main(&self) -> &str {
        self.0.split('.').next().unwrap_or_default()
    }

    pub fn sub(&self) -> Option<&str> {
        self.0.split_once('.').map(|(_, s)| s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A built-in main role by id, as the code that treats speech, music
    /// and effects apart names it.
    pub fn of(main: &str) -> RoleRef {
        debug_assert!(is_id(main), "{main} is an id");
        RoleRef(main.to_string())
    }

    /// Whether its main role is `main`.
    pub fn is(&self, main: &str) -> bool {
        self.main() == main
    }

    /// Whether it is speech: Dialogue, any of its sub-roles.
    pub fn is_speech(&self) -> bool {
        self.is("dialogue")
    }

    /// Its main role alone.
    pub fn to_main(&self) -> RoleRef {
        RoleRef(self.main().to_string())
    }

    /// The role an old project's clip role is now; `None` for B-roll,
    /// a picture with no role of its own.
    pub fn from_legacy(role: ClipRole) -> Option<RoleRef> {
        let id = match role {
            ClipRole::Dialogue => "dialogue",
            ClipRole::Voiceover => "dialogue.narration",
            ClipRole::Music => "music",
            ClipRole::SFX => "effects",
            ClipRole::Foley => "effects.foley",
            ClipRole::Ambience => "effects.ambience",
            ClipRole::Graphics => "titles",
            ClipRole::BRoll => return None,
        };
        Some(RoleRef(id.into()))
    }
}

impl std::fmt::Display for RoleRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Read as written now, or as an old project wrote a clip's role
/// (`"Voiceover"`); B-roll, which has no role now, isn't one.
impl<'de> Deserialize<'de> for RoleRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let legacy = serde_json::from_value::<ClipRole>(serde_json::Value::String(text.clone())).ok();
        match legacy {
            Some(old) => RoleRef::from_legacy(old).ok_or_else(|| serde::de::Error::custom("B-roll is no role now")),
            None => RoleRef::parse(&text).ok_or_else(|| serde::de::Error::custom(format!("\"{text}\" is not a role: main or main.sub, each lowercase letters, digits and _"))),
        }
    }
}

/// A clip's role as saved, read leniently: a role, an old one, or none
/// for B-roll, nothing written, or text that isn't a role, so a project
/// never fails to open over one clip's role.
pub fn read_lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<RoleRef>, D::Error> {
    let text = Option::<serde_json::Value>::deserialize(d)?;
    Ok(text.and_then(|v| serde_json::from_value::<RoleRef>(v).ok()))
}

/// An id: lowercase letters, digits and `_`, up to 32, starting with a
/// letter.
fn is_id(s: &str) -> bool {
    s.len() <= 32 && s.starts_with(|c: char| c.is_ascii_lowercase()) && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// An id made from `name`: its letters and digits lowercased, other runs
/// as `_`.
pub fn id_from_name(name: &str) -> String {
    let mut id = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            id.push(c.to_ascii_lowercase());
        } else if !id.ends_with('_') && !id.is_empty() {
            id.push('_');
        }
    }
    let id = id.trim_end_matches('_').chars().take(32).collect::<String>();
    if id.starts_with(|c: char| c.is_ascii_lowercase()) { id } else { format!("r{id}").chars().take(32).collect() }
}

fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(format!("a role's name is 1 to {MAX_NAME_CHARS} characters"));
    }
    Ok(name.to_string())
}

/// A project's roles, in the order they are listed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSet {
    pub roles: Vec<RoleDef>,
}

impl Default for RoleSet {
    fn default() -> Self {
        Self::built_in()
    }
}

impl RoleSet {
    /// The roles a new project starts with.
    pub fn built_in() -> Self {
        let role = |id: &str, name: &str, subs: &[(&str, &str)]| RoleDef {
            id: id.into(),
            name: name.into(),
            colour: None,
            subs: subs.iter().map(|(id, name)| SubRole { id: (*id).into(), name: (*name).into(), colour: None }).collect(),
        };
        Self {
            roles: vec![
                role("dialogue", "Dialogue", &[("interview", "Interview"), ("narration", "Narration")]),
                role("music", "Music", &[("score", "Score"), ("source", "Source")]),
                role("effects", "Effects", &[("foley", "Foley"), ("ambience", "Ambience")]),
                role("titles", "Titles", &[]),
                role("subtitles", "Subtitles", &[]),
            ],
        }
    }

    /// This set as a file may have written it, made sound: roles whose
    /// ids aren't ids or are given twice left out, names cut to length
    /// (an empty one named by its id), at most [`MAX_ROLES`] of each, and
    /// any built-in main role missing put back.
    pub fn checked(self) -> RoleSet {
        let name = |n: &str, id: &str| {
            let n: String = n.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME_CHARS).collect();
            if n.is_empty() { id.to_string() } else { n }
        };
        let mut out = RoleSet { roles: Vec::new() };
        for r in self.roles {
            if out.roles.len() >= MAX_ROLES || !is_id(&r.id) || out.main(&r.id).is_some() {
                continue;
            }
            let mut subs: Vec<SubRole> = Vec::new();
            for s in r.subs {
                if subs.len() < MAX_ROLES && is_id(&s.id) && !subs.iter().any(|x| x.id == s.id) {
                    subs.push(SubRole { name: name(&s.name, &s.id), ..s });
                }
            }
            out.roles.push(RoleDef { name: name(&r.name, &r.id), subs, ..r });
        }
        for b in RoleSet::built_in().roles {
            if out.main(&b.id).is_none() {
                if out.roles.len() >= MAX_ROLES {
                    if let Some(k) = out.roles.iter().rposition(|r| !BUILT_IN.contains(&r.id.as_str())) {
                        out.roles.remove(k);
                    }
                }
                out.roles.push(b);
            }
        }
        out
    }

    /// Every role in the set as it is listed: each main role, then its
    /// sub-roles.
    pub fn listed(&self) -> Vec<RoleRef> {
        self.roles.iter().flat_map(|r| std::iter::once(RoleRef(r.id.clone())).chain(r.subs.iter().map(move |s| RoleRef(format!("{}.{}", r.id, s.id))))).collect()
    }

    pub fn main(&self, id: &str) -> Option<&RoleDef> {
        self.roles.iter().find(|r| r.id == id)
    }

    /// `role` as this set has it: itself when its main role and sub-role
    /// are here, its main role when only that is, `None` when neither.
    pub fn resolve(&self, role: &RoleRef) -> Option<RoleRef> {
        let main = self.main(role.main())?;
        match role.sub() {
            Some(sub) if main.subs.iter().any(|s| s.id == sub) => Some(role.clone()),
            _ => Some(role.to_main()),
        }
    }

    /// `role`'s name as it is listed: "Dialogue" or "Dialogue › Interview".
    pub fn label(&self, role: &RoleRef) -> Option<String> {
        let role = self.resolve(role)?;
        let main = self.main(role.main())?;
        Some(match role.sub().and_then(|s| main.subs.iter().find(|x| x.id == s)) {
            Some(sub) => format!("{} › {}", main.name, sub.name),
            None => main.name.clone(),
        })
    }

    /// The colour chosen for `role`: its sub-role's, else its main
    /// role's; `None` when the palette's is used.
    pub fn colour(&self, role: &RoleRef) -> Option<[u8; 4]> {
        let role = self.resolve(role)?;
        let main = self.main(role.main())?;
        role.sub().and_then(|s| main.subs.iter().find(|x| x.id == s)).and_then(|s| s.colour).or(main.colour)
    }

    /// The palette's place for `role`'s main role: its place in the
    /// list, which the palette's role colours are taken in turn by.
    pub fn palette_slot(&self, role: &RoleRef) -> Option<usize> {
        self.roles.iter().position(|r| r.id == role.main())
    }

    /// A main role named `name` added at the end, or a sub-role of
    /// `parent`; the role it is.
    pub fn add(&mut self, parent: Option<&str>, name: &str) -> Result<RoleRef, String> {
        let name = check_name(name)?;
        let id = id_from_name(&name);
        match parent {
            None => {
                if self.roles.len() >= MAX_ROLES {
                    return Err(format!("a project has at most {MAX_ROLES} roles"));
                }
                if self.roles.iter().any(|r| r.id == id || r.name.eq_ignore_ascii_case(&name)) {
                    return Err(format!("there is a role \"{name}\" already"));
                }
                self.roles.push(RoleDef { id: id.clone(), name, colour: None, subs: Vec::new() });
                Ok(RoleRef(id))
            }
            Some(parent) => {
                let main = self.roles.iter_mut().find(|r| r.id == parent).ok_or_else(|| format!("there is no role \"{parent}\""))?;
                if main.subs.len() >= MAX_ROLES {
                    return Err(format!("a role has at most {MAX_ROLES} sub-roles"));
                }
                if main.subs.iter().any(|s| s.id == id || s.name.eq_ignore_ascii_case(&name)) {
                    return Err(format!("{} has a sub-role \"{name}\" already", main.name));
                }
                main.subs.push(SubRole { id: id.clone(), name, colour: None });
                Ok(RoleRef(format!("{parent}.{id}")))
            }
        }
    }

    /// `role` named `name`; its id stays, so clips keep it.
    pub fn rename(&mut self, role: &RoleRef, name: &str) -> Result<(), String> {
        let name = check_name(name)?;
        let main = self.roles.iter_mut().find(|r| r.id == role.main()).ok_or_else(|| format!("there is no role \"{role}\""))?;
        match role.sub() {
            None => main.name = name,
            Some(sub) => main.subs.iter_mut().find(|s| s.id == sub).ok_or_else(|| format!("there is no role \"{role}\""))?.name = name,
        }
        Ok(())
    }

    /// `role` drawn in `colour`, or the palette's again with `None`.
    pub fn set_colour(&mut self, role: &RoleRef, colour: Option<[u8; 4]>) -> Result<(), String> {
        let main = self.roles.iter_mut().find(|r| r.id == role.main()).ok_or_else(|| format!("there is no role \"{role}\""))?;
        match role.sub() {
            None => main.colour = colour,
            Some(sub) => main.subs.iter_mut().find(|s| s.id == sub).ok_or_else(|| format!("there is no role \"{role}\""))?.colour = colour,
        }
        Ok(())
    }

    /// `role` taken out; clips that have it fall back to its main role,
    /// or to none for a main role. A built-in main role stays.
    pub fn remove(&mut self, role: &RoleRef) -> Result<(), String> {
        match role.sub() {
            None if BUILT_IN.contains(&role.main()) => Err(format!("\"{role}\" is built in: stems and subtitle files are made by it")),
            None => {
                let before = self.roles.len();
                self.roles.retain(|r| r.id != role.main());
                (self.roles.len() < before).then_some(()).ok_or_else(|| format!("there is no role \"{role}\""))
            }
            Some(sub) => {
                let main = self.roles.iter_mut().find(|r| r.id == role.main()).ok_or_else(|| format!("there is no role \"{role}\""))?;
                let before = main.subs.len();
                main.subs.retain(|s| s.id != sub);
                (main.subs.len() < before).then_some(()).ok_or_else(|| format!("there is no role \"{role}\""))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> RoleRef {
        RoleRef::parse(s).unwrap_or_else(|| panic!("{s} is a role"))
    }

    /// A project starts with the five main roles and their sub-roles, in
    /// the palette's order, labelled as they are listed.
    #[test]
    fn a_project_starts_with_the_built_in_roles() {
        let set = RoleSet::default();
        assert_eq!(set.roles.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), BUILT_IN);
        assert_eq!(set.label(&r("dialogue.interview")).as_deref(), Some("Dialogue › Interview"));
        assert_eq!(set.label(&r("Music")).as_deref(), Some("Music"), "any case");
        assert_eq!(set.label(&r("effects.ambience")).as_deref(), Some("Effects › Ambience"));
        assert_eq!(set.palette_slot(&r("subtitles.english")), Some(4));
    }

    /// A role is `main` or `main.sub`, each part an id; anything else
    /// isn't one.
    #[test]
    fn a_role_is_a_main_role_and_maybe_a_sub_role() {
        let d = r("dialogue.interview");
        assert_eq!((d.main(), d.sub(), d.to_main().as_str()), ("dialogue", Some("interview"), "dialogue"));
        for bad in ["", ".", "dialogue.", ".interview", "a.b.c", "1st", "dia logue", "dialogue/interview", &"x".repeat(33)] {
            assert!(RoleRef::parse(bad).is_none(), "{bad:?} is no role");
        }
    }

    /// A role the set has resolves to itself; a sub-role it hasn't, to
    /// its main role; a main role it hasn't, to none. Colours come from
    /// the sub-role, else its main role, else the palette.
    #[test]
    fn a_role_resolves_to_what_the_set_has() {
        let mut set = RoleSet::built_in();
        assert_eq!(set.resolve(&r("music.score")), Some(r("music.score")));
        assert_eq!(set.resolve(&r("music.jingle")), Some(r("music")));
        assert_eq!(set.resolve(&r("trailer")), None);
        assert_eq!(set.colour(&r("music.score")), None, "the palette's");
        set.set_colour(&r("music"), Some([1, 2, 3, 255])).unwrap();
        assert_eq!(set.colour(&r("music.score")), Some([1, 2, 3, 255]), "its main role's");
        set.set_colour(&r("music.score"), Some([9, 9, 9, 255])).unwrap();
        assert_eq!(set.colour(&r("music.score")), Some([9, 9, 9, 255]), "its own");
        set.set_colour(&r("music.score"), None).unwrap();
        assert_eq!(set.colour(&r("music.score")), Some([1, 2, 3, 255]));
    }

    /// Roles are added, renamed and removed: an id from the name that
    /// stays through a rename; a name given twice, an empty or long one,
    /// a parent there isn't, too many, or a built-in main role taken out
    /// is refused, saying why.
    #[test]
    fn roles_are_added_renamed_and_removed() {
        let mut set = RoleSet::built_in();
        let spanish = set.add(Some("subtitles"), "Español (ES)").unwrap();
        assert_eq!(spanish.as_str(), "subtitles.espa_ol_es");
        assert_eq!(set.add(None, "Trailer VO").unwrap().as_str(), "trailer_vo");
        assert!(set.add(None, "trailer vo").unwrap_err().contains("already"));
        assert!(set.add(Some("dialogue"), "INTERVIEW").unwrap_err().contains("already"));
        assert!(set.add(Some("nope"), "X").unwrap_err().contains("no role"));
        assert!(set.add(None, " ").unwrap_err().contains("1 to 40"));
        assert!(set.add(None, &"n".repeat(41)).unwrap_err().contains("1 to 40"));
        assert_eq!(id_from_name("2 Cam"), "r2_cam", "an id starts with a letter");

        set.rename(&spanish, "Spanish").unwrap();
        assert_eq!(set.label(&spanish).as_deref(), Some("Subtitles › Spanish"), "renamed, the same id");
        assert!(set.remove(&r("dialogue")).unwrap_err().contains("built in"));
        set.remove(&r("dialogue.narration")).unwrap();
        assert_eq!(set.resolve(&r("dialogue.narration")), Some(r("dialogue")), "its clips fall back to the main role");
        set.remove(&r("trailer_vo")).unwrap();
        assert!(set.remove(&r("trailer_vo")).unwrap_err().contains("no role"));
        while set.roles.len() < MAX_ROLES {
            let n = set.roles.len();
            set.add(None, &format!("Extra {n}")).unwrap();
        }
        assert!(set.add(None, "One more").unwrap_err().contains("at most"));
    }

    /// An old project's clip roles read as the roles they are now, a
    /// role written now reads back as itself, and the set saves and
    /// reads back whole.
    #[test]
    fn old_clip_roles_read_as_the_roles_they_are_now() {
        let read = |s: &str| serde_json::from_str::<RoleRef>(s);
        for (old, now) in [("Dialogue", "dialogue"), ("Voiceover", "dialogue.narration"), ("Music", "music"), ("SFX", "effects"), ("Foley", "effects.foley"), ("Ambience", "effects.ambience"), ("Graphics", "titles")] {
            assert_eq!(read(&format!("\"{old}\"")).unwrap().as_str(), now, "{old}");
            assert_eq!(RoleRef::from_legacy(serde_json::from_str(&format!("\"{old}\"")).unwrap()).unwrap().as_str(), now);
        }
        assert!(RoleRef::from_legacy(ClipRole::BRoll).is_none());
        assert!(read("\"BRoll\"").is_err(), "B-roll is no role now");
        assert!(read("\"no such role!\"").is_err());
        let d = r("dialogue.interview");
        assert_eq!(read(&serde_json::to_string(&d).unwrap()).unwrap(), d);

        let mut set = RoleSet::built_in();
        set.add(Some("subtitles"), "English").unwrap();
        set.set_colour(&r("titles"), Some([200, 180, 0, 255])).unwrap();
        let json = serde_json::to_string(&set).unwrap();
        assert_eq!(serde_json::from_str::<RoleSet>(&json).unwrap(), set);
        assert!(serde_json::from_str::<RoleSet>(r#"{"roles":[],"extra":1}"#).is_err(), "a field it doesn't know is refused");
    }
}
