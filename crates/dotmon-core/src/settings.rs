//! User settings, persisted as TOML (`~/.config/dotmon/settings.toml`).
//!
//! Loading never fails hard: a missing or corrupt file yields defaults, and
//! every value is clamped to a safe range.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const REFRESH_CHOICES_MS: &[u32] = &[500, 1000, 2000, 5000];
pub const HISTORY_CHOICES: &[u32] = &[60, 120, 300];

const REFRESH_MIN_MS: u32 = 250;
const REFRESH_MAX_MS: u32 = 10_000;
const HISTORY_MIN: u32 = 30;
const HISTORY_MAX: u32 = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Accent {
    #[default]
    Red,
    Amber,
    Green,
    White,
}

impl Accent {
    pub const ALL: [Accent; 4] = [Accent::Red, Accent::Amber, Accent::Green, Accent::White];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|a| *a == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ChartStyle {
    #[default]
    Line,
    Dots,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    pub refresh_ms: u32,
    pub history_len: u32,
    pub accent: Accent,
    pub chart_style: ChartStyle,
    pub animations: bool,
    pub dot_grid: bool,
    pub show_kernel_threads: bool,
    /// Extra process names to protect, on top of the built-in list.
    pub protected_names: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            refresh_ms: 1000,
            history_len: 120,
            accent: Accent::Red,
            chart_style: ChartStyle::Line,
            animations: true,
            dot_grid: true,
            show_kernel_threads: false,
            protected_names: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub enum SettingsError {
    Io(std::io::Error),
    Serialize(toml::ser::Error),
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::Io(e) => write!(f, "E/S : {e}"),
            SettingsError::Serialize(e) => write!(f, "sérialisation : {e}"),
        }
    }
}

impl std::error::Error for SettingsError {}

impl Settings {
    /// Clamps every field to its allowed range.
    pub fn sanitized(mut self) -> Self {
        self.refresh_ms = self.refresh_ms.clamp(REFRESH_MIN_MS, REFRESH_MAX_MS);
        self.history_len = self.history_len.clamp(HISTORY_MIN, HISTORY_MAX);
        self.protected_names.retain(|n| !n.trim().is_empty());
        self
    }

    pub fn from_toml(text: &str) -> Self {
        toml::from_str::<Settings>(text).unwrap_or_default().sanitized()
    }

    pub fn to_toml(&self) -> Result<String, SettingsError> {
        toml::to_string_pretty(self).map_err(SettingsError::Serialize)
    }

    /// Loads settings; missing or unreadable files give defaults.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text),
            Err(_) => Self::default(),
        }
    }

    /// Writes atomically (temp file + rename) so a crash never leaves a
    /// truncated settings file.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(SettingsError::Io)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.to_toml()?).map_err(SettingsError::Io)?;
        std::fs::rename(&tmp, path).map_err(SettingsError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = Settings {
            refresh_ms: 2000,
            accent: Accent::Amber,
            chart_style: ChartStyle::Dots,
            protected_names: vec!["syncthing".into()],
            ..Default::default()
        };
        assert_eq!(Settings::from_toml(&s.to_toml().unwrap()), s);
    }

    #[test]
    fn partial_file_uses_defaults_for_missing_fields() {
        let s = Settings::from_toml("accent = \"green\"\n");
        assert_eq!(s.accent, Accent::Green);
        assert_eq!(s.refresh_ms, 1000);
    }

    #[test]
    fn corrupt_file_gives_defaults() {
        assert_eq!(Settings::from_toml("this is = = not toml"), Settings::default());
    }

    #[test]
    fn values_are_clamped() {
        let s = Settings::from_toml("refresh-ms = 1\nhistory-len = 100000\n");
        assert_eq!(s.refresh_ms, 250);
        assert_eq!(s.history_len, 600);
    }

    #[test]
    fn save_and_load_from_disk() {
        let dir = std::env::temp_dir().join(format!("dotmon-test-{}", std::process::id()));
        let path = dir.join("settings.toml");
        let s = Settings { dot_grid: false, ..Default::default() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        assert!(!path.with_extension("toml.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_gives_defaults() {
        assert_eq!(Settings::load(Path::new("/nonexistent/dotmon/settings.toml")), Settings::default());
    }

    #[test]
    fn accent_index_roundtrip() {
        for a in Accent::ALL {
            assert_eq!(Accent::from_index(a.index()), a);
        }
        assert_eq!(Accent::from_index(99), Accent::Red);
    }
}
