//! Native PipeWire backend through the `pipewire` crate (`libpipewire-0.3`).
//!
//! A short-lived client connection is opened for every operation. Registry
//! globals provide sinks, playback streams, ports and links. Moving a stream
//! means creating links from the stream's output ports to the target sink's
//! input ports and then destroying the old links; like `pw-link`, this works
//! with WirePlumber without going through PulseAudio.
//!
//! The default sink and metadata are read from the `default` metadata object;
//! per-stream volume/mute are set through the node's `Props` parameter.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use pipewire as pw;
use pw::spa;
use spa::pod::Pod;

use super::{AudioBackend, BackendKind};
use crate::{LinkInfo, Sink, Source, Stream, resolve_sink};

#[derive(Debug, Default, Clone)]
struct RawNode {
    props: BTreeMap<String, String>,
    volume: Option<f32>,
    mute: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawPort {
    id: u32,
    node_id: u32,
    direction: String,
    name: String,
    channel: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawLink {
    id: u32,
    output_node: u32,
    output_port: u32,
    input_node: u32,
    input_port: u32,
}

#[derive(Debug, Default)]
struct State {
    nodes: HashMap<u32, RawNode>,
    ports: Vec<RawPort>,
    links: Vec<RawLink>,
    metadata: HashMap<u32, BTreeMap<String, String>>,
    default_metadata: Option<u32>,
    link_factory: Option<String>,
}

#[derive(Debug, Clone)]
struct RawSnapshot {
    nodes: HashMap<u32, RawNode>,
    ports: Vec<RawPort>,
    links: Vec<RawLink>,
    metadata: HashMap<u32, BTreeMap<String, String>>,
    default_metadata: Option<u32>,
    link_factory: Option<String>,
}

impl RawSnapshot {
    fn sinks(&self) -> Vec<Sink> {
        let mut sinks: Vec<Sink> = self
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.props
                    .get("media.class")
                    .is_some_and(|class| class.starts_with("Audio/Sink"))
            })
            .map(|(id, node)| {
                let name = node
                    .props
                    .get("node.name")
                    .cloned()
                    .unwrap_or_else(|| format!("sink {id}"));
                let description = node
                    .props
                    .get("node.description")
                    .or_else(|| node.props.get("node.nick"))
                    .cloned()
                    .unwrap_or_else(|| name.clone());
                Sink {
                    id: *id,
                    name,
                    description,
                    volume: node.volume,
                    mute: node.mute,
                }
            })
            .collect();
        sinks.sort_by_key(|sink| sink.id);
        sinks
    }

    fn sources(&self) -> Vec<Source> {
        let mut sources: Vec<Source> = self
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.props
                    .get("media.class")
                    .is_some_and(|class| class.starts_with("Audio/Source"))
            })
            .map(|(id, node)| {
                let name = node
                    .props
                    .get("node.name")
                    .cloned()
                    .unwrap_or_else(|| format!("source {id}"));
                let description = node
                    .props
                    .get("node.description")
                    .or_else(|| node.props.get("node.nick"))
                    .cloned()
                    .unwrap_or_else(|| name.clone());
                Source {
                    id: *id,
                    name,
                    description,
                    volume: node.volume,
                    mute: node.mute,
                }
            })
            .collect();
        sources.sort_by_key(|source| source.id);
        sources
    }

    fn streams(&self) -> Vec<Stream> {
        let sink_ids: HashSet<u32> = self.sinks().into_iter().map(|sink| sink.id).collect();
        let mut streams: Vec<Stream> = self
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.props.get("media.class").map(String::as_str) == Some("Stream/Output/Audio")
            })
            .map(|(id, node)| {
                let app = [
                    "application.name",
                    "application.process.binary",
                    "media.name",
                ]
                .iter()
                .find_map(|key| node.props.get(*key))
                .or_else(|| node.props.get("node.name"))
                .cloned()
                .unwrap_or_else(|| format!("stream {id}"));
                let binary = node.props.get("application.process.binary").cloned();
                let media_name = node.props.get("media.name").cloned().unwrap_or_default();
                let sink_id = self
                    .links
                    .iter()
                    .filter(|link| link.output_node == *id && sink_ids.contains(&link.input_node))
                    .map(|link| link.input_node)
                    .min();
                Stream {
                    id: *id,
                    sink_id,
                    app,
                    binary,
                    media_name,
                    volume: node.volume,
                    mute: node.mute,
                }
            })
            .collect();
        streams.sort_by_key(|stream| stream.id);
        streams
    }

    fn default_sink(&self) -> Option<String> {
        self.metadata_name("default.audio.sink")
    }

    fn default_source(&self) -> Option<String> {
        self.metadata_name("default.audio.source")
            .or_else(|| self.metadata_name("default.configured.audio.source"))
    }

    fn metadata_name(&self, key: &str) -> Option<String> {
        let metadata = self.metadata.get(&self.default_metadata?)?;
        let value = metadata.get(key)?;
        let parsed: serde_json::Value = serde_json::from_str(value).ok()?;
        parsed
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }

    fn links(&self) -> Vec<LinkInfo> {
        self.links
            .iter()
            .map(|link| LinkInfo {
                id: link.id,
                output_node: link.output_node,
                input_node: link.input_node,
            })
            .collect()
    }
}

