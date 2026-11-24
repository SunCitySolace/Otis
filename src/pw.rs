//! The background PipeWire thread.
//!
//! One thread owns the PipeWire connection. It watches the graph (hardware
//! sinks, the Otis EQ node and its tags, the default sink), plays the sweep
//! preview, and reconnects by itself after PipeWire restarts. The rest of
//! the program sees a plain [`Graph`] snapshot and sends [`Cmd`]s.

use crate::dsp::{Engine, PREVIEW_RATE, PreviewParams};
use crate::pwconf::{KEY_HASH, KEY_TARGET, SINK_NAME};
use pipewire as pw;
use pw::proxy::Listener;
use pw::types::ObjectType;
use pw::{properties::properties, spa};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Oldest PipeWire with the builtin `param_eq` node.
pub const MIN_VERSION: (u32, u32) = (1, 4);

/// How long a lost connection is treated as "PipeWire is restarting".
const RESTART_GRACE: Duration = Duration::from_secs(5);
const RETRY_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, PartialEq, Debug)]
pub struct Sink {
    pub name: String,
    pub description: String,
}

/// The live `otis.managed` node and its tags.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct OtisNode {
    pub hash: Option<String>,
    pub target: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum Conn {
    /// Not connected yet, or reconnecting after a restart.
    #[default]
    Connecting,
    Connected,
    /// Retries have been failing for longer than the restart grace period.
    Unavailable,
}

/// Ground truth read from PipeWire.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Graph {
    pub conn: Conn,
    /// The initial enumeration after (re)connecting has completed.
    pub settled: bool,
    pub version: Option<String>,
    /// Physical output sinks; virtual sinks and Otis EQ are excluded.
    pub sinks: Vec<Sink>,
    pub otis: Option<OtisNode>,
    /// `node.name` of the current default sink.
    pub default_sink: Option<String>,
}

impl Graph {
    pub fn sink(&self, name: &str) -> Option<&Sink> {
        self.sinks.iter().find(|s| s.name == name)
    }

    /// False only when a version is known and it is too old.
    pub fn version_ok(&self) -> bool {
        let Some(version) = &self.version else { return true };
        let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= MIN_VERSION
    }

    pub fn otis_is_default(&self) -> bool {
        self.default_sink.as_deref() == Some(SINK_NAME)
    }
}

enum Cmd {
    /// Desired preview stream: which hardware sink, and whether it runs.
    Preview { target: Option<String>, on: bool },
    SetDefault(String),
}

struct Shared {
    graph: Mutex<Graph>,
    /// Writer half of a freshly created preview stream's parameter buffer,
    /// waiting to be picked up by the UI thread.
    input: Mutex<Option<triple_buffer::Input<PreviewParams>>>,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn set_graph(&self, graph: Graph) {
        let mut current = self.graph.lock().unwrap();
        if *current != graph {
            *current = graph;
            drop(current);
            (self.notify)();
        }
    }
}

pub struct Handle {
    tx: pw::channel::Sender<Cmd>,
    shared: Arc<Shared>,
}

impl Handle {
    /// Start the PipeWire thread. `notify` is called (from that thread)
    /// whenever the graph snapshot changes.
    pub fn spawn(notify: impl Fn() + Send + Sync + 'static) -> Handle {
        let shared = Arc::new(Shared { graph: Mutex::default(), input: Mutex::new(None), notify: Box::new(notify) });
        let (tx, rx) = pw::channel::channel();
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("otis-pipewire".into())
            .spawn(move || run(thread_shared, rx))
            .expect("failed to spawn the PipeWire thread");
        Handle { tx, shared }
    }

    pub fn graph(&self) -> Graph {
        self.shared.graph.lock().unwrap().clone()
    }

    /// The parameter writer of a new preview stream, if one was just created.
    pub fn take_input(&self) -> Option<triple_buffer::Input<PreviewParams>> {
        self.shared.input.lock().unwrap().take()
    }

    pub fn preview(&self, target: Option<String>, on: bool) {
        let _ = self.tx.send(Cmd::Preview { target, on });
    }

    pub fn set_default_sink(&self, node_name: &str) {
        let _ = self.tx.send(Cmd::SetDefault(node_name.to_string()));
    }
}

