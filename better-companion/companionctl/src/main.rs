//! `companionctl`: a small client for the `better-companion` socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use companion_core::{Request, Response};

fn usage() -> &'static str {
    "companionctl [--socket PATH] [--id N] [--dry-run] <command>\n\n\
     Commands:\n\
       list                                   List namespaces and actions\n\
       describe                               Protocol version and full action schema\n\
       status                                 Daemon status\n\
       call <namespace> <action> [json]       Call an action (json defaults to {{}})\n\
       explain <rules|windows|adapters|...> [json]\n\
                                              Explain matches without acting\n\
       history [limit]                        Show recent rule/automation runs\n\
       config <show|validate|reload> [json]   Inspect or reload the config file\n\
       subscribe [topic ...]                  Stream events until interrupted\n\n\
     --id N       Echo this request id on the response\n\
     --dry-run    Add `dry_run: true` to call/explain params\n"
}

#[derive(Debug, Clone, Default)]
struct Options {
    id: Option<u64>,
    dry_run: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Command {
    Request(Request),
    Subscribe { events: Vec<String> },
}

fn add_dry_run(params: serde_json::Value) -> Result<serde_json::Value, String> {
    match params {
        serde_json::Value::Null => Ok(serde_json::json!({ "dry_run": true })),
        serde_json::Value::Object(mut object) => {
            object.insert("dry_run".to_owned(), serde_json::json!(true));
            Ok(serde_json::Value::Object(object))
        }
        other => Err(format!("--dry-run needs object params, got `{other}`")),
    }
}

fn explain_params(json: Option<&String>, options: &Options) -> Result<serde_json::Value, String> {
    let params = match json {
        Some(text) => {
            serde_json::from_str(text).map_err(|err| format!("invalid params JSON: {err}"))?
        }
        None => serde_json::json!({}),
    };
    if options.dry_run {
        add_dry_run(params)
    } else {
        Ok(params)
    }
}

fn build_command(args: &[String], options: &Options) -> Result<Command, String> {
    let call = |namespace: &str, action: &str, params: serde_json::Value| {
        let params = if options.dry_run {
            add_dry_run(params)?
        } else {
            params
        };
        Ok(Command::Request(Request::Call {
            namespace: namespace.to_owned(),
            action: action.to_owned(),
            params,
            id: options.id,
        }))
    };

    match args {
        [command] if command == "list" => Ok(Command::Request(Request::List)),
        [command] if command == "describe" => Ok(Command::Request(Request::Describe)),
        [command] if command == "status" => Ok(Command::Request(Request::Status)),
        [command, namespace, action] if command == "call" => {
            call(namespace, action, serde_json::json!({}))
        }
        [command, namespace, action, params] if command == "call" => {
            let params = serde_json::from_str(params)
                .map_err(|err| format!("invalid params JSON: {err}"))?;
            call(namespace, action, params)
        }
        [command, target] if command == "explain" => {
            let action = if target == "windows" {
                "find"
            } else {
                "explain"
            };
            let params = explain_params(None, options)?;
            call(target, action, params)
        }
        [command, target, params] if command == "explain" => {
            let action = if target == "windows" {
                "find"
            } else {
                "explain"
            };
            let params = explain_params(Some(params), options)?;
            call(target, action, params)
        }
        [command] if command == "history" => call("history", "list", serde_json::json!({})),
        [command, limit] if command == "history" => {
            let limit: u64 = limit
                .parse()
                .map_err(|_| format!("history limit must be an integer, got `{limit}`"))?;
            call("history", "list", serde_json::json!({ "limit": limit }))
        }
        [command, sub] if command == "config" => call("config", sub, serde_json::json!({})),
        [command, sub, params] if command == "config" => {
            let params = serde_json::from_str(params)
                .map_err(|err| format!("invalid params JSON: {err}"))?;
            call("config", sub, params)
        }
        [command, topics @ ..] if command == "subscribe" => Ok(Command::Subscribe {
            events: topics.to_vec(),
        }),
        [] => Err("missing command".to_owned()),
        other => Err(format!("invalid arguments: {other:?}")),
    }
}

fn main() -> ExitCode {
    let mut socket = None;
    let mut options = Options::default();
    let mut rest = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => match args.next() {
                Some(path) => socket = Some(PathBuf::from(path)),
                None => {
                    eprintln!("--socket needs a path");
                    return ExitCode::FAILURE;
                }
            },
            "--id" => match args.next() {
                Some(id) => match id.parse() {
                    Ok(id) => options.id = Some(id),
                    Err(_) => {
                        eprintln!("--id needs an integer, got `{id}`");
                        return ExitCode::FAILURE;
                    }
                },
                None => {
                    eprintln!("--id needs a value");
                    return ExitCode::FAILURE;
                }
            },
            "--dry-run" => options.dry_run = true,
            "--help" | "-h" => {
                print!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other => rest.push(other.to_owned()),
        }
    }

    if options.dry_run && !matches!(rest.first().map(String::as_str), Some("call" | "explain")) {
        eprintln!("--dry-run is only valid with `call` or `explain`");
        return ExitCode::FAILURE;
    }

    let command = match build_command(&rest, &options) {
        Ok(command) => command,
        Err(err) => {
            eprintln!("companionctl: {err}\n\n{}", usage());
            return ExitCode::FAILURE;
        }
    };