struct Session {
    // Declared in drop order: listeners before proxies before the core objects.
    _listeners: Rc<RefCell<Vec<Box<dyn pw::proxy::Listener>>>>,
    node_proxies: Rc<RefCell<HashMap<u32, pw::node::Node>>>,
    metadata_proxies: Rc<RefCell<HashMap<u32, pw::metadata::Metadata>>>,
    _registry_listener: pw::registry::Listener,
    registry: pw::registry::RegistryRc,
    core: pw::core::CoreRc,
    _context: pw::context::ContextRc,
    mainloop: pw::main_loop::MainLoopRc,
    state: Rc<RefCell<State>>,
}

impl Session {
    fn open() -> Result<Self, String> {
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None)
            .map_err(|err| format!("cannot create PipeWire main loop: {err}"))?;
        let context = pw::context::ContextRc::new(&mainloop, None)
            .map_err(|err| format!("cannot create PipeWire context: {err}"))?;
        let core = context
            .connect_rc(None)
            .map_err(|err| format!("cannot connect to PipeWire: {err}"))?;
        let registry = core
            .get_registry_rc()
            .map_err(|err| format!("cannot get PipeWire registry: {err}"))?;

        let state: Rc<RefCell<State>> = Rc::new(RefCell::new(State::default()));
        let node_proxies: Rc<RefCell<HashMap<u32, pw::node::Node>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let metadata_proxies: Rc<RefCell<HashMap<u32, pw::metadata::Metadata>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let listeners: Rc<RefCell<Vec<Box<dyn pw::proxy::Listener>>>> =
            Rc::new(RefCell::new(Vec::new()));

