//! Strict parser for `pw-dump` JSON.
//!
//! `pw-dump` emits a JSON array of PipeWire globals. Node properties live under
//! `info.props` (string values) and node parameters under `info.params`;
//! ports and links carry their endpoint metadata under `info.props` or
//! `info.*-node-id`/`info.*-port-id`.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::{LinkInfo, Sink, Source, Stream};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpPort {
    pub id: u32,
    pub node_id: u32,
    pub direction: String,
    pub name: String,
    pub channel: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpLink {
    pub id: u32,
    pub output_node: u32,
    pub output_port: u32,
    pub input_node: u32,
    pub input_port: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Dump {
    pub sinks: Vec<Sink>,
    pub sources: Vec<Source>,
    pub streams: Vec<Stream>,
    pub ports: Vec<DumpPort>,
    pub links: Vec<DumpLink>,
    /// Name of the `link` factory, normally `link-factory`.
    pub link_factory: Option<String>,
}

impl Dump {
    pub fn sink_by_name(&self, name: &str) -> Option<&Sink> {
        self.sinks.iter().find(|sink| sink.name == name)
    }

    pub fn links(&self) -> Vec<LinkInfo> {
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

pub fn parse_dump(json: &str) -> Result<Dump, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|err| format!("invalid pw-dump JSON: {err}"))?;
    let array = value
        .as_array()
        .ok_or_else(|| "pw-dump JSON is not an array".to_owned())?;

    let mut dump = Dump::default();
    let mut sink_nodes = Vec::new();
    let mut source_nodes = Vec::new();
    let mut stream_nodes = Vec::new();

    for entry in array {
        let type_ = entry
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(id) = entry.get("id").and_then(Value::as_u64) else {
            continue;
        };
        let id = id as u32;
        let info = entry.get("info").unwrap_or(&Value::Null);
        let props = info.get("props").and_then(Value::as_object);

        match type_ {
            "PipeWire:Interface:Node" => {
                let Some(props) = props else { continue };
                let class = string_prop(props, "media.class").unwrap_or_default();
                if class == "Audio/Sink" {
                    let name = string_prop(props, "node.name")
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("sink {id}"));
                    let description = string_prop(props, "node.description")
                        .or_else(|| string_prop(props, "node.nick"))
                        .unwrap_or(&name)
                        .to_owned();
                    sink_nodes.push(Sink {
                        id,
                        name,
                        description,
                    });
                } else if class == "Audio/Source" {
                    let name = string_prop(props, "node.name")
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("source {id}"));
                    let description = string_prop(props, "node.description")
                        .or_else(|| string_prop(props, "node.nick"))
                        .unwrap_or(&name)
                        .to_owned();
                    source_nodes.push(Source {
                        id,
                        name,
                        description,
                    });
                } else if class == "Stream/Output/Audio" {
                    let (volume, mute) = parse_props(info);
                    let app = string_prop(props, "application.name")
                        .or_else(|| string_prop(props, "application.process.binary"))
                        .or_else(|| string_prop(props, "media.name"))
                        .or_else(|| string_prop(props, "node.name"))
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("stream {id}"));
                    let binary =
                        string_prop(props, "application.process.binary").map(str::to_owned);
                    let media_name = string_prop(props, "media.name")
                        .unwrap_or_default()
                        .to_owned();
                    stream_nodes.push(Stream {
                        id,
                        sink_id: None,
                        app,
                        binary,
                        media_name,
                        volume,
                        mute,
                    });
                }
            }
            "PipeWire:Interface:Port" => {
                let Some(props) = props else { continue };
                let Some(node_id) = number_prop(props, "node.id") else {
                    continue;
                };
                dump.ports.push(DumpPort {
                    id,
                    node_id,
                    direction: string_prop(props, "port.direction")
                        .unwrap_or_default()
                        .to_owned(),
                    name: string_prop(props, "port.name")
                        .unwrap_or_default()
                        .to_owned(),
                    channel: string_prop(props, "audio.channel")
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
            "PipeWire:Interface:Link" => {
                let Some(output_node) = number_field(info, "output-node-id") else {
                    continue;
                };
                let Some(input_node) = number_field(info, "input-node-id") else {
                    continue;
                };
                dump.links.push(DumpLink {
                    id,
                    output_node,
                    output_port: number_field(info, "output-port-id").unwrap_or_default(),
                    input_node,
                    input_port: number_field(info, "input-port-id").unwrap_or_default(),
                });
            }
            "PipeWire:Interface:Factory" => {
                if let Some(props) = props
                    && string_prop(props, "factory.type.name") == Some("PipeWire:Interface:Link")
                {
                    dump.link_factory = string_prop(props, "factory.name").map(str::to_owned);
                }
            }
            _ => {}
        }
    }

    sink_nodes.sort_by_key(|sink| sink.id);
    source_nodes.sort_by_key(|source| source.id);
    let sink_ids: HashSet<u32> = sink_nodes.iter().map(|sink| sink.id).collect();

    // A playback stream is connected to the first of its links whose input is
    // a sink node. Virtual sinks (e.g. an effects sink) are sinks too.
    for stream in &mut stream_nodes {
        stream.sink_id = dump
            .links
            .iter()
            .filter(|link| link.output_node == stream.id && sink_ids.contains(&link.input_node))
            .map(|link| link.input_node)
            .min();
    }
    stream_nodes.sort_by_key(|stream| stream.id);

    dump.sinks = sink_nodes;
    dump.sources = source_nodes;
    dump.streams = stream_nodes;
    dump.ports.sort_by_key(|port| port.id);
    dump.links.sort_by_key(|link| link.id);
    Ok(dump)
}

fn string_prop<'a>(props: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    props.get(key).and_then(Value::as_str)
}

