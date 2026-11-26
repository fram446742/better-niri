//! niri output controller.
//!
//! Applying a transform (and an optional position) uses `Request::Output`, not
//! an `Action`, so it goes through [`Compositor::request`]. Each request is one
//! IPC connection and niri replies with `OutputConfigChanged`; a missing output
//! is reported as an error so auto-rotation does not pretend it applied.
//!
//! Tests replace [`OutputController`] with a recorder, so debounce and policy
//! logic never needs a compositor.

use companion_niri::Compositor;
use niri_ipc::{
    ConfiguredPosition, OutputAction, OutputConfigChanged, PositionToSet, Request, Response,
    Transform,
};

/// Target-output operations used by `rotate.*`.
pub trait OutputController: Send + Sync {
    /// Applies `transform` and, when configured, `position`. Returns one
    /// description per request for status/explain output.
    fn apply(
        &self,
        output: &str,
        transform: Transform,
        position: Option<(i32, i32)>,
    ) -> Result<Vec<String>, String>;

    /// Current transform of `output`, or `None` when it is not mapped.
    fn current_transform(&self, output: &str) -> Result<Option<Transform>, String>;
}

/// niri-backed controller.
pub struct NiriOutputs {
    compositor: Compositor,
}

impl Default for NiriOutputs {
    fn default() -> Self {
        Self::from_env()
    }
}

impl NiriOutputs {
    pub fn from_env() -> Self {
        Self {
            compositor: Compositor::from_env(),
        }
    }

    pub fn with_compositor(compositor: Compositor) -> Self {
        Self { compositor }
    }

    fn send(&self, output: &str, action: OutputAction) -> Result<(), String> {
        let response = self.compositor.request(Request::Output {
            output: output.to_owned(),
            action,
        })?;
        match response {
            Response::OutputConfigChanged(OutputConfigChanged::Applied) => Ok(()),
            Response::OutputConfigChanged(OutputConfigChanged::OutputWasMissing) => {
                Err(format!("output `{output}` is not connected"))
            }
            other => Err(format!("unexpected niri response: {other:?}")),
        }
    }
}

impl OutputController for NiriOutputs {
    fn apply(
        &self,
        output: &str,
        transform: Transform,
        position: Option<(i32, i32)>,
    ) -> Result<Vec<String>, String> {
        self.send(output, OutputAction::Transform { transform })?;
        let mut calls = vec![format!(
            "transform {} on {output}",
            transform_name(transform)
        )];
        if let Some((x, y)) = position {
            self.send(
                output,
                OutputAction::Position {
                    position: PositionToSet::Specific(ConfiguredPosition { x, y }),
                },
            )?;
            calls.push(format!("position {x},{y} on {output}"));
        }
        Ok(calls)
    }

    fn current_transform(&self, output: &str) -> Result<Option<Transform>, String> {
        let response = self.compositor.request(Request::Outputs)?;
        let Response::Outputs(outputs) = response else {
            return Err(format!("unexpected niri response: {response:?}"));
        };
        let output = outputs
            .get(output)
            .ok_or_else(|| format!("output `{output}` is not connected"))?;
        Ok(output.logical.map(|logical| logical.transform))
    }
}

/// The name niri's IPC uses for a transform (`normal`, `90`, …).
pub fn transform_name(transform: Transform) -> &'static str {
    match transform {
        Transform::Normal => "normal",
        Transform::_90 => "90",
        Transform::_180 => "180",
        Transform::_270 => "270",
        Transform::Flipped => "flipped",
        Transform::Flipped90 => "flipped-90",
        Transform::Flipped180 => "flipped-180",
        Transform::Flipped270 => "flipped-270",
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loom-companion-rotate-outputs-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("niri.sock")
    }

    fn serve(path: &Path, responses: Vec<&'static str>) -> std::thread::JoinHandle<Vec<Request>> {
        let listener = UnixListener::bind(path).unwrap();
        std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                requests.push(serde_json::from_str(line.trim()).unwrap());
                let mut stream = stream;
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(b"\n").unwrap();
                stream.flush().unwrap();
            }
            requests
        })
    }

    #[test]
    fn applies_transform_and_position_over_ipc() {
        let socket = temp_socket("apply");
        let server = serve(
            &socket,
            vec![
                "{\"Ok\":{\"OutputConfigChanged\":\"Applied\"}}",
                "{\"Ok\":{\"OutputConfigChanged\":\"Applied\"}}",
            ],
        );
        let outputs = NiriOutputs::with_compositor(Compositor::at(&socket));
        let calls = outputs
            .apply("eDP-1", Transform::_90, Some((100, 50)))
            .unwrap();
        assert_eq!(calls, ["transform 90 on eDP-1", "position 100,50 on eDP-1"]);
        let requests = server.join().unwrap();
        assert!(matches!(requests[0], Request::Output { .. }));
        assert!(matches!(requests[1], Request::Output { .. }));
    }

    #[test]
    fn missing_output_is_an_error() {
        let socket = temp_socket("missing");
        let server = serve(
            &socket,
            vec!["{\"Ok\":{\"OutputConfigChanged\":\"OutputWasMissing\"}}"],
        );
        let outputs = NiriOutputs::with_compositor(Compositor::at(&socket));
        let error = outputs.apply("DP-9", Transform::_180, None).unwrap_err();
        assert!(error.contains("not connected"));
        server.join().unwrap();
    }

    #[test]
    fn reads_the_current_transform() {
        let socket = temp_socket("current");
        let response = json!({
            "Ok": {
                "Outputs": {
                    "eDP-1": {
                        "name": "eDP-1",
                        "make": "test",
                        "model": "test",
                        "serial": null,
                        "physical_size": null,
                        "modes": [],
                        "current_mode": null,
                        "is_custom_mode": false,
                        "vrr_supported": false,
                        "vrr_enabled": false,
                        "logical": {
                            "x": 0,
                            "y": 0,
                            "width": 1920,
                            "height": 1080,
                            "scale": 1.0,
                            "transform": "270"
                        },
                        "max_bpc": null
                    }
                }
            }
        })
        .to_string();
        let response: &'static str = Box::leak(response.into_boxed_str());
        let server = serve(&socket, vec![response, response]);
        let outputs = NiriOutputs::with_compositor(Compositor::at(&socket));
        assert_eq!(
            outputs.current_transform("eDP-1").unwrap(),
            Some(Transform::_270)
        );
        assert!(outputs.current_transform("DP-1").is_err());
        server.join().unwrap();
    }
}
