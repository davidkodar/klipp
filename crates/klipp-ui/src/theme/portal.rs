//! The desktop's appearance settings, read from the freedesktop
//! portal (`org.freedesktop.portal.Settings`, namespace
//! `org.freedesktop.appearance`): whether the user prefers dark or
//! light, and their accent colour (#2625).
//!
//! This is the fallback for "Follow system theme" where no full
//! palette is found (`omarchy.rs`). Any desktop with a portal backend
//! answers it. Dark and light pick the built-in Dark and Light themes,
//! the accent colours them, and no preference keeps the theme the
//! user picked.
//!
//! The portal is read on a thread of its own, never the UI's: the bus
//! may start the portal on the first call, which can take seconds, or
//! never answer. The thread then waits for `SettingChanged`, so a
//! switch on the desktop reaches the app while it runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui::Color32;
use zbus::zvariant::{OwnedValue, Value};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";
const ACCENT_COLOR: &str = "accent-color";

/// The desktop's `color-scheme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    #[default]
    NoPreference,
    Dark,
    Light,
}

impl ColorScheme {
    /// The portal's value: 1 prefers dark, 2 light; 0, and anything it
    /// does not define, no preference.
    pub fn from_portal(value: u32) -> Self {
        match value {
            1 => Self::Dark,
            2 => Self::Light,
            _ => Self::NoPreference,
        }
    }
}

/// What the desktop says about appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Appearance {
    pub scheme: ColorScheme,
    /// The accent, when the desktop sets one.
    pub accent: Option<Color32>,
}

impl Appearance {
    /// Take in one changed setting: whether it was one of ours.
    pub fn apply(&mut self, namespace: &str, key: &str, value: &Value) -> bool {
        if namespace != NAMESPACE {
            return false;
        }
        match key {
            COLOR_SCHEME => self.scheme = scheme_of(value).unwrap_or_default(),
            ACCENT_COLOR => self.accent = accent_of(value),
            _ => return false,
        }
        true
    }
}

/// Where the portal stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PortalStatus {
    /// Not answered yet.
    #[default]
    Looking,
    /// No session bus, no portal, or no appearance settings in it.
    Absent,
    Found(Appearance),
}

/// A value the portal sent, out of any variants wrapped around it:
/// the older `Read` puts the value in one more than `ReadOne` does.
fn unwrapped<'a>(value: &'a Value<'a>) -> &'a Value<'a> {
    match value {
        Value::Value(inner) => unwrapped(inner),
        v => v,
    }
}

/// A `color-scheme` value.
pub fn scheme_of(value: &Value) -> Option<ColorScheme> {
    match unwrapped(value) {
        Value::U32(n) => Some(ColorScheme::from_portal(*n)),
        _ => None,
    }
}

/// An `accent-color` value: red, green and blue from 0 to 1. A channel
/// outside that range means the desktop sets no accent.
pub fn accent_of(value: &Value) -> Option<Color32> {
    let Value::Structure(s) = unwrapped(value) else { return None };
    let channel = |v: &Value| match unwrapped(v) {
        Value::F64(x) if (0.0..=1.0).contains(x) => Some((x * 255.0).round() as u8),
        _ => None,
    };
    match s.fields() {
        [r, g, b] => Some(Color32::from_rgb(channel(r)?, channel(g)?, channel(b)?)),
        _ => None,
    }
}

/// The portal's answer as the reading thread leaves it, and the flag it
/// sets when that changes.
#[derive(Clone, Default)]
pub struct PortalWatch {
    status: Arc<Mutex<PortalStatus>>,
    changed: Arc<AtomicBool>,
}

impl PortalWatch {
    /// The answer so far.
    pub fn status(&self) -> PortalStatus {
        *self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the answer changed since this was last asked.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }

    /// Set the answer and wake the UI.
    pub fn publish(&self, status: PortalStatus, ctx: &egui::Context) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
        self.changed.store(true, Ordering::Release);
        ctx.request_repaint();
    }
}

