//! The tuning GUI: one window, four stacked zones plus a slim shortcut footer.

use crate::dsp::{self, PreviewParams};
use crate::graph::{self, format_hz};
use crate::model::{Band, FEED_MAX, FEED_MIN, FREQ_MAX, FREQ_MIN, FilterType, MAX_BANDS, Q_MAX, Q_MIN, Workspace};
use crate::pw::{self, Graph};
use crate::pwconf::{self, SINK_NAME};
use crate::store::{self, Config, Devices, State};
use crate::{preset, system, theme};
use eframe::egui::{self, Align, Button, Color32, ComboBox, DragValue, Key, Layout, Rect, RichText, Sense, Ui};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const UNDO_STEPS: usize = 50;
/// Edits closer together than this collapse into one undo step.
const UNDO_SETTLE: Duration = Duration::from_millis(400);
/// How long after a restart Otis keeps trying to make Otis EQ the default.
const RESTART_SETTLE: Duration = Duration::from_secs(4);
/// The stream is paused this long after Sweep Off, once the fade has played.
const PAUSE_DELAY: Duration = Duration::from_millis(150);
/// How long transient Apply / Disable confirmation stays visible in the footer.
const FLASH_DURATION: Duration = Duration::from_secs(3);

pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Otis")
            .with_app_id("otis")
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([1120.0, 540.0]),
        ..Default::default()
    };
    eframe::run_native("otis", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx)))))
}

#[derive(Clone, Copy, PartialEq)]
enum RestartAction {
    Apply,
    Disable,
}

/// A switch that discards the workspace, held back while it has unsaved edits.
enum Pending {
    Device(String),
    Preset(Option<String>),
    /// Not a switch: Apply, waiting for the workspace to be given a name.
    Apply,
}

enum Modal {
    ConfirmRestart { action: RestartAction, dont_show_again: bool },
    Unsaved(Pending),
    UnsavedApply,
    SaveAs { name: String, then: Option<Pending> },
    Error(String),
}

struct App {
    pw: pw::Handle,
    graph: Graph,
    /// Writer for the audio thread's parameters; replaced per preview stream.
    params_tx: Option<triple_buffer::Input<PreviewParams>>,
    last_params: Option<PreviewParams>,
    last_preview_cmd: (Option<String>, bool),
    pause_at: Option<Instant>,

    config: Config,
    devices: Devices,
    presets: Vec<String>,
    daemon_enabled: bool,
    active_units: Vec<String>,

    /// Selected hardware sink (`node.name`).
    device: Option<String>,
    preset: Option<String>,
    workspace: Workspace,
    /// Serialized form of the preset as last loaded or saved.
    saved: String,

    selected: Option<usize>,
    cursor_hz: f64,
    cursor_drag: bool,
    sweep: bool,
    bypass: bool,
    level_db: f32,

    committed: Workspace,
    last_seen: Workspace,
    last_change: Instant,
    undo: Vec<Workspace>,
    redo: Vec<Workspace>,

    modal: Option<Modal>,
    restart_rx: Option<Receiver<Result<(), String>>>,
    /// Active restart action and whether it enables (true) or disables (false) EQ.
    restarting: Option<(RestartAction, bool)>,
    restart_at: Option<Instant>,
    flash: Option<(&'static str, Instant)>,
    /// Make Otis EQ the default sink once the applied node shows up.
    want_default: bool,
    import_rx: Option<Receiver<Option<PathBuf>>>,
    drop_focus: bool,
    _lock: system::GuiLock,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        theme::apply(ctx);
        let repaint = ctx.clone();
        let state = State::load().unwrap_or_default();
        let mut app = App {
            pw: pw::Handle::spawn(move || repaint.request_repaint()),
            graph: Graph::default(),
            params_tx: None,
            last_params: None,
            last_preview_cmd: (None, false),
            pause_at: None,
            config: Config::load(),
            devices: Devices::load(),
            presets: store::list_presets(),
            daemon_enabled: system::daemon_enabled(),
            active_units: system::active_units(),
            device: state.device,
            preset: None,
            workspace: Workspace::default(),
            saved: String::new(),
            selected: None,
            cursor_hz: 1000.0,
            cursor_drag: false,
            sweep: false,
            bypass: false,
            level_db: dsp::LEVEL_DEFAULT_DB,
            committed: Workspace::default(),
            last_seen: Workspace::default(),
            last_change: Instant::now(),
            undo: Vec::new(),
            redo: Vec::new(),
            modal: None,
            restart_rx: None,
            restarting: None,
            restart_at: None,
            flash: None,
            want_default: false,
            import_rx: None,
            drop_focus: false,
            _lock: system::GuiLock::acquire(),
        };
        // Restore the last workspace on top of the preset it was based on,
        // so unsaved edits come back still marked as unsaved.
        app.load(state.preset);
        app.workspace = state.workspace;
        app.reset_history();
        app
    }

    // --- Workspace, presets, history ------------------------------------------

    fn dirty(&self) -> bool {
        preset::serialize(&self.workspace) != self.saved
    }

    /// False for the pristine "None" preset, which means "no EQ at all".
    fn wants_eq(&self) -> bool {
        self.preset.is_some() || self.dirty()
    }

    fn wanted_hash(&self) -> Option<String> {
        let device = self.device.as_deref()?;
        self.wants_eq().then(|| pwconf::config_hash(&self.workspace, device))
    }

    fn reset_history(&mut self) {
        self.workspace.normalize();
        self.committed = self.workspace.clone();
        self.last_seen = self.workspace.clone();
        self.undo.clear();
        self.redo.clear();
        self.selected = None;
    }

    fn load(&mut self, name: Option<String>) {
        let loaded = name.as_deref().and_then(store::load_preset);
        self.preset = name.filter(|_| loaded.is_some());
        self.workspace = loaded.unwrap_or_default();
        self.saved = preset::serialize(&self.workspace);
        self.reset_history();
    }

