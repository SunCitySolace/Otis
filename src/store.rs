//! On-disk layout under `~/.config/otis` and the files Otis keeps there.

use crate::model::Workspace;
use crate::preset;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

fn xdg(var: &str, fallback: &str) -> PathBuf {
    env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(fallback))
}

pub fn config_home() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
}

fn otis_dir() -> PathBuf {
    config_home().join("otis")
}

pub fn presets_dir() -> PathBuf {
    otis_dir().join("presets")
}

pub fn pipewire_conf() -> PathBuf {
    config_home().join("pipewire/pipewire.conf.d/99-otis-eq.conf")
}

/// Lock held by the GUI so the daemon pauses automatic switching.
pub fn lock_file() -> PathBuf {
    xdg("XDG_RUNTIME_DIR", ".cache").join("otis.lock")
}

/// `path` with the home directory abbreviated, for tooltips.
pub fn display(path: &Path) -> String {
    let home = PathBuf::from(env::var_os("HOME").unwrap_or_default());
    match path.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

fn write_file(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, text)
}

fn load_toml<T: DeserializeOwned + Default>(path: &Path) -> T {
    fs::read_to_string(path).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default()
}

fn save_toml<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let text = toml::to_string_pretty(value).map_err(io::Error::other)?;
    write_file(path, &text)
}

// --- config.toml ------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Config {
    /// "Do not show again" on the audio-interruption warning.
    pub skip_restart_warning: bool,
    /// Show the tray icon while the daemon runs.
    pub tray: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { skip_restart_warning: false, tray: true }
    }
}

impl Config {
    pub fn load() -> Self {
        load_toml(&otis_dir().join("config.toml"))
    }

    pub fn save(&self) -> io::Result<()> {
        save_toml(&otis_dir().join("config.toml"), self)
    }
}

// --- devices.toml -----------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct Device {
    /// Cached `node.description`, so unplugged devices stay recognisable.
    pub description: String,
    /// Assigned default preset; absent means None (no EQ).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub preset: Option<String>,
}

/// Hardware sinks keyed by PipeWire `node.name` (never numeric ids).
#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct Devices {
    #[serde(default)]
    pub devices: BTreeMap<String, Device>,
}

impl Devices {
    pub fn load() -> Self {
        load_toml(&otis_dir().join("devices.toml"))
    }

    pub fn save(&self) -> io::Result<()> {
        save_toml(&otis_dir().join("devices.toml"), self)
    }

    pub fn preset_of(&self, node_name: &str) -> Option<&str> {
        self.devices.get(node_name)?.preset.as_deref()
    }

    pub fn assign(&mut self, node_name: &str, description: &str, preset: Option<String>) {
        let device = self.devices.entry(node_name.to_string()).or_default();
        if !description.is_empty() {
            device.description = description.to_string();
        }
        device.preset = preset;
    }
}

// --- presets ----------------------------------------------------------------

fn preset_path(name: &str) -> PathBuf {
    presets_dir().join(format!("{name}.txt"))
}

/// Make `name` safe to use as a file stem.
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c == '/' || c.is_control() { '-' } else { c }).collect();
    cleaned.trim().trim_start_matches('.').to_string()
}

pub fn list_presets() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(presets_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "txt"))
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_string()))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names
}

pub fn load_preset(name: &str) -> Option<Workspace> {
    fs::read_to_string(preset_path(name)).ok().map(|text| preset::parse(&text))
}

pub fn save_preset(name: &str, ws: &Workspace) -> io::Result<()> {
    write_file(&preset_path(name), &preset::serialize(ws))
}

/// A preset name based on `stem` that does not collide with an existing one.
pub fn unique_preset_name(stem: &str) -> String {
    let base = Some(sanitize_name(stem)).filter(|s| !s.is_empty()).unwrap_or_else(|| "Imported".into());
    let taken = list_presets();
    (1..)
        .map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") })
        .find(|candidate| !taken.contains(candidate))
        .unwrap()
}

// --- state.txt --------------------------------------------------------------

/// Last workspace, restored on launch. Same text format as a preset, with
/// two extra comment lines naming the device and preset it belongs to.
#[derive(Default)]
pub struct State {
    pub device: Option<String>,
    pub preset: Option<String>,
    pub workspace: Workspace,
}

const STATE_DEVICE: &str = "# otis-device:";
const STATE_PRESET: &str = "# otis-preset:";

impl State {
    pub fn load() -> Option<Self> {
        Self::load_file("state.txt")
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_file("state.txt")
    }

    fn load_file(name: &str) -> Option<Self> {
        let text = fs::read_to_string(otis_dir().join(name)).ok()?;
        let tagged = |tag: &str| {
            text.lines().find_map(|l| l.strip_prefix(tag)).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
        };
        Some(State { device: tagged(STATE_DEVICE), preset: tagged(STATE_PRESET), workspace: preset::parse(&text) })
    }

    fn save_file(&self, name: &str) -> io::Result<()> {
        let text = format!(
            "{STATE_DEVICE} {}\n{STATE_PRESET} {}\n{}",
            self.device.as_deref().unwrap_or(""),
            self.preset.as_deref().unwrap_or(""),
            preset::serialize(&self.workspace),
        );
        write_file(&otis_dir().join(name), &text)
    }
}