/// What the commands have asked for; survives reconnects so the preview
/// resumes by itself once PipeWire is back.
#[derive(Default)]
struct Want {
    target: Option<String>,
    on: bool,
}

fn run(shared: Arc<Shared>, rx: pw::channel::Receiver<Cmd>) {
    pw::init();
    let want = Rc::new(RefCell::new(Want::default()));
    let mut rx = Some(rx);
    let mut down_since: Option<Instant> = None;
    loop {
        if session(&shared, &want, &mut rx).is_ok() {
            // Was connected and lost it: PipeWire is most likely restarting.
            down_since = Some(Instant::now());
        }
        let restarting = down_since.get_or_insert_with(Instant::now).elapsed() < RESTART_GRACE;
        let conn = if restarting { Conn::Connecting } else { Conn::Unavailable };
        shared.set_graph(Graph { conn, ..Graph::default() });
        std::thread::sleep(if restarting { RETRY_INTERVAL } else { Duration::from_secs(1) });
    }
}

enum NodeEntry {
    Hardware(Sink),
    Otis(OtisNode),
}

struct Preview {
    target: String,
    stream: pw::stream::StreamRc,
    active: bool,
    _listener: pw::stream::StreamListener<AudioState>,
}

/// State owned by the real-time audio callback.
struct AudioState {
    params: triple_buffer::Output<PreviewParams>,
    engine: Engine,
}

/// A bound proxy and the listener registered on it.
type BoundProxy = (Box<dyn pw::proxy::ProxyT>, Box<dyn Listener>);

struct Session {
    shared: Arc<Shared>,
    want: Rc<RefCell<Want>>,
    core: pw::core::CoreRc,
    version: Option<String>,
    settled: bool,
    nodes: HashMap<u32, NodeEntry>,
    /// Proxies we listen on, keyed by global id; dropped when the global goes.
    bound: HashMap<u32, BoundProxy>,
    metadata: Option<(u32, pw::metadata::Metadata, pw::metadata::MetadataListener)>,
    default_sink: Option<String>,
    /// A default-sink request that arrived before the metadata object.
    pending_default: Option<String>,
    preview: Option<Preview>,
}

impl Session {
    fn publish(&self) {
        let mut sinks = Vec::new();
        let mut otis = None;
        for entry in self.nodes.values() {
            match entry {
                NodeEntry::Hardware(sink) => sinks.push(sink.clone()),
                NodeEntry::Otis(node) => otis = Some(node.clone()),
            }
        }
        sinks.sort_by(|a, b| (&a.description, &a.name).cmp(&(&b.description, &b.name)));
        self.shared.set_graph(Graph {
            conn: Conn::Connected,
            settled: self.settled,
            version: self.version.clone(),
            sinks,
            otis,
            default_sink: self.default_sink.clone(),
        });
    }

    fn set_default(&mut self, node_name: String) {
        match &self.metadata {
            // The "configured" key is the user's choice; the session manager
            // turns it into the effective default.
            Some((_, metadata, _)) => metadata.set_property(
                0,
                "default.configured.audio.sink",
                Some("Spa:String:JSON"),
                Some(&format!("{{\"name\":\"{node_name}\"}}")),
            ),
            None => self.pending_default = Some(node_name),
        }
    }

    /// Bring the preview stream in line with `want`.
    fn sync_preview(&mut self) {
        let (target, on) = {
            let want = self.want.borrow();
            (want.target.clone(), want.on)
        };
        if self.preview.as_ref().is_some_and(|p| Some(&p.target) != target.as_ref()) {
            self.preview = None;
        }
        match (&mut self.preview, target) {
            (Some(preview), _) if preview.active != on => {
                preview.active = preview.stream.set_active(on).is_ok() == on;
            }
            (None, Some(target)) if on => match create_preview(&self.core, &target) {
                Ok((preview, input)) => {
                    self.preview = Some(preview);
                    *self.shared.input.lock().unwrap() = Some(input);
                    (self.shared.notify)();
                }
                Err(e) => eprintln!("otis: could not start the preview stream: {e}"),
            },
            _ => {}
        }
    }
}