    fn save_as(&mut self, name: &str) -> bool {
        self.workspace.normalize();
        if let Err(e) = store::save_preset(name, &self.workspace) {
            self.modal = Some(Modal::Error(format!("Could not save preset \"{name}\": {e}")));
            return false;
        }
        self.saved = preset::serialize(&self.workspace);
        self.preset = Some(name.to_string());
        self.presets = store::list_presets();
        true
    }

    /// Save to the current preset, or ask for a name if there is none.
    fn save(&mut self, then: Option<Pending>) {
        match self.preset.clone() {
            Some(name) => {
                if self.save_as(&name) && let Some(pending) = then {
                    self.switch(pending);
                }
            }
            None => self.modal = Some(Modal::SaveAs { name: String::new(), then }),
        }
    }

    /// Switch device or preset, asking first if that would lose edits.
    fn request(&mut self, pending: Pending) {
        let replaces_workspace = match &pending {
            Pending::Device(name) => self.devices.devices.contains_key(name),
            Pending::Preset(_) => true,
            Pending::Apply => false,
        };
        if replaces_workspace && self.dirty() {
            self.modal = Some(Modal::Unsaved(pending));
        } else {
            self.switch(pending);
        }
    }

    fn switch(&mut self, pending: Pending) {
        match pending {
            Pending::Device(name) => {
                // A known device brings its assigned preset; a new one keeps
                // whatever is in the workspace.
                if self.devices.devices.contains_key(&name) {
                    self.load(self.devices.preset_of(&name).map(str::to_string));
                }
                self.device = Some(name);
            }
            Pending::Preset(name) => self.load(name),
            Pending::Apply => self.request_restart(RestartAction::Apply),
        }
    }

    fn import(&mut self, path: PathBuf) {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => return self.modal = Some(Modal::Error(format!("Could not read {}: {e}", path.display()))),
        };
        let imported = preset::parse(&text);
        if imported.bands.is_empty() {
            return self.modal = Some(Modal::Error(format!(
                "{} contains no Equalizer APO filters (lines like \"Filter 1: ON PK Fc 1000 Hz Gain -3.0 dB Q 1.41\").",
                path.display()
            )));
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Imported");
        let name = store::unique_preset_name(stem);
        match store::save_preset(&name, &imported) {
            Ok(()) => {
                self.presets = store::list_presets();
                self.request(Pending::Preset(Some(name)));
            }
            Err(e) => self.modal = Some(Modal::Error(format!("Could not import preset: {e}"))),
        }
    }