        let registry_weak = registry.downgrade();
        let state_cb = state.clone();
        let nodes_cb = node_proxies.clone();
        let metadata_cb = metadata_proxies.clone();
        let listeners_cb = listeners.clone();
        let registry_listener = registry
            .add_listener_local()
            .global(move |global| {
                let Some(registry) = registry_weak.upgrade() else {
                    return;
                };
                let props = dict_to_map(global.props);
                match global.type_ {
                    pw::types::ObjectType::Node => {
                        let Ok(node) = registry.bind::<pw::node::Node, _>(global) else {
                            return;
                        };
                        let id = global.id;
                        let info_state = state_cb.clone();
                        let param_state = state_cb.clone();
                        let listener = node
                            .add_listener_local()
                            .info(move |info| {
                                let props = dict_to_map(info.props());
                                info_state.borrow_mut().nodes.entry(id).or_default().props = props;
                            })
                            .param(move |_seq, _type, _index, _next, param| {
                                let Some(pod) = param else {
                                    return;
                                };
                                let Some((volume, mute)) = parse_props_pod(pod) else {
                                    return;
                                };
                                let mut state = param_state.borrow_mut();
                                let node = state.nodes.entry(id).or_default();
                                if volume.is_some() {
                                    node.volume = volume;
                                }
                                if mute.is_some() {
                                    node.mute = mute;
                                }
                            })
                            .register();
                        listeners_cb.borrow_mut().push(Box::new(listener));
                        nodes_cb.borrow_mut().insert(id, node);
                    }
                    pw::types::ObjectType::Port => {
                        let Some(node_id) = props.get("node.id").and_then(|id| id.parse().ok())
                        else {
                            return;
                        };
                        state_cb.borrow_mut().ports.push(RawPort {
                            // `pw-cli create-link` and the link API use the
                            // global object id of the port, not `port.id`.
                            id: global.id,
                            node_id,
                            direction: props.get("port.direction").cloned().unwrap_or_default(),
                            name: props.get("port.name").cloned().unwrap_or_default(),
                            channel: props.get("audio.channel").cloned().unwrap_or_default(),
                        });
                    }
                    pw::types::ObjectType::Link => {
                        let Some(output_node) =
                            props.get("link.output.node").and_then(|v| v.parse().ok())
                        else {
                            return;
                        };
                        let Some(output_port) =
                            props.get("link.output.port").and_then(|v| v.parse().ok())
                        else {
                            return;
                        };
                        let Some(input_node) =
                            props.get("link.input.node").and_then(|v| v.parse().ok())
                        else {
                            return;
                        };
                        let Some(input_port) =
                            props.get("link.input.port").and_then(|v| v.parse().ok())
                        else {
                            return;
                        };
                        state_cb.borrow_mut().links.push(RawLink {
                            id: global.id,
                            output_node,
                            output_port,
                            input_node,
                            input_port,
                        });
                    }
                    pw::types::ObjectType::Factory => {
                        if props.get("factory.type.name").map(String::as_str)
                            == Some("PipeWire:Interface:Link")
                            && let Some(name) = props.get("factory.name")
                        {
                            state_cb.borrow_mut().link_factory = Some(name.clone());
                        }
                    }
                    pw::types::ObjectType::Metadata => {
                        let Ok(metadata) = registry.bind::<pw::metadata::Metadata, _>(global)
                        else {
                            return;
                        };
                        let id = global.id;
                        let metadata_state = state_cb.clone();
                        let listener = metadata
                            .add_listener_local()
                            .property(move |_subject, key, _type, value| {
                                if let (Some(key), Some(value)) = (key, value) {
                                    metadata_state
                                        .borrow_mut()
                                        .metadata
                                        .entry(id)
                                        .or_default()
                                        .insert(key.to_owned(), value.to_owned());
                                }
                                0
                            })
                            .register();
                        if props.get("metadata.name").map(String::as_str) == Some("default") {
                            state_cb.borrow_mut().default_metadata = Some(id);
                        }
                        listeners_cb.borrow_mut().push(Box::new(listener));
                        metadata_cb.borrow_mut().insert(id, metadata);
                    }
                    _ => {}
                }
            })
            .register();

