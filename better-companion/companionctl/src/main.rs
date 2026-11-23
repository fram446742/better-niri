//! `companionctl`: a small client for the `better-companion` socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use companion_core::{Request, Response};

fn usage() -> &'static str {
    "companionctl [--socket PATH] <command>\n\n\
     Commands:\n\
       list                                   List namespaces and actions\n\
       status                                 Daemon status\n\
       call <namespace> <action> [json]       Call an action (json defaults to {{}})\n"
}

fn build_request(args: &[String]) -> Result<Request, String> {
    match args {
        [command] if command == "list" => Ok(Request::List),
        [command] if command == "status" => Ok(Request::Status),
        [command, namespace, action] if command == "call" => Ok(Request::Call {
            namespace: namespace.clone(),
            action: action.clone(),
            params: serde_json::json!({}),
        }),
        [command, namespace, action, params] if command == "call" => {
            let params = serde_json::from_str(params)
                .map_err(|err| format!("invalid params JSON: {err}"))?;
            Ok(Request::Call {
                namespace: namespace.clone(),
                action: action.clone(),
                params,
            })
        }
        [] => Err("missing command".to_owned()),
        other => Err(format!("invalid arguments: {other:?}")),
    }
}

fn main() -> ExitCode {
    let mut socket = None;
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
            "--help" | "-h" => {
                print!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other => rest.push(other.to_owned()),
        }
    }

    let request = match build_request(&rest) {
        Ok(request) => request,
        Err(err) => {
            eprintln!("companionctl: {err}\n\n{}", usage());
            return ExitCode::FAILURE;
        }
    };

    let socket = socket.unwrap_or_else(companion_core::socket_path);
    match send(&socket, &request) {
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
    }
}

fn send(socket: &std::path::Path, request: &Request) -> Result<Response, String> {
    let stream = UnixStream::connect(socket)
        .map_err(|err| format!("cannot connect to {}: {err}", socket.display()))?;
    let mut writer = stream
        .try_clone()
        .map_err(|err| format!("cannot clone socket: {err}"))?;
    let mut reader = BufReader::new(stream);

    let body = serde_json::to_string(request).map_err(|err| format!("encode: {err}"))?;
    writer
        .write_all(body.as_bytes())
        .and_then(|()| writer.write_all(b"\n"))
        .and_then(|()| writer.flush())
        .map_err(|err| format!("write: {err}"))?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|err| format!("read: {err}"))?;
    if line.trim().is_empty() {
        return Err("daemon closed the connection without a response".to_owned());
    }
    serde_json::from_str(line.trim()).map_err(|err| format!("invalid response: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_requests_with_default_and_explicit_params() {
        assert_eq!(build_request(&["list".into()]), Ok(Request::List));
        assert_eq!(build_request(&["status".into()]), Ok(Request::Status));
        assert_eq!(
            build_request(&["call".into(), "system".into(), "ping".into()]),
            Ok(Request::Call {
                namespace: "system".into(),
                action: "ping".into(),
                params: serde_json::json!({}),
            })
        );
        assert_eq!(
            build_request(&[
                "call".into(),
                "parking".into(),
                "park".into(),
                r#"{"id":"x"}"#.into()
            ]),
            Ok(Request::Call {
                namespace: "parking".into(),
                action: "park".into(),
                params: serde_json::json!({ "id": "x" }),
            })
        );
        assert!(build_request(&[]).is_err());
        assert!(build_request(&["nope".into()]).is_err());
    }
}