fn number_prop(props: &Map<String, Value>, key: &str) -> Option<u32> {
    match props.get(key)? {
        Value::String(value) => value.parse().ok(),
        Value::Number(value) => value.as_u64().map(|value| value as u32),
        _ => None,
    }
}

fn number_field(info: &Value, key: &str) -> Option<u32> {
    match info.get(key)? {
        Value::String(value) => value.parse().ok(),
        Value::Number(value) => value.as_u64().map(|value| value as u32),
        _ => None,
    }
}

/// Reads `volume` and `mute` from the first `Props` parameter of a node.
fn parse_props(info: &Value) -> (Option<f32>, Option<bool>) {
    let Some(props) = info
        .get("params")
        .and_then(|params| params.get("Props"))
        .and_then(Value::as_array)
        .and_then(|props| props.first())
    else {
        return (None, None);
    };
    let volume = props
        .get("channelVolumes")
        .and_then(Value::as_array)
        .and_then(|volumes| volumes.first())
        .and_then(Value::as_f64)
        .map(|volume| volume as f32)
        .or_else(|| {
            props
                .get("volume")
                .and_then(Value::as_f64)
                .map(|volume| volume as f32)
        });
    let mute = props.get("mute").and_then(Value::as_bool);
    (volume, mute)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = include_str!("../../tests/fixtures/pw-dump.json");

    #[test]
    fn parses_pw_dump_fixture() {
        let dump = parse_dump(DUMP).unwrap();
        assert_eq!(
            dump.sinks
                .iter()
                .map(|sink| sink.name.as_str())
                .collect::<Vec<_>>(),
            [
                "easyeffects_sink",
                "alsa_output.usb-headset",
                "alsa_output.hdmi"
            ]
        );

        let zen = dump
            .streams
            .iter()
            .find(|stream| stream.app == "Zen")
            .unwrap();
        assert_eq!(zen.sink_id, Some(52));
        assert_eq!(zen.binary.as_deref(), Some("zen-bin"));
        assert_eq!(zen.media_name, "Song");
        assert_eq!(zen.volume, Some(0.5));
        assert_eq!(zen.mute, Some(false));

        let cat = dump
            .streams
            .iter()
            .find(|stream| stream.app == "pw-cat")
            .unwrap();
        assert_eq!(cat.sink_id, Some(52));
        assert_eq!(cat.mute, Some(true));
        assert_eq!(dump.link_factory.as_deref(), Some("link-factory"));
        assert_eq!(dump.ports.len(), 10);
        assert_eq!(dump.links.len(), 4);
    }

    #[test]
    fn rejects_malformed_dump() {
        assert!(parse_dump("{}").unwrap_err().contains("not an array"));
        assert!(
            parse_dump("nope")
                .unwrap_err()
                .contains("invalid pw-dump JSON")
        );
    }

    #[test]
    fn ignores_streams_without_sink_links() {
        let json = r#"[
            {"id": 5, "type": "PipeWire:Interface:Node", "info": {"props": {"media.class": "Stream/Output/Audio", "application.name": "Solo"}}}
        ]"#;
        let dump = parse_dump(json).unwrap();
        assert_eq!(dump.streams.len(), 1);
        assert_eq!(dump.streams[0].sink_id, None);
        assert_eq!(dump.streams[0].volume, None);
    }

    #[test]
    fn parses_input_sources_and_links() {
        let json = r#"[
            {"id": 9, "type": "PipeWire:Interface:Node", "info": {"props": {
                "media.class": "Audio/Source", "node.name": "alsa_input.usb-mic",
                "node.description": "USB Mic"
            }}},
            {"id": 12, "type": "PipeWire:Interface:Link", "info": {
                "output-node-id": 4, "output-port-id": 5, "input-node-id": 9, "input-port-id": 6
            }}
        ]"#;
        let dump = parse_dump(json).unwrap();
        assert_eq!(dump.sources.len(), 1);
        assert_eq!(dump.sources[0].name, "alsa_input.usb-mic");
        assert_eq!(dump.sources[0].description, "USB Mic");
        assert_eq!(
            dump.links(),
            vec![LinkInfo {
                id: 12,
                output_node: 4,
                input_node: 9
            }]
        );
    }
}