/// Start reading the portal, and following its changes, on a thread of
/// its own.
pub fn start(ctx: egui::Context) -> PortalWatch {
    let watch = PortalWatch::default();
    let thread_watch = watch.clone();
    let spawned = std::thread::Builder::new().name("desktop-appearance".into()).spawn(move || {
        if let Err(e) = follow(&thread_watch, &ctx) {
            tracing::info!("Desktop appearance: no portal settings ({e})");
        }
        // A connection lost after an answer keeps that answer.
        if thread_watch.status() == PortalStatus::Looking {
            thread_watch.publish(PortalStatus::Absent, &ctx);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("Desktop appearance: cannot start the portal reader: {e}");
        watch.publish(PortalStatus::Absent, &egui::Context::default());
    }
    watch
}

/// Read the appearance settings, then follow their changes until the
/// bus goes away.
fn follow(watch: &PortalWatch, ctx: &egui::Context) -> zbus::Result<()> {
    let connection = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&connection, DESTINATION, PATH, INTERFACE)?;
    // Listening before the first read, so a change in between is not
    // missed.
    let changes = proxy.receive_signal("SettingChanged")?;
    let read = |key: &str| -> zbus::Result<OwnedValue> {
        proxy.call("ReadOne", &(NAMESPACE, key)).or_else(|_| proxy.call("Read", &(NAMESPACE, key)))
    };
    // No color-scheme at all means no appearance settings: an old
    // portal, or none.
    let scheme = read(COLOR_SCHEME)?;
    let mut now = Appearance { scheme: scheme_of(&scheme).unwrap_or_default(), accent: read(ACCENT_COLOR).ok().and_then(|v| accent_of(&v)) };
    tracing::info!("Desktop appearance: {now:?}");
    watch.publish(PortalStatus::Found(now), ctx);
    for message in changes {
        let Ok((namespace, key, value)) = message.body().deserialize::<(String, String, OwnedValue)>() else { continue };
        if now.apply(&namespace, &key, &value) {
            tracing::info!("Desktop appearance changed: {now:?}");
            watch.publish(PortalStatus::Found(now), ctx);
        }
    }
    Ok(())
}

/// A colour's name in a word, for saying what is being followed.
pub fn colour_name(c: Color32) -> &'static str {
    let [r, g, b] = [c.r(), c.g(), c.b()].map(|v| v as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max - min < 0.12 {
        return if max < 0.25 { "black" } else if max > 0.85 { "white" } else { "grey" };
    }
    let d = max - min;
    let hue = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    match hue {
        h if h < 15.0 => "red",
        h if h < 45.0 => "orange",
        h if h < 70.0 => "yellow",
        h if h < 160.0 => "green",
        h if h < 195.0 => "teal",
        h if h < 255.0 => "blue",
        h if h < 290.0 => "purple",
        h if h < 340.0 => "pink",
        _ => "red",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::StructureBuilder;

    fn rgb(r: f64, g: f64, b: f64) -> Value<'static> {
        Value::Structure(StructureBuilder::new().add_field(r).add_field(g).add_field(b).build())
    }

    /// The portal's color-scheme values, bare or in the variant the
    /// older `Read` adds.
    #[test]
    fn color_scheme_values_map_to_a_preference() {
        assert_eq!(scheme_of(&Value::U32(1)), Some(ColorScheme::Dark));
        assert_eq!(scheme_of(&Value::U32(2)), Some(ColorScheme::Light));
        assert_eq!(scheme_of(&Value::U32(0)), Some(ColorScheme::NoPreference));
        assert_eq!(scheme_of(&Value::U32(7)), Some(ColorScheme::NoPreference), "an undefined value is no preference");
        assert_eq!(scheme_of(&Value::Value(Box::new(Value::U32(1)))), Some(ColorScheme::Dark));
        assert_eq!(scheme_of(&Value::from("dark")), None);
    }

    /// An accent is three channels from 0 to 1; one outside that range
    /// means the desktop sets none.
    #[test]
    fn accent_colour_values_map_to_a_colour() {
        assert_eq!(accent_of(&rgb(0.2078, 0.5176, 0.8941)), Some(Color32::from_rgb(53, 132, 228)));
        assert_eq!(accent_of(&Value::Value(Box::new(rgb(1.0, 0.0, 0.0)))), Some(Color32::from_rgb(255, 0, 0)));
        assert_eq!(accent_of(&rgb(-1.0, -1.0, -1.0)), None, "unset");
        assert_eq!(accent_of(&Value::U32(1)), None);
    }

    /// A SettingChanged for ours changes the answer; one for another
    /// namespace or key leaves it.
    #[test]
    fn a_setting_change_updates_the_appearance() {
        let mut a = Appearance { scheme: ColorScheme::Dark, accent: None };
        assert!(a.apply(NAMESPACE, COLOR_SCHEME, &Value::U32(2)));
        assert_eq!(a.scheme, ColorScheme::Light);
        assert!(a.apply(NAMESPACE, ACCENT_COLOR, &rgb(0.0, 0.0, 1.0)));
        assert_eq!(a.accent, Some(Color32::from_rgb(0, 0, 255)));
        assert!(!a.apply("org.gnome.desktop.interface", COLOR_SCHEME, &Value::U32(1)));
        assert!(!a.apply(NAMESPACE, "contrast", &Value::U32(1)));
        assert_eq!(a.scheme, ColorScheme::Light);
    }

    #[test]
    fn colours_are_named_in_a_word() {
        assert_eq!(colour_name(Color32::from_rgb(53, 132, 228)), "blue");
        assert_eq!(colour_name(Color32::from_rgb(230, 97, 0)), "orange");
        assert_eq!(colour_name(Color32::from_rgb(46, 194, 126)), "green");
        assert_eq!(colour_name(Color32::from_rgb(145, 65, 172)), "purple");
        assert_eq!(colour_name(Color32::from_rgb(120, 120, 120)), "grey");
    }
}
