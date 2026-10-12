//! Where a clip's grade stands (#899).

/// Where a clip's grade stands (#899), as a colourist flags a shot to
/// come back to it or to sign it off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeFlag {
    #[default]
    None,
    /// Still to be graded, or to be gone back to.
    NeedsWork,
    /// Graded, waiting to be signed off.
    Review,
    /// Signed off.
    Approved,
}

impl GradeFlag {
    /// Every flag, none first, as a menu lists them.
    pub const ALL: [GradeFlag; 4] = [GradeFlag::None, GradeFlag::NeedsWork, GradeFlag::Review, GradeFlag::Approved];

    pub fn is_none(&self) -> bool {
        *self == GradeFlag::None
    }

    /// Its name, as the app and the control socket say it.
    pub fn label(self) -> &'static str {
        match self {
            GradeFlag::None => "None",
            GradeFlag::NeedsWork => "Needs Work",
            GradeFlag::Review => "Review",
            GradeFlag::Approved => "Approved",
        }
    }

    /// Its colour (sRGB): red to be gone back to, amber to review, green
    /// signed off.
    pub fn rgb(self) -> [u8; 3] {
        match self {
            GradeFlag::None => [0x80, 0x80, 0x80],
            GradeFlag::NeedsWork => [0xe5, 0x48, 0x4d],
            GradeFlag::Review => [0xf5, 0xa6, 0x23],
            GradeFlag::Approved => [0x3d, 0xc4, 0x6b],
        }
    }

    /// The flag a name gives, any case, spaces or underscores alike.
    pub fn parse(name: &str) -> Option<GradeFlag> {
        let plain = |s: &str| s.chars().filter(char::is_ascii_alphanumeric).collect::<String>().to_lowercase();
        let want = plain(name);
        GradeFlag::ALL.into_iter().find(|f| plain(f.label()) == want)
    }
}