    let socket = socket.unwrap_or_else(companion_core::socket_path);
    match command {
        Command::Request(request) => match send(&socket, &request) {
            Ok(response) => {
                println!("{}", serde_json::to_string_pretty(&response).unwrap());
                match response {
                    Response::Error { .. } => ExitCode::FAILURE,
                    _ => ExitCode::SUCCESS,
                }
            }
            Err(err) => {
                eprintln!("companionctl: {err}");
                ExitCode::FAILURE
            }
        },
        Command::Subscribe { events } => match subscribe(&socket, events) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("companionctl: {err}");
                ExitCode::FAILURE
            }
        },
    }
}

fn connect(socket: &std::path::Path) -> Result<(UnixStream, BufReader<UnixStream>), String> {
    let stream = UnixStream::connect(socket)
        .map_err(|err| format!("cannot connect to {}: {err}", socket.display()))?;
    let reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|err| format!("cannot clone socket: {err}"))?,
    );
    Ok((stream, reader))
}

fn write_request(writer: &mut UnixStream, request: &Request) -> Result<(), String> {
    let body = serde_json::to_string(request).map_err(|err| format!("encode: {err}"))?;
    writer
        .write_all(body.as_bytes())
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush())
        .map_err(|err| format!("write: {err}"))
}

fn send(socket: &std::path::Path, request: &Request) -> Result<Response, String> {
    let (mut writer, mut reader) = connect(socket)?;
    write_request(&mut writer, request)?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|err| format!("read: {err}"))?;
    if line.trim().is_empty() {
        return Err("daemon closed the connection without a response".to_owned());
    }
    serde_json::from_str(line.trim()).map_err(|err| format!("invalid response: {err}"))
}

/// Streams subscription events until the daemon closes the connection.
fn subscribe(socket: &std::path::Path, events: Vec<String>) -> Result<(), String> {
    let (mut writer, reader) = connect(socket)?;
    write_request(&mut writer, &Request::Subscribe { events, id: None })?;
    for line in reader.lines() {
        let line = line.map_err(|err| format!("read: {err}"))?;
        if line.trim().is_empty() {
            continue;
        }
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(args: &[&str]) -> Command {
        build_command(
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            &Options::default(),
        )
        .unwrap()
    }

    fn call(namespace: &str, action: &str, params: serde_json::Value) -> Command {
        Command::Request(Request::Call {
            namespace: namespace.into(),
            action: action.into(),
            params,
            id: None,
        })
    }

    #[test]
    fn builds_requests_with_default_and_explicit_params() {
        assert_eq!(command(&["list"]), Command::Request(Request::List));
        assert_eq!(command(&["status"]), Command::Request(Request::Status));
        assert_eq!(
            command(&["call", "system", "ping"]),
            call("system", "ping", serde_json::json!({}))
        );
        assert_eq!(
            command(&["call", "parking", "park", r#"{"id":"x"}"#]),
            call("parking", "park", serde_json::json!({ "id": "x" }))
        );
        assert!(build_command(&[], &Options::default()).is_err());
        assert!(build_command(&["nope".to_owned()], &Options::default()).is_err());
    }

    #[test]
    fn request_ids_are_attached_to_calls() {
        let options = Options {
            id: Some(9),
            dry_run: false,
        };
        match build_command(&["status".to_owned()], &options).unwrap() {
            Command::Request(Request::Status) => {}
            other => panic!("unexpected: {other:?}"),
        }
        match build_command(
            &["call".to_owned(), "system".to_owned(), "ping".to_owned()],
            &options,
        )
        .unwrap()
        {
            Command::Request(Request::Call { id, .. }) => assert_eq!(id, Some(9)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn explain_maps_rules_and_windows_to_existing_actions() {
        assert_eq!(
            command(&["explain", "rules"]),
            call("rules", "explain", serde_json::json!({}))
        );
        assert_eq!(
            command(&["explain", "windows", r#"{"app_id":"firefox"}"#]),
            call(
                "windows",
                "find",
                serde_json::json!({ "app_id": "firefox" })
            )
        );
        assert_eq!(
            command(&["explain", "adapters", r#"{"event":"workspace-activated"}"#]),
            call(
                "adapters",
                "explain",
                serde_json::json!({ "event": "workspace-activated" })
            )
        );
    }

    #[test]
    fn dry_run_adds_the_flag_to_call_and_explain_params() {
        let options = Options {
            id: None,
            dry_run: true,
        };
        match build_command(
            &[
                "call".to_owned(),
                "automation".to_owned(),
                "run".to_owned(),
                r#"{"name":"focus"}"#.to_owned(),
            ],
            &options,
        )
        .unwrap()
        {
            Command::Request(Request::Call { params, .. }) => {
                assert_eq!(params["dry_run"], true);
                assert_eq!(params["name"], "focus");
            }
            other => panic!("unexpected: {other:?}"),
        }
        match build_command(&["explain".to_owned(), "windows".to_owned()], &options).unwrap() {
            Command::Request(Request::Call { params, .. }) => {
                assert_eq!(params["dry_run"], true);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn history_config_and_subscribe_commands() {
        assert_eq!(
            command(&["history", "5"]),
            call("history", "list", serde_json::json!({ "limit": 5 }))
        );
        assert_eq!(
            command(&["config", "show"]),
            call("config", "show", serde_json::json!({}))
        );
        assert_eq!(
            command(&["config", "validate", r#"{"config":{}}"#]),
            call("config", "validate", serde_json::json!({ "config": {} }))
        );
        assert_eq!(
            command(&["subscribe", "park", "restore"]),
            Command::Subscribe {
                events: vec!["park".into(), "restore".into()]
            }
        );
    }
}