/// Run one connection until it is lost. `Err` means it never came up.
fn session(
    shared: &Arc<Shared>,
    want: &Rc<RefCell<Want>>,
    rx: &mut Option<pw::channel::Receiver<Cmd>>,
) -> Result<(), pw::Error> {
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    let session = Rc::new(RefCell::new(Session {
        shared: shared.clone(),
        want: want.clone(),
        core: core.clone(),
        version: None,
        settled: false,
        nodes: HashMap::new(),
        bound: HashMap::new(),
        metadata: None,
        default_sink: None,
        pending_default: None,
        preview: None,
    }));

    // Two round trips: the first delivers every global, the second the
    // info/property events of the proxies bound while handling those.
    let syncs_left = Rc::new(std::cell::Cell::new(2));
    core.sync(0)?;

    let _core_listener = core
        .add_listener_local()
        .info({
            let session = session.clone();
            move |info| {
                let mut s = session.borrow_mut();
                s.version = Some(info.version().to_string());
                s.publish();
            }
        })
        .done({
            let (session, core, syncs_left) = (session.clone(), core.clone(), syncs_left.clone());
            move |id, _seq| {
                if id != pw::core::PW_ID_CORE || syncs_left.get() == 0 {
                    return;
                }
                syncs_left.set(syncs_left.get() - 1);
                if syncs_left.get() > 0 {
                    let _ = core.sync(0);
                } else {
                    let mut s = session.borrow_mut();
                    s.settled = true;
                    s.publish();
                }
            }
        })
        .error({
            let mainloop = mainloop.downgrade();
            move |id, _seq, _res, _message| {
                // An error on the core itself means the connection is gone.
                if id == pw::core::PW_ID_CORE
                    && let Some(mainloop) = mainloop.upgrade()
                {
                    mainloop.quit();
                }
            }
        })
        .register();

    let _registry_listener = registry
        .add_listener_local()
        .global({
            let (weak, registry) = (Rc::downgrade(&session), registry.downgrade());
            move |global| {
                let (Some(session), Some(registry), Some(props)) = (weak.upgrade(), registry.upgrade(), global.props)
                else {
                    return;
                };
                let mut s = session.borrow_mut();
                match global.type_ {
                    ObjectType::Node if props.get("media.class") == Some("Audio/Sink") => {
                        let name = props.get("node.name").unwrap_or_default();
                        if name == SINK_NAME {
                            // Our tags are not part of the global's props;
                            // they arrive with the node info.
                            let Ok(node) = registry.bind::<pw::node::Node, _>(global) else { return };
                            let (weak, id) = (weak.clone(), global.id);
                            let listener = node
                                .add_listener_local()
                                .info(move |info| {
                                    let (Some(session), Some(props)) = (weak.upgrade(), info.props()) else { return };
                                    let mut s = session.borrow_mut();
                                    let tags = OtisNode {
                                        hash: props.get(KEY_HASH).map(str::to_string),
                                        target: props.get(KEY_TARGET).map(str::to_string),
                                    };
                                    s.nodes.insert(id, NodeEntry::Otis(tags));
                                    s.publish();
                                })
                                .register();
                            s.bound.insert(global.id, (Box::new(node), Box::new(listener)));
                            s.nodes.insert(global.id, NodeEntry::Otis(OtisNode::default()));
                        } else if props.get("device.id").is_some() && props.get("node.virtual") != Some("true") {
                            let description = props.get("node.description").unwrap_or(name).to_string();
                            s.nodes.insert(global.id, NodeEntry::Hardware(Sink { name: name.to_string(), description }));
                        }
                        s.publish();
                    }
                    ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                        let Ok(metadata) = registry.bind::<pw::metadata::Metadata, _>(global) else { return };
                        let weak = weak.clone();
                        let listener = metadata
                            .add_listener_local()
                            .property(move |subject, key, _type, value| {
                                if subject == 0
                                    && key == Some("default.audio.sink")
                                    && let Some(session) = weak.upgrade()
                                {
                                    let mut s = session.borrow_mut();
                                    s.default_sink = value.and_then(json_name);
                                    s.publish();
                                }
                                0
                            })
                            .register();
                        s.metadata = Some((global.id, metadata, listener));
                        if let Some(name) = s.pending_default.take() {
                            s.set_default(name);
                        }
                    }
                    _ => {}
                }
            }
        })
        .global_remove({
            let weak = Rc::downgrade(&session);
            move |id| {
                let Some(session) = weak.upgrade() else { return };
                let mut s = session.borrow_mut();
                s.bound.remove(&id);
                if s.metadata.as_ref().is_some_and(|(metadata_id, ..)| *metadata_id == id) {
                    s.metadata = None;
                    s.default_sink = None;
                }
                if s.nodes.remove(&id).is_some() {
                    s.publish();
                }
            }
        })
        .register();

    let receiver = rx.take().expect("command receiver is always returned");
    let attached = receiver.attach(mainloop.loop_(), {
        let session = session.clone();
        move |cmd| {
            let mut s = session.borrow_mut();
            match cmd {
                Cmd::Preview { target, on } => {
                    *s.want.borrow_mut() = Want { target, on };
                    s.sync_preview();
                }
                Cmd::SetDefault(name) => s.set_default(name),
            }
        }
    });

    session.borrow_mut().sync_preview();
    session.borrow().publish();
    mainloop.run();

    *rx = Some(attached.deattach());
    Ok(())
}

