//! `otis --daemon`: the optional zero-DSP hot-swap daemon and its tray icon.
//!
//! It watches which hardware output is in use and keeps the single Otis EQ
//! virtual sink pointed at it with that device's assigned preset, by
//! rewriting the drop-in and restarting PipeWire, exactly like the GUI's
//! Apply button. It never touches audio itself.

use crate::pw::{self, Conn, Graph};
use crate::pwconf::{self, SINK_NAME};
use crate::store::{self, Config, Devices};
use crate::system;
use ksni::blocking::TrayMethods;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// Quiet time required after the last graph change before acting. Absorbs
/// the burst of events when a device (re)registers.
const DEBOUNCE: Duration = Duration::from_millis(1500);
/// After our own restart, devices re-register (Bluetooth can take seconds).
/// Their brief absence must not be mistaken for an unplug, or the daemon
/// would chase its own restarts forever.
const RESTART_GRACE: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_secs(2);

enum Event {
    GraphChanged,
    /// Tray: switch the active output's preset (`None` disables EQ).
    SetPreset(Option<String>),
    OpenGui,
}

pub fn run(allow_tray: bool) {
    let (tx, rx) = mpsc::channel();
    let graph_tx = tx.clone();
    let mut daemon = Daemon {
        pw: pw::Handle::spawn(move || drop(graph_tx.send(Event::GraphChanged))),
        graph: Graph::default(),
        seen: None,
        active: None,
        due: None,
        restarted_at: None,
        want_default: false,
    };

    let tray = (allow_tray && Config::load().tray)
        .then(|| OtisTray { tx, view: TrayView::default() }.spawn())
        .and_then(|result| result.inspect_err(|e| eprintln!("otis: tray unavailable: {e}")).ok());
    let mut shown = TrayView::default();

    loop {
        let wait = daemon.due.map_or(TICK, |due| due.saturating_duration_since(Instant::now()).min(TICK));
        match rx.recv_timeout(wait) {
            Ok(Event::GraphChanged) => daemon.graph_changed(),
            Ok(Event::SetPreset(preset)) => daemon.set_preset(preset),
            Ok(Event::OpenGui) => daemon.open_gui(),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if daemon.due.is_some_and(|due| Instant::now() >= due) {
            daemon.due = None;
            daemon.reconcile();
        }
        if let Some(tray) = &tray {
            let view = daemon.tray_view();
            if view != shown {
                shown = view.clone();
                tray.update(|t| t.view = view);
            }
        }
    }
}

struct Daemon {
    pw: pw::Handle,
    graph: Graph,
    /// Hardware sinks and default sink of the last settled graph.
    seen: Option<(Vec<String>, Option<String>)>,
    /// The hardware output Otis EQ should serve.
    active: Option<String>,
    /// When the debounced reconcile should run.
    due: Option<Instant>,
    restarted_at: Option<Instant>,
    /// Make Otis EQ the default sink once it reappears after a restart.
    want_default: bool,
}

impl Daemon {
    fn graph_changed(&mut self) {
        self.graph = self.pw.graph();
        if self.graph.conn != Conn::Connected || !self.graph.settled {
            return;
        }
        let graph = self.graph.clone();
        if self.want_default && graph.otis.is_some() {
            if !graph.otis_is_default() {
                self.pw.set_default_sink(SINK_NAME);
            }
            self.want_default = false;
        }

        let sinks: Vec<String> = graph.sinks.iter().map(|s| s.name.clone()).collect();
        let hardware_default = graph.default_sink.clone().filter(|d| sinks.contains(d));
        let devices = Devices::load();
        match &self.seen {
            // First look: serve whatever hardware output is the default.
            None => {
                self.active = hardware_default.or_else(|| graph.otis.as_ref()?.target.clone());
                self.schedule();
            }
            Some((old_sinks, old_default)) => {
                let plugged = sinks.iter().find(|s| !old_sinks.contains(s) && devices.devices.contains_key(*s));
                if let Some(device) = plugged {
                    // A known device appeared: it takes over.
                    self.active = Some(device.clone());
                    self.schedule();
                } else if *old_default != graph.default_sink && hardware_default.is_some() {
                    // The user picked a hardware output as default.
                    self.active = hardware_default;
                    self.schedule();
                } else if old_sinks.iter().any(|s| !sinks.contains(s)) {
                    self.schedule();
                }
            }
        }
        self.seen = Some((sinks, graph.default_sink.clone()));
    }

    fn schedule(&mut self) {
        self.due = Some(Instant::now() + DEBOUNCE);
    }

    fn in_restart_grace(&self) -> bool {
        self.restarted_at.is_some_and(|t| t.elapsed() < RESTART_GRACE)
    }

    /// Bring PipeWire in line with the active device's mapping, if needed.
    fn reconcile(&mut self) {
        // The GUI owns the config while it is open.
        if system::gui_running() || self.graph.conn != Conn::Connected {
            return;
        }
        let online = |name: &String| self.graph.sink(name).is_some();
        if !self.active.as_ref().is_some_and(online) {
            if self.in_restart_grace() {
                // Probably still re-registering; look again later.
                return self.schedule();
            }
            // The active output is gone: fall back to the default one.
            self.active = self.graph.default_sink.clone().filter(online);
        }
        let Some(device) = self.active.clone() else { return };
        let devices = Devices::load();
        let Some(mapping) = devices.devices.get(&device) else { return };
        let live = self.graph.otis.as_ref();

        match mapping.preset.as_deref().and_then(store::load_preset) {
            Some(workspace) => {
                // Already serving this device: leave it alone, even if the
                // hash differs (the user may have applied unsaved tweaks).
                // This check is also what stops restart loops when devices
                // re-register after a PipeWire restart.
                if live.is_some_and(|node| node.target.as_deref() == Some(&device)) {
                    return;
                }
                self.apply(Some((&workspace, &device)));
            }
            None if live.is_some() || pwconf::exists() => self.apply(None),
            None => {}
        }
    }

    /// Write (or delete) the drop-in and restart PipeWire.
    fn apply(&mut self, config: Option<(&crate::model::Workspace, &str)>) {
        let written = match config {
            Some((workspace, device)) => pwconf::write(workspace, device),
            None => pwconf::delete(),
        };
        if let Err(e) = written {
            return eprintln!("otis: could not update the PipeWire config: {e}");
        }
        eprintln!("otis: {}; restarting PipeWire", match config {
            Some((_, device)) => format!("switching Otis EQ to {device}"),
            None => "disabling Otis EQ".into(),
        });
        if let Err(e) = system::restart_pipewire() {
            eprintln!("otis: {e}");
        }
        self.restarted_at = Some(Instant::now());
        self.want_default = config.is_some();
    }

    /// The device tray actions apply to.
    fn tray_device(&self) -> Option<String> {
        let live_target = self.graph.otis.as_ref().and_then(|o| o.target.clone());
        live_target.or_else(|| self.active.clone())
    }

    fn set_preset(&mut self, preset: Option<String>) {
        if system::gui_running() {
            return;
        }
        let Some(device) = self.tray_device() else { return };
        let mut devices = Devices::load();
        let description = self.graph.sink(&device).map(|s| s.description.clone()).unwrap_or_default();
        devices.assign(&device, &description, preset.clone());
        if let Err(e) = devices.save() {
            eprintln!("otis: could not save devices.toml: {e}");
        }
        self.active = Some(device.clone());

        let live = self.graph.otis.as_ref();
        match preset.as_deref().and_then(store::load_preset) {
            Some(workspace) => {
                // An explicit switch compares the full (target, hash) pair.
                let hash = pwconf::config_hash(&workspace, &device);
                if !live.is_some_and(|node| node.hash.as_deref() == Some(&hash)) {
                    self.apply(Some((&workspace, &device)));
                }
            }
            None if live.is_some() || pwconf::exists() => self.apply(None),
            None => {}
        }
    }

    fn open_gui(&self) {
        let spawned = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).spawn());
        if let Err(e) = spawned {
            eprintln!("otis: could not open the GUI: {e}");
        }
    }

    fn tray_view(&self) -> TrayView {
        let devices = Devices::load();
        let device = self.tray_device();
        let preset = device.as_deref().and_then(|d| devices.preset_of(d)).map(str::to_string);
        let output = match &device {
            Some(name) => match (self.graph.sink(name), devices.devices.get(name)) {
                (Some(sink), _) => sink.description.clone(),
                (None, Some(known)) => format!("{} (Offline)", known.description),
                (None, None) => name.clone(),
            },
            None => "none".into(),
        };
        TrayView { output, preset, presets: store::list_presets() }
    }
}

