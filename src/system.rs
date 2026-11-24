//! systemd user-service control and GUI/daemon coordination.

use crate::store;
use std::fs::{self, File};
use std::process::Command;

/// Units that may make up the user's PipeWire stack.
const CANDIDATE_UNITS: [&str; 4] =
    ["pipewire.service", "wireplumber.service", "pipewire-pulse.service", "pipewire-media-session.service"];

pub const DAEMON_UNIT: &str = "otis.service";
const PACKAGED_UNIT: &str = "/usr/lib/systemd/user/otis.service";

fn systemctl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("could not run systemctl: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        Err(format!("systemctl --user {} failed: {}", args.join(" "), err.trim()))
    }
}

/// The candidate units that are currently active.
pub fn active_units() -> Vec<String> {
    // `is-active` prints one state per unit, in order, and exits non-zero
    // unless all are active, so read stdout regardless of the status.
    let output = Command::new("systemctl").args(["--user", "is-active"]).args(CANDIDATE_UNITS).output();
    let Ok(output) = output else { return Vec::new() };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .zip(CANDIDATE_UNITS)
        .filter(|(state, _)| state.trim() == "active")
        .map(|(_, unit)| unit.to_string())
        .collect()
}

/// Restart every active PipeWire unit in one batched call.
pub fn restart_pipewire() -> Result<(), String> {
    let units = active_units();
    if units.is_empty() {
        return Err("no active PipeWire user services found".into());
    }
    let mut args = vec!["restart"];
    args.extend(units.iter().map(String::as_str));
    systemctl(&args).map(drop)
}

pub fn daemon_enabled() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-enabled", "--quiet", DAEMON_UNIT])
        .status()
        .is_ok_and(|s| s.success())
}

/// The unit shipped in packages; source installs get the same text with
/// `ExecStart` pointing at the running binary.
pub const UNIT_TEMPLATE: &str = include_str!("../assets/otis.service");

pub fn set_daemon_enabled(enable: bool) -> Result<(), String> {
    if !enable {
        return systemctl(&["disable", "--now", DAEMON_UNIT]).map(drop);
    }
    let user_unit = store::config_home().join("systemd/user").join(DAEMON_UNIT);
    if !std::path::Path::new(PACKAGED_UNIT).exists() {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let unit = UNIT_TEMPLATE.replace("/usr/bin/otis", &exe.to_string_lossy());
        fs::create_dir_all(user_unit.parent().unwrap()).map_err(|e| e.to_string())?;
        fs::write(&user_unit, unit).map_err(|e| e.to_string())?;
        systemctl(&["daemon-reload"])?;
    }
    systemctl(&["enable", "--now", DAEMON_UNIT]).map(drop)
}

/// Restart the daemon if it is running (to pick up a tray preference).
pub fn reload_daemon() {
    let _ = systemctl(&["try-restart", DAEMON_UNIT]);
}

/// Held by the GUI for its whole lifetime. The kernel releases the lock
/// when the process exits, however it exits.
pub struct GuiLock(#[allow(dead_code)] Option<File>);

impl GuiLock {
    pub fn acquire() -> Self {
        let file = File::create(store::lock_file()).ok().filter(|f| f.try_lock().is_ok());
        GuiLock(file)
    }
}

/// True while an Otis GUI holds the lock.
pub fn gui_running() -> bool {
    match File::open(store::lock_file()) {
        Ok(file) => matches!(file.try_lock_shared(), Err(fs::TryLockError::WouldBlock)),
        Err(_) => false,
    }
}