        Ok(Self {
            _listeners: listeners,
            node_proxies,
            metadata_proxies,
            _registry_listener: registry_listener,
            registry,
            core,
            _context: context,
            mainloop,
            state,
        })
    }

    fn sync(&self) -> Result<(), String> {
        let done = Rc::new(Cell::new(false));
        let done_callback = done.clone();
        let mainloop = self.mainloop.clone();
        let pending = self
            .core
            .sync(0)
            .map_err(|err| format!("PipeWire sync failed: {err}"))?;
        let _listener = self
            .core
            .add_listener_local()
            .done(move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    done_callback.set(true);
                    mainloop.quit();
                }
            })
            .register();
        while !done.get() {
            self.mainloop.run();
        }
        Ok(())
    }

    fn snapshot(&self) -> Result<RawSnapshot, String> {
        self.sync()?;
        {
            let nodes = self.node_proxies.borrow();
            for node in nodes.values() {
                node.subscribe_params(&[spa::param::ParamType::Props]);
                node.enum_params(0, Some(spa::param::ParamType::Props), 0, 1);
            }
        }
        self.sync()?;

        let state = self.state.borrow();
        Ok(RawSnapshot {
            nodes: state.nodes.clone(),
            ports: state.ports.clone(),
            links: state.links.clone(),
            metadata: state.metadata.clone(),
            default_metadata: state.default_metadata,
            link_factory: state.link_factory.clone(),
        })
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NativeBackend;

impl NativeBackend {
    pub fn new() -> Self {
        Self
    }
}

impl AudioBackend for NativeBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Native
    }

    fn description(&self) -> String {
        "native libpipewire-0.3".to_owned()
    }

    fn sinks(&self) -> Result<Vec<Sink>, String> {
        Ok(Session::open()?.snapshot()?.sinks())
    }

    fn streams(&self) -> Result<Vec<Stream>, String> {
        Ok(Session::open()?.snapshot()?.streams())
    }

    fn sources(&self) -> Result<Vec<Source>, String> {
        Ok(Session::open()?.snapshot()?.sources())
    }

    fn default_sink(&self) -> Result<Option<String>, String> {
        Ok(Session::open()?.snapshot()?.default_sink())
    }

    fn default_source(&self) -> Result<Option<String>, String> {
        Ok(Session::open()?.snapshot()?.default_source())
    }

    fn links(&self) -> Result<Vec<LinkInfo>, String> {
        Ok(Session::open()?.snapshot()?.links())
    }

    fn move_stream(&self, stream_id: u32, sink: &str) -> Result<(), String> {
        let session = Session::open()?;
        let snapshot = session.snapshot()?;
        let sinks = snapshot.sinks();
        let sink =
            resolve_sink(&sinks, sink).ok_or_else(|| format!("sink `{sink}` does not resolve"))?;
        let sink_id = sink.id;
        let sink_name = sink.name.clone();

        let outputs: Vec<&RawPort> = snapshot
            .ports
            .iter()
            .filter(|port| port.node_id == stream_id && port.direction == "out")
            .collect();
        if outputs.is_empty() {
            return Err(format!("stream {stream_id} has no output ports"));
        }
        let inputs: Vec<&RawPort> = snapshot
            .ports
            .iter()
            .filter(|port| port.node_id == sink_id && port.direction == "in")
            .collect();
        if inputs.is_empty() {
            return Err(format!("sink `{sink_name}` has no input ports"));
        }

        let mut used_inputs = Vec::new();
        let mut planned = Vec::new();
        for (index, output) in outputs.iter().enumerate() {
            let input = inputs
                .iter()
                .find(|input| {
                    !used_inputs.contains(&input.id)
                        && !output.channel.is_empty()
                        && input.channel == output.channel
                })
                .or_else(|| inputs.iter().find(|input| !used_inputs.contains(&input.id)))
                .or_else(|| inputs.get(index))
                .ok_or_else(|| format!("sink `{sink_name}` has too few input ports"))?;
            used_inputs.push(input.id);
            let linked = snapshot.links.iter().any(|link| {
                link.output_node == stream_id
                    && link.output_port == output.id
                    && link.input_node == sink_id
            });
            if !linked {
                planned.push((*output, *input));
            }
        }

        let obsolete: Vec<u32> = snapshot
            .links
            .iter()
            .filter(|link| link.output_node == stream_id && link.input_node != sink_id)
            .map(|link| link.id)
            .collect();
        if planned.is_empty() && obsolete.is_empty() {
            return Ok(());
        }

        // Create the new links first; only then tear down the old paths.
        let factory = snapshot
            .link_factory
            .clone()
            .unwrap_or_else(|| "link-factory".to_owned());
        let mut created = Vec::new();
        for (output, input) in planned {
            let mut props = pw::properties::PropertiesBox::new();
            props.insert("link.output.node", stream_id.to_string());
            props.insert("link.output.port", output.id.to_string());
            props.insert("link.input.node", sink_id.to_string());
            props.insert("link.input.port", input.id.to_string());
            props.insert("object.linger", "1");
            let link: pw::link::Link = session
                .core
                .create_object(&factory, &props)
                .map_err(|err| format!("cannot link stream {stream_id} to `{sink_name}`: {err}"))?;
            created.push(link);
        }
        session.sync()?;

        for link_id in obsolete {
            session
                .registry
                .destroy_global(link_id)
                .into_result()
                .map_err(|err| format!("cannot remove old link {link_id}: {err}"))?;
        }
        session.sync()?;
        drop(created);
        Ok(())
    }

    fn set_default_sink(&self, sink: &str) -> Result<(), String> {
        self.set_default_metadata("default.audio.sink", sink)?;
        self.set_default_metadata("default.configured.audio.sink", sink)
    }

    fn set_default_source(&self, source: &str) -> Result<(), String> {
        self.set_default_metadata("default.audio.source", source)?;
        self.set_default_metadata("default.configured.audio.source", source)
    }

    fn set_stream_volume(&self, stream_id: u32, volume: f32) -> Result<(), String> {
        self.set_stream_props(stream_id, Some(volume), None)
    }

    fn set_stream_mute(&self, stream_id: u32, mute: bool) -> Result<(), String> {
        self.set_stream_props(stream_id, None, Some(mute))
    }

    fn set_sink_volume(&self, sink: &str, volume: f32) -> Result<(), String> {
        self.set_device_props(false, sink, Some(volume), None)
    }

    fn set_sink_mute(&self, sink: &str, mute: bool) -> Result<(), String> {
        self.set_device_props(false, sink, None, Some(mute))
    }

    fn set_source_volume(&self, source: &str, volume: f32) -> Result<(), String> {
        self.set_device_props(true, source, Some(volume), None)
    }

    fn set_source_mute(&self, source: &str, mute: bool) -> Result<(), String> {
        self.set_device_props(true, source, None, Some(mute))
    }
}