// --- Tray -------------------------------------------------------------------

#[derive(Clone, PartialEq)]
struct TrayView {
    output: String,
    preset: Option<String>,
    presets: Vec<String>,
}

impl Default for TrayView {
    fn default() -> Self {
        TrayView { output: "none".into(), preset: None, presets: Vec::new() }
    }
}

struct OtisTray {
    tx: Sender<Event>,
    view: TrayView,
}

impl ksni::Tray for OtisTray {
    /// Fixed id, so Plasma remembers the icon's visibility across reboots.
    fn id(&self) -> String {
        "otis".into()
    }

    fn title(&self) -> String {
        "Otis EQ".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![tray_icon()]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Otis EQ".into(),
            description: match &self.view.preset {
                Some(preset) => format!("{}: {preset}", self.view.output),
                None => format!("{}: no EQ", self.view.output),
            },
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Event::OpenGui);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, RadioGroup, RadioItem, StandardItem};
        let info = |label: String| MenuItem::from(StandardItem { label, enabled: false, ..Default::default() });
        let selected = self.view.preset.as_ref().and_then(|p| self.view.presets.iter().position(|name| name == p));
        let radio = |label: &str| RadioItem { label: label.to_string(), ..Default::default() };
        vec![
            info(format!("Output: {}", self.view.output)),
            MenuItem::Separator,
            RadioGroup {
                selected: selected.map_or(0, |i| i + 1),
                select: Box::new(|tray: &mut Self, index| {
                    let preset = index.checked_sub(1).and_then(|i| tray.view.presets.get(i).cloned());
                    let _ = tray.tx.send(Event::SetPreset(preset));
                }),
                options: std::iter::once(radio("None")).chain(self.view.presets.iter().map(|p| radio(p))).collect(),
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Disable EQ".into(),
                activate: Box::new(|tray: &mut Self| drop(tray.tx.send(Event::SetPreset(None)))),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open Otis".into(),
                activate: Box::new(|tray: &mut Self| drop(tray.tx.send(Event::OpenGui))),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// A 32 px miniature Otis badge: Warm Ivory "O" on a Charcoal disc (ARGB32).
fn tray_icon() -> ksni::Icon {
    const BG: [f32; 3] = [0x19 as f32, 0x18 as f32, 0x17 as f32];
    const FG: [f32; 3] = [0xf8 as f32, 0xf0 as f32, 0xdf as f32];
    const SIZE: i32 = 32;
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let centre = (SIZE as f32 - 1.0) / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            let outer_alpha = (14.5 - d + 0.5).clamp(0.0, 1.0);
            let ring = (9.8 - d + 0.5).clamp(0.0, 1.0) * (d - 5.2 + 0.5).clamp(0.0, 1.0);
            let mix = |a: f32, b: f32| (a + (b - a) * ring).round() as u8;
            data.extend([
                (outer_alpha * 255.0).round() as u8,
                mix(BG[0], FG[0]),
                mix(BG[1], FG[1]),
                mix(BG[2], FG[2]),
            ]);
        }
    }
    ksni::Icon { width: SIZE, height: SIZE, data }
}