    fn commit(&mut self) {
        if self.workspace != self.committed {
            self.undo.push(std::mem::replace(&mut self.committed, self.workspace.clone()));
            if self.undo.len() > UNDO_STEPS {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    fn step_history(&mut self, back: bool) {
        self.commit();
        let (from, to) = if back { (&mut self.undo, &mut self.redo) } else { (&mut self.redo, &mut self.undo) };
        if let Some(workspace) = from.pop() {
            to.push(std::mem::replace(&mut self.committed, workspace.clone()));
            self.last_seen = workspace.clone();
            self.workspace = workspace;
            self.selected = self.selected.filter(|&i| i < self.workspace.bands.len());
        }
    }

    /// Normalise the workspace and fold finished edits into the undo stack.
    fn track_history(&mut self, ctx: &egui::Context) {
        self.workspace.normalize();
        if let Some(band) = self.selected.and_then(|i| self.workspace.bands.get(i)) {
            self.cursor_hz = band.freq;
        }
        if self.workspace == self.committed {
            return;
        }
        if self.workspace != self.last_seen {
            self.last_seen = self.workspace.clone();
            self.last_change = Instant::now();
        }
        let editing = ctx.input(|i| i.pointer.any_down()) || ctx.egui_wants_keyboard_input();
        if !editing && self.last_change.elapsed() >= UNDO_SETTLE {
            self.commit();
        } else {
            ctx.request_repaint_after(UNDO_SETTLE / 2);
        }
    }

    // --- Band operations --------------------------------------------------------

    fn select(&mut self, index: usize) {
        if let Some(band) = self.workspace.bands.get(index) {
            self.selected = Some(index);
            self.cursor_hz = band.freq;
        }
    }

    fn add_band(&mut self) {
        if self.workspace.bands.len() < MAX_BANDS {
            self.workspace.bands.push(Band::peak(self.cursor_hz));
            self.select(self.workspace.bands.len() - 1);
        }
    }

    fn delete_band(&mut self, index: usize) {
        if index < self.workspace.bands.len() {
            self.workspace.bands.remove(index);
            self.selected = match self.selected {
                Some(s) if s == index => None,
                Some(s) if s > index => Some(s - 1),
                other => other,
            };
        }
    }

    fn sort_bands(&mut self) {
        let selected = self.selected.map(|i| self.workspace.bands[i]);
        self.workspace.bands.sort_by(|a, b| a.freq.total_cmp(&b.freq));
        self.selected = selected.and_then(|band| self.workspace.bands.iter().position(|b| *b == band));
    }

    fn cycle_band(&mut self, backwards: bool) {
        let count = self.workspace.bands.len();
        if count == 0 {
            return;
        }
        let next = match (self.selected, backwards) {
            (None, false) => 0,
            (None, true) => count - 1,
            (Some(i), false) => (i + 1) % count,
            (Some(i), true) => (i + count - 1) % count,
        };
        self.select(next);
    }

    // --- Keyboard ---------------------------------------------------------------

    fn keyboard(&mut self, ctx: &egui::Context) {
        // Focus guard: typing into a field never triggers single-key shortcuts.
        if self.modal.is_some() || ctx.egui_wants_keyboard_input() || egui::Popup::is_any_open(ctx) {
            return;
        }
        let (keys, mods) = ctx.input(|i| {
            let keys: Vec<Key> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, .. } => Some(*key),
                    _ => None,
                })
                .collect();
            (keys, i.modifiers)
        });
        // Default / Shift (coarse) / Alt (fine) steps.
        let (freq_step, gain_step, q_step) = match (mods.shift, mods.alt) {
            (true, _) => (1.10, 2.0, 0.5),
            (_, true) => (1.005, 0.1, 0.02),
            _ => (1.02, 0.5, 0.1),
        };

        for key in keys {
            if mods.command {
                if key == Key::Z {
                    self.step_history(!mods.shift);
                }
                continue;
            }
            match key {
                Key::Space => self.sweep = !self.sweep,
                Key::B => self.bypass = !self.bypass,
                Key::Tab => {
                    self.cycle_band(mods.shift);
                    self.drop_focus = true;
                }
                Key::H | Key::ArrowLeft | Key::L | Key::ArrowRight => {
                    let factor = if matches!(key, Key::H | Key::ArrowLeft) { 1.0 / freq_step } else { freq_step };
                    let freq = match self.selected {
                        Some(i) => &mut self.workspace.bands[i].freq,
                        None => &mut self.cursor_hz,
                    };
                    *freq = (*freq * factor).clamp(FREQ_MIN, FREQ_MAX);
                }
                _ => {
                    if let Some(digit) = (Key::Num1 as u8..=Key::Num9 as u8).position(|k| k == key as u8) {
                        self.select(digit);
                    } else if let Some(index) = self.selected {
                        self.band_key(key, index, gain_step, q_step);
                    } else {
                        match key {
                            Key::A | Key::Enter => self.add_band(),
                            Key::Escape => self.sweep = false,
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// Band-Edit Mode keys.
    fn band_key(&mut self, key: Key, index: usize, gain_step: f64, q_step: f64) {
        let band = &mut self.workspace.bands[index];
        match key {
            Key::K | Key::ArrowUp if band.kind.has_gain() => band.gain += gain_step,
            Key::J | Key::ArrowDown if band.kind.has_gain() => band.gain -= gain_step,
            // Lower Q is a wider band.
            Key::OpenBracket => band.q -= q_step,
            Key::CloseBracket => band.q += q_step,
            Key::M => band.enabled = !band.enabled,
            Key::D | Key::Delete => self.delete_band(index),
            Key::Escape => self.selected = None,
            _ => {}
        }
    }

    // --- PipeWire ---------------------------------------------------------------

    fn device_online(&self) -> bool {
        self.device.as_deref().is_some_and(|d| self.graph.sink(d).is_some())
    }

    fn device_label(&self, name: &str) -> String {
        match (self.graph.sink(name), self.devices.devices.get(name)) {
            (Some(sink), _) => sink.description.clone(),
            (None, Some(device)) if !device.description.is_empty() => format!("{} (Offline)", device.description),
            _ => format!("{name} (Offline)"),
        }
    }

    /// Pull in everything that happened off the UI thread since last frame.
    fn sync(&mut self, ctx: &egui::Context) {
        self.graph = self.pw.graph();

        if self.device.is_none() && self.graph.settled {
            let live_target = self.graph.otis.as_ref().and_then(|o| o.target.clone());
            let default = self.graph.default_sink.clone().filter(|d| self.graph.sink(d).is_some());
            self.device = default.or(live_target).or_else(|| self.graph.sinks.first().map(|s| s.name.clone()));
        }
        // Keep the cached label of a mapped device fresh.
        if let Some(sink) = self.device.as_deref().and_then(|d| self.graph.sink(d))
            && let Some(known) = self.devices.devices.get_mut(&sink.name)
            && known.description != sink.description
        {
            known.description = sink.description.clone();
            let _ = self.devices.save();
        }

        if let Some(result) = self.restart_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.restart_rx = None;
            let outcome = self.restarting.take();
            self.active_units = system::active_units();
            match result {
                Ok(()) => {
                    let msg = if matches!(outcome, Some((_, true))) { "Applied to PipeWire" } else { "EQ disabled" };
                    self.flash = Some((msg, Instant::now()));
                }
                Err(e) => {
                    self.want_default = false;
                    self.modal = Some(Modal::Error(e));
                }
            }
        }
        if self.want_default
            && let Some(live) = &self.graph.otis
            && live.hash.is_some()
            && live.hash == self.wanted_hash()
        {
            if !self.graph.otis_is_default() {
                self.pw.set_default_sink(SINK_NAME);
            }
            self.want_default = false;
        }
        if self.restart_at.is_some_and(|t| t.elapsed() > RESTART_SETTLE) {
            self.restart_at = None;
            self.want_default = false;
        }

        if let Some(path) = self.import_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.import_rx = None;
            if let Some(path) = path {
                self.import(path);
            }
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            self.import(path);
        }
    }

    /// Publish the audio parameters and keep the preview stream in step.
    fn drive_preview(&mut self, ctx: &egui::Context) {
        let online = self.device_online();
        if !online {
            self.sweep = false;
        }
        if let Some(input) = self.pw.take_input() {
            self.params_tx = Some(input);
            self.last_params = None;
        }

        let mut params = PreviewParams {
            playing: self.sweep,
            bypass: self.bypass,
            freq: self.cursor_hz,
            level: dsp::db_to_lin(self.level_db.min(dsp::LEVEL_CAP_DB) as f64),
            preamp: dsp::db_to_lin(self.workspace.preamp_db()),
            bands: self.workspace.bands.len(),
            ..Default::default()
        };
        // Disabled bands keep their slot as a pass-through so toggling one
        // never shuffles filter state between the others.
        for (slot, band) in params.coeffs.iter_mut().zip(&self.workspace.bands) {
            if band.enabled {
                *slot = dsp::band_coeffs(band);
            }
        }
        if self.last_params != Some(params)
            && let Some(tx) = &mut self.params_tx
        {
            tx.write(params);
            self.last_params = Some(params);
        }

        // The stream runs while sweeping, plus a moment after Sweep Off so
        // the fade-out is actually heard before the node is paused.
        let was_on = self.last_preview_cmd.1;
        if self.sweep {
            self.pause_at = None;
        } else if was_on && self.pause_at.is_none() {
            self.pause_at = Some(Instant::now() + PAUSE_DELAY);
        }
        let fading = self.pause_at.is_some_and(|t| Instant::now() < t);
        if fading {
            ctx.request_repaint_after(PAUSE_DELAY / 3);
        }
        let cmd = (self.device.clone().filter(|_| online), self.sweep || fading);
        if cmd != self.last_preview_cmd {
            self.pw.preview(cmd.0.clone(), cmd.1);
            self.last_preview_cmd = cmd;
        }
    }

    fn request_restart(&mut self, action: RestartAction) {
        // What is live always belongs to a named preset, so the device
        // mapping (and with it the daemon) knows what this output uses.
        if action == RestartAction::Apply && self.wants_eq() {
            if self.preset.is_none() {
                self.modal = Some(Modal::SaveAs { name: String::new(), then: Some(Pending::Apply) });
                return;
            }
            if self.dirty() {
                self.modal = Some(Modal::UnsavedApply);
                return;
            }
        }
        self.proceed_restart(action);
    }

    fn proceed_restart(&mut self, action: RestartAction) {
        if self.config.skip_restart_warning {
            self.restart(action);
        } else {
            self.modal = Some(Modal::ConfirmRestart { action, dont_show_again: false });
        }
    }

    /// Write or delete the drop-in, then restart PipeWire in the background.
    fn restart(&mut self, action: RestartAction) {
        let enable = action == RestartAction::Apply && self.wants_eq();
        let description = self.device.as_deref().and_then(|d| self.graph.sink(d)).map(|s| s.description.clone());
        let result = match (&self.device, enable) {
            (Some(device), true) if description.is_some() => {
                self.workspace.normalize();
                pwconf::write(&self.workspace, device).map_err(|e| format!("Could not write the PipeWire config: {e}"))
            }
            (_, true) => Err("Select a connected hardware output before applying.".into()),
            (_, false) => pwconf::delete().map_err(|e| format!("Could not delete the PipeWire config: {e}")),
        };
        if let Err(e) = result {
            return self.modal = Some(Modal::Error(e));
        }
        // Remember what this device now uses; the daemon reads this mapping.
        if let Some(device) = &self.device {
            let preset = self.preset.clone().filter(|_| enable);
            self.devices.assign(device, description.as_deref().unwrap_or_default(), preset);
            let _ = self.devices.save();
        }
        self.save_state();

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || tx.send(system::restart_pipewire()));
        self.restart_rx = Some(rx);
        self.restarting = Some((action, enable));
        self.restart_at = Some(Instant::now());
        self.flash = None;
        self.want_default = enable;
    }

    fn save_state(&self) {
        let state = State { device: self.device.clone(), preset: self.preset.clone(), workspace: self.workspace.clone() };
        if let Err(e) = state.save() {
            eprintln!("otis: could not save state: {e}");
        }
    }

    // --- Zone 1: routing & action bar -------------------------------------------

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Output").color(theme::TEXT_DIM));
            self.device_picker(ui);
            ui.label(RichText::new("Preset").color(theme::TEXT_DIM));
            self.preset_picker(ui);

            let dirty = self.dirty();
            let save_hint = if dirty { "Save the workspace to the current preset" } else { "No unsaved changes" };
            if ui.add_enabled(dirty, Button::new("Save")).on_hover_text(save_hint).clicked() {
                self.save(None);
            }
            if ui.button("Save As…").clicked() {
                let name = self.preset.clone().unwrap_or_default();
                self.modal = Some(Modal::SaveAs { name, then: None });
            }
            let importing = self.import_rx.is_some();
            let import = ui.add_enabled(!importing, Button::new("Import…")).on_hover_text(
                "Import an Equalizer APO / AutoEQ preset (.txt). You can also drop a file onto the window.",
            );
            if import.clicked() {
                let (tx, rx) = mpsc::channel();
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let picked = rfd::FileDialog::new()
                        .set_title("Import EQ preset")
                        .add_filter("Equalizer APO preset", &["txt"])
                        .pick_file();
                    let _ = tx.send(picked);
                    ctx.request_repaint();
                });
                self.import_rx = Some(rx);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let conf = store::display(&store::pipewire_conf());
                let units = if self.active_units.is_empty() { "no active units found".into() } else { self.active_units.join(", ") };
                let busy = self.restart_rx.is_some();

                let disable_label = if matches!(self.restarting, Some((RestartAction::Disable, _))) {
                    "Restarting…"
                } else {
                    "Disable EQ"
                };
                let disable = ui
                    .add_enabled(!busy, Button::new(disable_label))
                    .on_hover_text(format!("Deletes {conf}\nand restarts: {units}"));
                if disable.clicked() {
                    self.request_restart(RestartAction::Disable);
                }

                let apply_label = if matches!(self.restarting, Some((RestartAction::Apply, _))) {
                    "Restarting…"
                } else {
                    "Apply to PipeWire"
                };
                let apply = Button::new(RichText::new(apply_label).strong().color(theme::ON_ACCENT)).fill(theme::ACCENT);
                let effect = if self.wants_eq() { format!("Writes {conf}") } else { format!("Preset is None: deletes {conf}") };
                let apply = ui.add_enabled(!busy, apply).on_hover_text(format!("{effect}\nand restarts: {units}"));
                if apply.clicked() {
                    self.request_restart(RestartAction::Apply);
                }

                self.settings_menu(ui);
                self.crossfeed_menu(ui);
            });
        });
    }

    fn device_picker(&mut self, ui: &mut Ui) {
        let current = self.device.as_deref().map(|d| self.device_label(d)).unwrap_or_else(|| "No output".into());
        let mut picked = None;
        ComboBox::from_id_salt("device")
            .width(200.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                for sink in &self.graph.sinks {
                    if ui.selectable_label(self.device.as_deref() == Some(&sink.name), &sink.description).clicked() {
                        picked = Some(sink.name.clone());
                    }
                }
                // Mapped devices that are unplugged stay listed, greyed out.
                for name in self.devices.devices.keys().filter(|name| self.graph.sink(name).is_none()) {
                    let label = RichText::new(self.device_label(name)).color(theme::TEXT_DIM);
                    if ui.selectable_label(self.device.as_deref() == Some(name), label).clicked() {
                        picked = Some(name.clone());
                    }
                }
                if self.graph.sinks.is_empty() && self.devices.devices.is_empty() {
                    ui.label(RichText::new("No hardware outputs found").color(theme::TEXT_DIM));
                }
            })
            .response
            .on_hover_text("Target hardware output sink for Otis EQ and sweep preview");
        if let Some(name) = picked.filter(|name| self.device.as_ref() != Some(name)) {
            self.request(Pending::Device(name));
        }
    }

    fn preset_picker(&mut self, ui: &mut Ui) {
        let dirty = self.dirty();
        let current = format!("{}{}", self.preset.as_deref().unwrap_or("None"), if dirty { " *" } else { "" });
        let mut picked = None;
        ComboBox::from_id_salt("preset")
            .width(150.0)
            .selected_text(current)
            .show_ui(ui, |ui| {
                if ui.selectable_label(self.preset.is_none(), "None").clicked() {
                    picked = Some(None);
                }
                for name in &self.presets {
                    if ui.selectable_label(self.preset.as_ref() == Some(name), name).clicked() {
                        picked = Some(Some(name.clone()));
                    }
                }
            })
            .response
            .on_hover_text("EQ preset for this output. Choose None to remove EQ on Apply.");
        if let Some(name) = picked.filter(|name| *name != self.preset || dirty) {
            self.request(Pending::Preset(name));
        }
    }

    /// A PipeWire that is unreachable or too old makes Otis useless; say so.
    fn pipewire_problem(&self) -> Option<String> {
        match self.graph.conn {
            pw::Conn::Unavailable => Some("Cannot connect to PipeWire.".into()),
            pw::Conn::Connected if !self.graph.version_ok() => Some(format!(
                "PipeWire {} is too old: Otis needs {}.{} or newer for the built-in param_eq node.",
                self.graph.version.as_deref().unwrap_or("?"),
                pw::MIN_VERSION.0,
                pw::MIN_VERSION.1,
            )),
            _ => None,
        }
    }

    fn settings_menu(&mut self, ui: &mut Ui) {
        let settings = ui.button("Settings");
        egui::Popup::menu(&settings).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(260.0);
            let mut daemon = self.daemon_enabled;
            let toggle = ui.checkbox(&mut daemon, "Run hot-swap daemon (otis.service)").on_hover_text(
                "A background service that re-targets Otis EQ and switches to each device's preset \
                 when outputs are plugged in or removed. It does no audio processing.",
            );
            if toggle.changed() {
                match system::set_daemon_enabled(daemon) {
                    Ok(()) => self.daemon_enabled = daemon,
                    Err(e) => self.modal = Some(Modal::Error(e)),
                }
            }
            if ui.checkbox(&mut self.config.tray, "Show tray icon while the daemon runs").changed() {
                let _ = self.config.save();
                system::reload_daemon();
            }
            ui.separator();
            let reset = ui.add_enabled(self.config.skip_restart_warning, Button::new("Reset dismissed warnings"));
            if reset.clicked() {
                self.config.skip_restart_warning = false;
                let _ = self.config.save();
            }
        });
    }

    fn crossfeed_menu(&mut self, ui: &mut Ui) {
        let cf = self.workspace.crossfeed;
        let tip = if cf.enabled {
            format!(
                "Crossfeed: {:.1} dB feed (700 Hz, 260 µs).\n\
                 Applied in PipeWire; not part of the sweep preview.",
                cf.feed_db
            )
        } else {
            "Crossfeed: Off.\n\
             Feeds a low-passed (700 Hz), slightly delayed (260 µs) copy of each channel into the other.\n\
             Not part of the sweep preview."
                .into()
        };
        let button = ui.button("Crossfeed").on_hover_text(tip);
        if cf.enabled {
            let dot = button.rect.right_top() + egui::vec2(-5.0, 5.0);
            ui.painter().circle_filled(dot, 3.0, theme::ACCENT);
        }
        egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(510.0);
            let crossfeed = &mut self.workspace.crossfeed;
            ui.checkbox(&mut crossfeed.enabled, "Enable crossfeed");
            ui.add_enabled_ui(crossfeed.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Feed");
                    // 400 px across a 40.0 dB span = 1 px per 0.1 dB; smart_aim off
                    // stops egui from snapping the pointer to round .0 / .5 values.
                    ui.spacing_mut().slider_width = 400.0;
                    ui.add(
                        egui::Slider::new(&mut crossfeed.feed_db, FEED_MIN..=FEED_MAX)
                            .step_by(0.1)
                            .smart_aim(false)
                            .drag_value_speed(0.05)
                            .fixed_decimals(1)
                            .suffix(" dB"),
                    );
                });
            });
            ui.label(RichText::new("700 Hz low-pass · 260 µs delay").size(11.0).color(theme::TEXT_DIM));
            ui.separator();

            const REFERENCES: [(&str, &str); 5] = [
                ("-43 to -15 dB", "Ultra-subtle — keeps headphone width, gently eases hard-pan fatigue"),
                ("-9.5 dB", "Jan Meier — subtle and airy, softens hard panning with minimal tonal shift"),
                ("-6.0 dB", "Chu Moy — balanced speaker-like center image; natural all-rounder"),
                ("-4.5 dB", "BS2B Default — distinct nearfield monitor feel; narrower, cohesive stage"),
                ("-3.0 dB", "Maximum — heavy blend approaching mono; tames extreme 60s hard-panning"),
            ];
            egui::Grid::new("crossfeed_ref")
                .num_columns(2)
                .spacing([14.0, 5.0])
                .striped(true)
                .show(ui, |ui| {
                    ui.label(RichText::new("Feed Level").size(10.5).color(theme::TEXT_DIM));
                    ui.label(RichText::new("Reference & Subjective Effect").size(10.5).color(theme::TEXT_DIM));
                    ui.end_row();

                    for (level, desc) in REFERENCES {
                        ui.label(RichText::new(level).monospace().size(11.0).color(theme::TEXT));
                        ui.label(RichText::new(desc).size(11.0).color(theme::TEXT_DIM));
                        ui.end_row();
                    }
                });

            ui.add_space(6.0);
            egui::Frame::new()
                .fill(theme::SURFACE)
                .stroke(egui::Stroke::new(1.0, theme::HAIRLINE))
                .corner_radius(egui::CornerRadius::same(5))
                .inner_margin(egui::Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        ui.label(RichText::new("Note:").strong().size(11.0).color(theme::WARN));
                        ui.label(
                            RichText::new("Crossfeed is applied in PipeWire only and is not audible during sweep preview.")
                                .size(11.0)
                                .color(theme::TEXT_DIM),
                        );
                    });
                });
        });
    }

    // --- Zone 3: sweep & preview strip ------------------------------------------

    fn sweep_strip(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let label = if self.sweep { "Sweep On" } else { "Sweep Off" };
            let mut button = Button::new(RichText::new(label).strong()).min_size(egui::vec2(118.0, 28.0));
            if self.sweep {
                button = Button::new(RichText::new(label).strong().color(theme::ON_ACCENT))
                    .min_size(egui::vec2(118.0, 28.0))
                    .fill(theme::ACCENT);
            }
            let hint = if self.device_online() {
                "Play a sine at the cursor frequency, straight to the hardware output (Space)"
            } else {
                "Select a connected hardware output to preview"
            };
            if ui.add_enabled(self.device_online(), button).on_hover_text(hint).clicked() {
                self.sweep = !self.sweep;
            }
            ui.label(RichText::new("Level").color(theme::TEXT_DIM));
            ui.add(
                egui::Slider::new(&mut self.level_db, dsp::LEVEL_MIN_DB..=dsp::LEVEL_CAP_DB)
                    .fixed_decimals(0)
                    .suffix(" dBFS"),
            )
            .on_hover_text("Preview level. Capped at -6 dBFS to protect your hearing and sensitive IEMs.");

            let mut hz = self.cursor_hz;
            let hz_drag = DragValue::new(&mut hz)
                .range(FREQ_MIN..=FREQ_MAX)
                .speed(self.cursor_hz * 0.004)
                .custom_formatter(|n, _| format_hz(n))
                .custom_parser(|s| {
                    let clean = s.trim().replace([',', ' '], "");
                    let stripped = clean.strip_suffix("Hz").or_else(|| clean.strip_suffix("hz")).unwrap_or(&clean);
                    stripped.parse::<f64>().ok()
                });
            let hz_resp = ui
                .scope(|ui| {
                    ui.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
                    ui.add_sized(egui::vec2(102.0, 24.0), hz_drag)
                })
                .inner
                .on_hover_text("Sweep cursor frequency. Drag or click to type an exact Hz value.");
            if hz_resp.changed() {
                self.selected = None;
                self.cursor_hz = hz;
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let can_add = self.workspace.bands.len() < MAX_BANDS;
                let add = ui.add_enabled(can_add, Button::new("+ Add Band"));
                if add.on_hover_text("Add a peaking band at the cursor frequency (A)").clicked() {
                    self.add_band();
                }
                ui.separator();

                let preamp = self.workspace.preamp_db();
                let mut auto = self.workspace.auto_preamp;
                let toggle = ui.checkbox(&mut auto, "Auto").on_hover_text(
                    "Auto-Preamp: lower the level by the true peak of the combined response (plus 0.2 dB) so boosts never clip.",
                );
                if toggle.changed() {
                    // Start manual adjustment from the value that was in effect.
                    self.workspace.manual_preamp = preamp;
                    self.workspace.auto_preamp = auto;
                }
                if self.workspace.auto_preamp {
                    ui.label(RichText::new(format!("{preamp:.1} dB")).monospace());
                } else {
                    ui.scope(|ui| {
                        ui.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
                        ui.add(
                            DragValue::new(&mut self.workspace.manual_preamp)
                                .range(-30.0..=30.0)
                                .speed(0.1)
                                .fixed_decimals(1)
                                .suffix(" dB"),
                        );
                    });
                }
                ui.label(RichText::new("Preamp").color(theme::TEXT_DIM));
                ui.separator();

                let mut bypass = Button::new(RichText::new("Bypass").strong()).min_size(egui::vec2(84.0, 28.0));
                if self.bypass {
                    bypass = Button::new(RichText::new("Bypassed").strong().color(theme::ON_ACCENT))
                        .min_size(egui::vec2(84.0, 28.0))
                        .fill(theme::WARN);
                }
                if ui.add(bypass).on_hover_text("Hear the sweep without EQ, for A/B comparison (B)").clicked() {
                    self.bypass = !self.bypass;
                }
            });
        });
    }

    // --- Zone 4: band table -------------------------------------------------------

    fn band_table(&mut self, ui: &mut Ui) {
        if self.workspace.bands.is_empty() {
            ui.add_space(18.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No bands yet").color(theme::TEXT_DIM));
                ui.label(
                    RichText::new("Sweep to a problem frequency, then press A to add a band there.").color(theme::TEXT_DIM),
                );
            });
            return;
        }

        let mut select = None;
        let mut delete = None;
        let mut sort = false;

        // Rows span the window: fixed columns for the controls, and the
        // three numeric columns share whatever width is left.
        const ROW_H: f32 = 30.0;
        const FIXED: [f32; 3] = [46.0, 46.0, 150.0];
        const DELETE_W: f32 = 44.0;
        let columns = |row: Rect| -> [Rect; 7] {
            let flex = ((row.width() - FIXED.iter().sum::<f32>() - DELETE_W) / 3.0).max(60.0);
            let widths = [FIXED[0], FIXED[1], FIXED[2], flex, flex, flex, DELETE_W];
            let mut x = row.left();
            widths.map(|w| {
                let rect = Rect::from_min_size(egui::pos2(x, row.top()), egui::vec2(w, row.height())).shrink2(egui::vec2(8.0, 0.0));
                x += w;
                rect
            })
        };
        let cell = |ui: &mut Ui, rect: Rect, salt: (usize, usize)| {
            ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)).id_salt(salt))
        };

        let (header, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), Sense::hover());
        let cols = columns(header);
        for (col, title) in [(0, "#"), (1, "On"), (2, "Type"), (3, "Frequency (Hz)"), (4, "Gain (dB)"), (5, "Q Factor")] {
            let mut ui = cell(ui, cols[col], (usize::MAX, col));
            ui.label(RichText::new(title).color(theme::TEXT_DIM).size(10.5));
            if col == 3 {
                sort = ui.small_button("Sort").on_hover_text("Sort bands by frequency").clicked();
            }
        }
        ui.painter().hline(header.x_range(), header.bottom(), egui::Stroke::new(1.0, theme::HAIRLINE));

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (i, band) in self.workspace.bands.iter_mut().enumerate() {
                let (row, row_response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
                let is_selected = self.selected == Some(i);
                let fill = match (is_selected, row_response.hovered()) {
                    (true, _) => theme::ROW_SELECTED,
                    (false, true) => theme::ROW_HOVER,
                    (false, false) if i % 2 == 1 => theme::ROW_ALT,
                    _ => Color32::TRANSPARENT,
                };
                ui.painter().rect_filled(row, 5.0, fill);
                if is_selected {
                    let bar = Rect::from_min_size(row.min, egui::vec2(3.0, row.height())).shrink2(egui::vec2(0.0, 6.0));
                    ui.painter().rect_filled(bar, 1.5, theme::ACCENT);
                }
                if row_response.clicked() {
                    select = Some(i);
                }
                let cols = columns(row);
                // A disabled band stays editable but reads as inactive.
                let opacity = if band.enabled { 1.0 } else { 0.4 };
                let mut edited = false;

                let mut c = cell(ui, cols[0], (i, 0));
                let number = RichText::new((i + 1).to_string()).monospace().color(if is_selected { theme::ACCENT } else { theme::TEXT_DIM });
                c.label(number);

                let mut c = cell(ui, cols[1], (i, 1));
                c.checkbox(&mut band.enabled, "").on_hover_text("Enable or disable this band");

                let mut c = cell(ui, cols[2], (i, 2));
                c.multiply_opacity(opacity);
                ComboBox::from_id_salt(("type", i)).width(cols[2].width()).selected_text(band.kind.name()).show_ui(&mut c, |ui| {
                    for kind in FilterType::ALL {
                        edited |= ui.selectable_value(&mut band.kind, kind, kind.name()).changed();
                    }
                });

                let field = |c: &Ui, max_w: f32| egui::vec2(c.available_width().min(max_w), 22.0);
                let mut c = cell(ui, cols[3], (i, 3));
                c.multiply_opacity(opacity);
                c.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
                let speed = band.freq * 0.004;
                let freq = DragValue::new(&mut band.freq)
                    .range(FREQ_MIN..=FREQ_MAX)
                    .speed(speed)
                    .custom_formatter(|n, _| preset::num(n, 1));
                edited |= c.add_sized(field(&c, 136.0), freq).changed();

                let mut c = cell(ui, cols[4], (i, 4));
                c.multiply_opacity(opacity);
                c.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
                let has_gain = band.kind.has_gain();
                let gain = DragValue::new(&mut band.gain)
                    .range(-30.0..=30.0)
                    .speed(0.05)
                    .custom_formatter(|n, _| {
                        if n > 0.05 {
                            format!("+{n:.1}")
                        } else if n < -0.05 {
                            format!("{n:.1}")
                        } else {
                            "0.0".into()
                        }
                    });
                edited |= c.add_enabled_ui(has_gain, |c| c.add_sized(field(c, 110.0), gain).changed()).inner;

                let mut c = cell(ui, cols[5], (i, 5));
                c.multiply_opacity(opacity);
                c.style_mut().override_text_style = Some(egui::TextStyle::Monospace);
                let q = DragValue::new(&mut band.q).range(Q_MIN..=Q_MAX).speed(0.01).fixed_decimals(2);
                edited |= c.add_sized(field(&c, 100.0), q).changed();

                let mut c = cell(ui, cols[6], (i, 6));
                let remove = Button::new(RichText::new("×").size(16.0).color(theme::TEXT_DIM)).frame(false);
                if c.add(remove).on_hover_text("Delete band").clicked() {
                    delete = Some(i);
                }

                if edited {
                    select = Some(i);
                }
            }
        });

        if let Some(i) = select {
            self.select(i);
        }
        if let Some(i) = delete {
            self.delete_band(i);
        }
        if sort {
            self.sort_bands();
        }
    }

    // --- Footer: context-sensitive shortcut hints & transient feedback ----------

    fn footer_bar(&mut self, ui: &mut Ui) {
        let hint = |ui: &mut Ui, key: &str, desc: &str| {
            ui.label(RichText::new(key).monospace().size(11.0).color(theme::TEXT));
            ui.label(RichText::new(desc).size(11.0).color(theme::TEXT_DIM));
        };
        let sep = |ui: &mut Ui| {
            ui.label(RichText::new("·").size(11.0).color(theme::ZERO_LINE));
        };

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            match self.selected {
                Some(i) => {
                    ui.label(RichText::new(format!("Band {}", i + 1)).strong().size(11.0).color(theme::ACCENT));
                    sep(ui);
                    hint(ui, "h/l", "Freq");
                    sep(ui);
                    hint(ui, "j/k", "Gain");
                    sep(ui);
                    hint(ui, "[/]", "Q");
                    sep(ui);
                    hint(ui, "m", "Mute");
                    sep(ui);
                    hint(ui, "d", "Delete");
                    sep(ui);
                    hint(ui, "Esc", "Free sweep");
                    sep(ui);
                    hint(ui, "Space", "Sweep");
                    sep(ui);
                    hint(ui, "b", "Bypass");
                    sep(ui);
                    hint(ui, "Shift/Alt", "Coarse/Fine");
                }
                None => {
                    hint(ui, "h/l", "Sweep");
                    sep(ui);
                    hint(ui, "a", "Add band");
                    sep(ui);
                    hint(ui, "Space", "Play/Stop");
                    sep(ui);
                    hint(ui, "b", "Bypass");
                    sep(ui);
                    hint(ui, "1-9 / Tab", "Select band");
                    sep(ui);
                    hint(ui, "Shift/Alt", "Coarse/Fine");
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.restart_rx.is_some() {
                    ui.label(RichText::new("Restarting PipeWire…").size(11.0).color(theme::ACCENT));
                } else if let Some((msg, at)) = self.flash {
                    if let Some(remaining) = FLASH_DURATION.checked_sub(at.elapsed()) {
                        ui.label(RichText::new(msg).size(11.0).color(theme::ACCENT));
                        ui.ctx().request_repaint_after(remaining);
                    } else {
                        self.flash = None;
                    }
                }
            });
        });
    }

    // --- Dialogs ----------------------------------------------------------------

    fn show_modal(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.modal.take() else { return };
        let mut close = false;
        let response = egui::Modal::new(egui::Id::unique("otis-modal")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            match &mut modal {
                Modal::ConfirmRestart { action, dont_show_again } => {
                    ui.heading(if *action == RestartAction::Apply { "Apply to PipeWire" } else { "Disable EQ" });
                    ui.label(
                        "PipeWire will be restarted. Anything that is playing or recording will be \
                         interrupted for a moment, and some apps may need their stream restarted.",
                    );
                    ui.checkbox(dont_show_again, "Do not show again");
                    ui.horizontal(|ui| {
                        if ui.button("Restart PipeWire").clicked() {
                            if *dont_show_again {
                                self.config.skip_restart_warning = true;
                                let _ = self.config.save();
                            }
                            self.restart(*action);
                            close = true;
                        }
                        close |= ui.button("Cancel").clicked();
                    });
                }
                Modal::Unsaved(_) => {
                    ui.heading("Unsaved changes");
                    ui.label(format!(
                        "\"{}\" has changes that have not been saved.",
                        self.preset.as_deref().unwrap_or("This workspace")
                    ));
                    ui.horizontal(|ui| {
                        let (save, discard) = (ui.button("Save").clicked(), ui.button("Discard").clicked());
                        close = save || discard || ui.button("Cancel").clicked();
                        if save || discard {
                            let Modal::Unsaved(pending) = std::mem::replace(&mut modal, Modal::Error(String::new())) else {
                                unreachable!()
                            };
                            if save { self.save(Some(pending)) } else { self.switch(pending) }
                        }
                    });
                }
                Modal::UnsavedApply => {
                    let name = self.preset.clone().unwrap_or_else(|| "this preset".into());
                    ui.heading(format!("Save changes to \"{name}\"?"));
                    ui.label(
                        "Applying without saving writes your edits to PipeWire now, but the hot-swap daemon \
                         loads saved presets and would revert unsaved tweaks if the device reconnects.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Save & Apply").clicked() {
                            close = true;
                            if self.save_as(&name) {
                                self.proceed_restart(RestartAction::Apply);
                            }
                        }
                        if ui.button("Apply Without Saving").clicked() {
                            close = true;
                            self.proceed_restart(RestartAction::Apply);
                        }
                        close |= ui.button("Cancel").clicked();
                    });
                }
                Modal::SaveAs { name, then } => {
                    ui.heading(if matches!(then, Some(Pending::Apply)) { "Name this preset to apply it" } else { "Save preset as" });
                    let field = ui.add(egui::TextEdit::singleline(name).hint_text("Preset name").desired_width(300.0));
                    field.request_focus();
                    let clean = store::sanitize_name(name);
                    if self.presets.contains(&clean) {
                        ui.label(RichText::new("A preset with this name exists and will be overwritten.").color(theme::WARN));
                    }
                    ui.horizontal(|ui| {
                        let enter = ui.input(|i| i.key_pressed(Key::Enter));
                        let save = ui.add_enabled(!clean.is_empty(), Button::new("Save")).clicked() || (enter && !clean.is_empty());
                        if save && self.save_as(&clean) {
                            if let Some(pending) = then.take() {
                                self.switch(pending);
                            }
                            close = true;
                        }
                        close |= ui.button("Cancel").clicked();
                    });
                }
                Modal::Error(message) => {
                    ui.heading("Something went wrong");
                    ui.label(RichText::new(message.as_str()).color(theme::DANGER));
                    close = ui.button("Close").clicked();
                }
            }
        });
        close |= response.should_close();
        // An action above may have opened a follow-up dialog; keep that one.
        if !close && self.modal.is_none() {
            self.modal = Some(modal);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.sync(&ctx);
        self.keyboard(&ctx);

        let frame = egui::Frame::new().fill(theme::WINDOW_BG).inner_margin(egui::Margin::symmetric(12, 8));
        egui::Panel::top("routing").frame(frame).show_separator_line(false).show(ui, |ui| {
            self.top_bar(ui);
            if let Some(problem) = self.pipewire_problem() {
                ui.label(RichText::new(problem).color(theme::DANGER));
            }
        });
        let footer_frame = egui::Frame::new().fill(theme::WINDOW_BG).inner_margin(egui::Margin::symmetric(12, 5));
        egui::Panel::bottom("footer").frame(footer_frame).show_separator_line(true).show(ui, |ui| {
            self.footer_bar(ui);
        });
        egui::Panel::bottom("bands").frame(frame).resizable(true).default_size(220.0).min_size(110.0).show(ui, |ui| {
            self.band_table(ui);
        });
        egui::Panel::bottom("sweep").frame(frame).show_separator_line(false).show(ui, |ui| self.sweep_strip(ui));
        egui::CentralPanel::default().frame(frame.inner_margin(egui::Margin::symmetric(12, 0))).show(ui, |ui| {
            let view = graph::View {
                workspace: &self.workspace,
                selected: self.selected,
                cursor_hz: self.cursor_hz,
                bypass: self.bypass,
            };
            match graph::show(ui, &view, &mut self.cursor_drag) {
                Some(graph::Event::SelectBand(i)) => self.select(i),
                Some(graph::Event::MoveCursor(freq)) => {
                    self.selected = None;
                    self.cursor_hz = freq;
                }
                None => {}
            }
        });

        self.show_modal(&ctx);
        self.track_history(&ctx);
        self.drive_preview(&ctx);

        // Tab is ours (band cycling); do not let it leave a widget focused.
        if std::mem::take(&mut self.drop_focus) {
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_state();
    }
}