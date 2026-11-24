use std::path::PathBuf;
use std::process::ExitCode;

use better_wallpaper::backend::{Backend, Selection, select};
use better_wallpaper::native::{self, Mode};
use better_wallpaper::{
    Scene, Store, apply_once, command_template, state_path, watch, workspace_plan,
};

fn usage() -> &'static str {
    "better-wallpaper [--socket PATH] [--backend=auto|native|command] <command>\n\n\
     Commands:\n\
       scene set <scene> <workspace|default> <image>\n\
       scene use <scene>\n\
       scene list\n\
       scene show <scene>\n\
       scene delete <scene>\n\
       apply [--dry-run]             Apply to every output now (native keeps running)\n\
       watch [--dry-run]             Follow niri and reapply on focus changes\n\
       status                        Show the active scene, mappings and backend\n\n\
     Backends:\n\
       auto     native when the compositor supports layer-shell, else command (default)\n\
       native   built-in wlr-layer-shell background renderer (no external program)\n\
       command  external command template per output, from $BETTER_WALLPAPER_COMMAND\n\
                (default `swaybg -i {image}`) with {image}, {workspace} and {output}\n\n\
     $BETTER_WALLPAPER_BACKEND sets the default backend; --socket defaults to $NIRI_SOCKET."
}

fn main() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("better-wallpaper: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_cli() -> Result<(), String> {
    let mut socket = std::env::var_os("NIRI_SOCKET").map(PathBuf::from);
    let mut dry_run = false;
    let mut backend = None;
    let mut args = Vec::new();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--socket" => {
                socket = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--socket needs a path".to_owned())?,
                ))
            }
            "--backend" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--backend needs auto, native or command".to_owned())?;
                backend = Some(Backend::parse(&value)?);
            }
            "--dry-run" => dry_run = true,
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            other if other.starts_with("--backend=") => {
                backend = Some(Backend::parse(&other["--backend=".len()..])?);
            }
            _ => args.push(arg),
        }
    }
    let Some(command) = args.first().cloned() else {
        return Err(format!("missing command\n\n{}", usage()));
    };

    let state = std::env::var_os("BETTER_WALLPAPER_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(state_path);
    let mut store = Store::load(&state).map_err(|err| format!("cannot load state: {err}"))?;
    let socket = || {
        socket
            .clone()
            .ok_or_else(|| "NIRI_SOCKET is not set; run inside niri or pass --socket".to_owned())
    };

    match command.as_str() {
        "scene" => {
            let sub = args
                .get(1)
                .ok_or_else(|| "scene needs set/use/list/show/delete".to_owned())?;
            match sub.as_str() {
                "set" => {
                    let name = args
                        .get(2)
                        .ok_or_else(|| "scene set needs a scene name".to_owned())?;
                    let key = args
                        .get(3)
                        .ok_or_else(|| "scene set needs `<workspace|default>`".to_owned())?;
                    let image = args
                        .get(4)
                        .ok_or_else(|| "scene set needs an image path".to_owned())?;
                    let scene = store.scenes.entry(name.clone()).or_default();
                    if key == "default" {
                        scene.default = Some(image.clone());
                    } else {
                        scene.wallpapers.insert(key.clone(), image.clone());
                    }
                    store
                        .save(&state)
                        .map_err(|err| format!("cannot save state: {err}"))?;
                    println!(
                        "{}",
                        serde_json::json!({ "scene": name, "workspace": key, "image": image })
                    );
                }
                "use" => {
                    let name = args
                        .get(2)
                        .ok_or_else(|| "scene use needs a scene name".to_owned())?;
                    if !store.scenes.contains_key(name) {
                        return Err(format!("unknown scene `{name}`"));
                    }
                    store.active = Some(name.clone());
                    store
                        .save(&state)
                        .map_err(|err| format!("cannot save state: {err}"))?;
                    println!("{}", serde_json::json!({ "active": name }));
                }
                "list" => {
                    let names: Vec<_> = store.scenes.keys().cloned().collect();
                    println!("{}", serde_json::to_string_pretty(&names).unwrap());
                }
                "show" => {
                    let name = args
                        .get(2)
                        .ok_or_else(|| "scene show needs a scene name".to_owned())?;
                    let scene: &Scene = store
                        .scenes
                        .get(name)
                        .ok_or_else(|| format!("unknown scene `{name}`"))?;
                    println!("{}", serde_json::to_string_pretty(scene).unwrap());
                }
                "delete" => {
                    let name = args
                        .get(2)
                        .ok_or_else(|| "scene delete needs a scene name".to_owned())?;
                    store.scenes.remove(name);
                    if store.active.as_deref() == Some(name) {
                        store.active = None;
                    }
                    store
                        .save(&state)
                        .map_err(|err| format!("cannot save state: {err}"))?;
                    println!("{}", serde_json::json!({ "deleted": name }));
                }
                other => return Err(format!("unknown scene command `{other}`")),
            }
            Ok(())
        }
        "apply" => {
            let requested = requested_backend(backend)?;
            let scene = active_scene(&store)?;
            let socket = socket()?;
            if dry_run {
                if requested == Backend::Command {
                    // Keep the original single-workspace dry-run output for the
                    // command backend.
                    let image = apply_once(&socket, &store, true)?;
                    print_json(&serde_json::json!({
                        "dry_run": true,
                        "backend": requested.as_str(),
                        "image": image,
                        "template": command_template(),
                    }));
                } else {
                    print_plan(requested, &socket, &store, true, "apply")?;
                }
                return Ok(());
            }
            let selection = select(requested, native::probe())?;
            if !matches!(selection, Selection::Native) {
                let image = apply_once(&socket, &store, false)?;
                print_json(&serde_json::json!({
                    "dry_run": false,
                    "backend": selection.as_str(),
                    "image": image,
                    "template": command_template(),
                }));
                return Ok(());
            }
            if let Err(err) = print_plan(requested, &socket, &store, false, "apply") {
                eprintln!("better-wallpaper: {err}; continuing with the scene default");
            }
            eprintln!(
                "better-wallpaper: native apply keeps running to stay on screen; \
                 use `watch` to follow workspaces"
            );
            match native::run(&scene, Some(&socket), Mode::Apply) {
                Ok(()) => Ok(()),
                Err(native::NativeError::Unsupported(reason)) if requested == Backend::Auto => {
                    eprintln!("better-wallpaper: {reason}; using the command backend");
                    let image = apply_once(&socket, &store, false)?;
                    print_json(&serde_json::json!({
                        "dry_run": false,
                        "backend": "command",
                        "image": image,
                        "template": command_template(),
                    }));
                    Ok(())
                }
                Err(err) => Err(err.to_string()),
            }
        }
        "watch" => {
            let requested = requested_backend(backend)?;
            let scene = active_scene(&store)?;
            let socket = socket()?;
            if dry_run {
                print_plan(requested, &socket, &store, true, "watch")?;
                return Ok(());
            }
            let selection = select(requested, native::probe())?;
            match selection {
                Selection::Command => {
                    let applied = watch(&socket, &store, &command_template(), false)?;
                    print_json(&serde_json::json!({
                        "backend": selection.as_str(),
                        "applied": applied,
                    }));
                    Ok(())
                }
                Selection::Native => {
                    if let Err(err) = print_plan(requested, &socket, &store, false, "watch") {
                        eprintln!("better-wallpaper: {err}; continuing with the scene default");
                    }
                    match native::run(&scene, Some(&socket), Mode::Watch) {
                        Ok(()) => Ok(()),
                        Err(native::NativeError::Unsupported(reason))
                            if requested == Backend::Auto =>
                        {
                            eprintln!(
                                "better-wallpaper: {reason}; using the command backend instead"
                            );
                            let applied = watch(&socket, &store, &command_template(), false)?;
                            print_json(&serde_json::json!({
                                "backend": "command",
                                "applied": applied,
                            }));
                            Ok(())
                        }
                        Err(err) => Err(err.to_string()),
                    }
                }
            }
        }
        "status" => {
            let requested = requested_backend(backend)?;
            let probe = native::probe();
            print_json(&serde_json::json!({
                "active": store.active,
                "scenes": store.scenes,
                "template": command_template(),
                "backend": {
                    "requested": requested.as_str(),
                    "native_available": probe.is_ok(),
                    "native_error": probe.err(),
                },
            }));
            Ok(())
        }
        other => Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
}

/// Backend from `--backend`, else `$BETTER_WALLPAPER_BACKEND`, else `auto`.
fn requested_backend(explicit: Option<Backend>) -> Result<Backend, String> {
    match explicit {
        Some(backend) => Ok(backend),
        None => Backend::from_env(),
    }
}

fn active_scene(store: &Store) -> Result<Scene, String> {
    store
        .active_scene()
        .cloned()
        .ok_or_else(|| "no active scene; use `scene use <name>` first".to_owned())
}

fn print_plan(
    requested: Backend,
    socket: &std::path::Path,
    store: &Store,
    dry_run: bool,
    mode: &str,
) -> Result<(), String> {
    let images = workspace_plan(socket, store)?;
    print_json(&serde_json::json!({
        "dry_run": dry_run,
        "backend": requested.as_str(),
        "mode": mode,
        "images": images,
        "template": command_template(),
    }));
    Ok(())
}

fn print_json(value: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}