impl NativeBackend {
    fn set_default_metadata(&self, key: &str, value_name: &str) -> Result<(), String> {
        let session = Session::open()?;
        let snapshot = session.snapshot()?;
        let id = snapshot
            .default_metadata
            .ok_or_else(|| "PipeWire has no `default` metadata object".to_owned())?;
        let proxies = session.metadata_proxies.borrow();
        let metadata = proxies
            .get(&id)
            .ok_or_else(|| "cannot bind the `default` metadata object".to_owned())?;
        let value = serde_json::json!({ "name": value_name }).to_string();
        metadata.set_property(0, key, Some("Spa:String:JSON"), Some(&value));
        drop(proxies);
        session.sync()
    }

    fn set_stream_props(
        &self,
        stream_id: u32,
        volume: Option<f32>,
        mute: Option<bool>,
    ) -> Result<(), String> {
        let session = Session::open()?;
        let snapshot = session.snapshot()?;
        self.write_node_props(&session, &snapshot, stream_id, "out", volume, mute)
    }

    fn set_device_props(
        &self,
        source: bool,
        reference: &str,
        volume: Option<f32>,
        mute: Option<bool>,
    ) -> Result<(), String> {
        let session = Session::open()?;
        let snapshot = session.snapshot()?;
        let (node_id, direction) = if source {
            let sources = snapshot.sources();
            let found = crate::resolve_source(&sources, reference)
                .ok_or_else(|| format!("source `{reference}` does not resolve"))?;
            (found.id, "out")
        } else {
            let sinks = snapshot.sinks();
            let found = resolve_sink(&sinks, reference)
                .ok_or_else(|| format!("sink `{reference}` does not resolve"))?;
            (found.id, "in")
        };
        self.write_node_props(&session, &snapshot, node_id, direction, volume, mute)
    }