/// Create the sweep stream, pinned to the hardware sink `target` so it
/// bypasses the Otis EQ virtual sink (no double EQ while a config is live).
fn create_preview(
    core: &pw::core::CoreRc,
    target: &str,
) -> Result<(Preview, triple_buffer::Input<PreviewParams>), pw::Error> {
    let (input, output) = triple_buffer::triple_buffer(&PreviewParams::default());
    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "otis-preview",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::APP_NAME => "Otis",
            *pw::keys::NODE_NAME => "otis_preview",
            *pw::keys::NODE_DESCRIPTION => "Otis sweep preview",
            *pw::keys::NODE_LATENCY => "512/48000",
            *pw::keys::TARGET_OBJECT => target,
            *pw::keys::NODE_DONT_RECONNECT => "true",
        },
    )?;

    const STRIDE: usize = 2 * size_of::<f32>();
    let listener = stream
        .add_local_listener_with_user_data(AudioState { params: output, engine: Engine::default() })
        .process(|stream, state| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let requested = buffer.requested() as usize;
            let Some(data) = buffer.datas_mut().first_mut() else { return };
            let params = *state.params.read();
            let mut frames = 0;
            if let Some(bytes) = data.data() {
                frames = bytes.len() / STRIDE;
                if requested > 0 {
                    frames = frames.min(requested);
                }
                if state.engine.silent(&params) {
                    bytes[..frames * STRIDE].fill(0);
                } else {
                    for frame in bytes[..frames * STRIDE].as_chunks_mut::<STRIDE>().0 {
                        let sample = state.engine.next(&params).to_le_bytes();
                        frame[..4].copy_from_slice(&sample);
                        frame[4..].copy_from_slice(&sample);
                    }
                }
            }
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = STRIDE as i32;
            *chunk.size_mut() = (frames * STRIDE) as u32;
        })
        .register()?;

    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(PREVIEW_RATE as u32);
    info.set_channels(2);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let format = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Format,
            id: spa::sys::SPA_PARAM_EnumFormat,
            properties: info.into(),
        }),
    )
    .map_err(|_| pw::Error::CreationFailed)?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&format).ok_or(pw::Error::CreationFailed)?];

    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;
    Ok((Preview { target: target.to_string(), stream, active: true, _listener: listener }, input))
}

/// Extract `name` from metadata values like `{"name":"alsa_output.x"}`.
fn json_name(value: &str) -> Option<String> {
    let rest = value.split_once("\"name\"")?.1;
    let rest = rest.split_once('"')?.1;
    Some(rest.split_once('"')?.0.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_sink_metadata() {
        assert_eq!(json_name(r#"{"name":"alsa_output.usb-dac.analog-stereo"}"#).as_deref(), Some("alsa_output.usb-dac.analog-stereo"));
        assert_eq!(json_name(r#"{ "name": "otis_eq" }"#).as_deref(), Some("otis_eq"));
        assert_eq!(json_name("{}"), None);
    }

    #[test]
    fn version_gate() {
        let graph = |v: &str| Graph { version: Some(v.into()), ..Default::default() };
        assert!(graph("1.4.0").version_ok() && graph("1.6.11").version_ok() && graph("2.0.0").version_ok());
        assert!(!graph("1.0.5").version_ok() && !graph("0.3.80").version_ok());
        assert!(Graph::default().version_ok());
    }
}
