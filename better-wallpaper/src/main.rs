use std::path::PathBuf;
use std::process::ExitCode;

use better_wallpaper::{Scene, Store, apply_once, command_template, state_path, watch};

fn usage() -> &'static str {
    "better-wallpaper [--socket PATH] <command>\n\n\
     Commands:\n\
       scene set <scene> <workspace|default> <image>\n\
       scene use <scene>\n\
       scene list\n\
       scene show <scene>\n\
       scene delete <scene>\n\
       apply [--dry-run]             Apply to the focused workspace now\n\
       watch [--dry-run]             Follow niri and reapply on focus changes\n\
       status                        Show the active scene and mappings\n\n\
     Command template: $BETTER_WALLPAPER_COMMAND (default `swaybg -i {image}`)\n\
     with {image}, {workspace} and {output}. Socket defaults to $NIRI_SOCKET."
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
            "--dry-run" => dry_run = true,
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
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
        }
        "apply" => {
            let image = apply_once(&socket()?, &store, dry_run)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "dry_run": dry_run,
                    "image": image,
                    "template": command_template(),
                }))
                .unwrap()
            );
        }
        "watch" => {
            let applied = watch(&socket()?, &store, &command_template(), dry_run)?;
            println!("{}", serde_json::json!({ "applied": applied }));
        }
        "status" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "active": store.active,
                    "scenes": store.scenes,
                    "template": command_template(),
                }))
                .unwrap()
            );
        }
        other => return Err(format!("unknown command `{other}`\n\n{}", usage())),
    }
    Ok(())
}