    fn write_node_props(
        &self,
        session: &Session,
        snapshot: &RawSnapshot,
        node_id: u32,
        direction: &str,
        volume: Option<f32>,
        mute: Option<bool>,
    ) -> Result<(), String> {
        let channels = snapshot
            .ports
            .iter()
            .filter(|port| port.node_id == node_id && port.direction == direction)
            .count()
            .max(1);
        let bytes = props_pod(volume, mute, channels)?;
        let pod = Pod::from_bytes(&bytes)
            .ok_or_else(|| "cannot serialize PipeWire properties".to_owned())?;
        let proxies = session.node_proxies.borrow();
        let node = proxies
            .get(&node_id)
            .ok_or_else(|| format!("audio node {node_id} is not present"))?;
        node.set_param(spa::param::ParamType::Props, 0, pod);
        drop(proxies);
        session.sync()
    }
}

fn dict_to_map(props: Option<&spa::utils::dict::DictRef>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(props) = props {
        for (key, value) in props.iter() {
            map.insert(key.to_owned(), value.to_owned());
        }
    }
    map
}

fn parse_props_pod(pod: &Pod) -> Option<(Option<f32>, Option<bool>)> {
    let object = pod.as_object().ok()?;
    let mut volume = None;
    let mut mute = None;
    let mut channel_volumes = None;
    for property in object.props() {
        let key = property.key().0;
        if key == spa::sys::SPA_PROP_volume {
            volume = property.value().get_float().ok();
        } else if key == spa::sys::SPA_PROP_mute {
            mute = property.value().get_bool().ok();
        } else if key == spa::sys::SPA_PROP_channelVolumes {
            channel_volumes = parse_channel_volumes(property.value());
        }
    }
    // Some nodes leave `volume` at its default while the real value lives in
    // `channelVolumes`; prefer the channel values when they agree.
    if let Some(volumes) = channel_volumes
        && let Some(first) = volumes.first()
        && volumes
            .iter()
            .all(|value| (value - first).abs() < f32::EPSILON)
    {
        volume = Some(*first);
    }
    Some((volume, mute))
}

fn parse_channel_volumes(pod: &Pod) -> Option<Vec<f32>> {
    let mut count = 0u32;
    let mut size = 0u32;
    let mut type_ = 0u32;
    // Safety: the pod is alive for the duration of this call and the returned
    // slice borrows from it only while the pod is borrowed.
    let values = unsafe {
        spa::sys::spa_pod_get_array_full(pod.as_raw_ptr(), &mut count, &mut size, &mut type_)
    };
    if values.is_null() || size != std::mem::size_of::<f32>() as u32 {
        return None;
    }
    if type_ != spa::sys::SPA_TYPE_Float {
        return None;
    }
    // Safety: the pointer points at `count` f32 values owned by the pod.
    let values = unsafe { std::slice::from_raw_parts(values.cast::<f32>(), count as usize) };
    Some(values.to_vec())
}

fn props_pod(volume: Option<f32>, mute: Option<bool>, channels: usize) -> Result<Vec<u8>, String> {
    use spa::pod::serialize::PodSerializer;
    use spa::pod::{Object, Property, PropertyFlags, Value, ValueArray};

    let mut properties = Vec::new();
    if let Some(volume) = volume {
        properties.push(Property {
            key: spa::sys::SPA_PROP_volume,
            flags: PropertyFlags::empty(),
            value: Value::Float(volume),
        });
        // `volume` is informational for many nodes; `channelVolumes` is what
        // actually changes the stream volume (the same field `wpctl` writes).
        properties.push(Property {
            key: spa::sys::SPA_PROP_channelVolumes,
            flags: PropertyFlags::empty(),
            value: Value::ValueArray(ValueArray::Float(vec![volume; channels])),
        });
    }
    if let Some(mute) = mute {
        properties.push(Property {
            key: spa::sys::SPA_PROP_mute,
            flags: PropertyFlags::empty(),
            value: Value::Bool(mute),
        });
    }
    let object = Object {
        type_: spa::utils::SpaTypes::ObjectParamProps.as_raw(),
        id: spa::param::ParamType::Props.as_raw(),
        properties,
    };
    let cursor = std::io::Cursor::new(Vec::new());
    let (cursor, _) = PodSerializer::serialize(cursor, &Value::Object(object))
        .map_err(|err| format!("cannot serialize PipeWire properties: {err}"))?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_pod_round_trips_volume_and_mute() {
        let bytes = props_pod(Some(0.25), Some(true), 2).unwrap();
        let pod = Pod::from_bytes(&bytes).unwrap();
        let (volume, mute) = parse_props_pod(pod).unwrap();
        assert_eq!(volume, Some(0.25));
        assert_eq!(mute, Some(true));
    }

    #[test]
    fn snapshot_derives_streams_and_sink_links() {
        let mut snapshot = RawSnapshot {
            nodes: HashMap::new(),
            ports: Vec::new(),
            links: Vec::new(),
            metadata: HashMap::new(),
            default_metadata: None,
            link_factory: None,
        };
        snapshot.nodes.insert(
            1,
            RawNode {
                props: BTreeMap::from([
                    ("media.class".to_owned(), "Audio/Sink".to_owned()),
                    ("node.name".to_owned(), "sink".to_owned()),
                ]),
                volume: None,
                mute: None,
            },
        );
        snapshot.nodes.insert(
            2,
            RawNode {
                props: BTreeMap::from([
                    ("media.class".to_owned(), "Stream/Output/Audio".to_owned()),
                    ("application.name".to_owned(), "Spotify".to_owned()),
                    ("media.name".to_owned(), "Song".to_owned()),
                ]),
                volume: Some(0.5),
                mute: Some(false),
            },
        );
        snapshot.links.push(RawLink {
            id: 9,
            output_node: 2,
            output_port: 10,
            input_node: 1,
            input_port: 11,
        });

        let streams = snapshot.streams();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].app, "Spotify");
        assert_eq!(streams[0].sink_id, Some(1));
        assert_eq!(streams[0].volume, Some(0.5));
    }

    #[test]
    fn reads_default_sink_from_metadata() {
        let mut metadata = HashMap::new();
        metadata.insert(
            40,
            BTreeMap::from([(
                "default.audio.sink".to_owned(),
                r#"{"name":"alsa_output.hdmi"}"#.to_owned(),
            )]),
        );
        let snapshot = RawSnapshot {
            nodes: HashMap::new(),
            ports: Vec::new(),
            links: Vec::new(),
            metadata,
            default_metadata: Some(40),
            link_factory: None,
        };
        assert_eq!(snapshot.default_sink().as_deref(), Some("alsa_output.hdmi"));
    }

    #[test]
    fn snapshot_exposes_sources_and_default_source() {
        let mut snapshot = RawSnapshot {
            nodes: HashMap::new(),
            ports: Vec::new(),
            links: Vec::new(),
            metadata: HashMap::new(),
            default_metadata: Some(40),
            link_factory: None,
        };
        snapshot.nodes.insert(
            7,
            RawNode {
                props: BTreeMap::from([
                    ("media.class".to_owned(), "Audio/Source".to_owned()),
                    ("node.name".to_owned(), "alsa_input.usb-mic".to_owned()),
                    ("node.description".to_owned(), "USB Mic".to_owned()),
                ]),
                volume: None,
                mute: None,
            },
        );
        snapshot.metadata.insert(
            40,
            BTreeMap::from([(
                "default.audio.source".to_owned(),
                r#"{"name":"alsa_input.usb-mic"}"#.to_owned(),
            )]),
        );
        let sources = snapshot.sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "alsa_input.usb-mic");
        assert_eq!(
            snapshot.default_source().as_deref(),
            Some("alsa_input.usb-mic")
        );
    }
}
