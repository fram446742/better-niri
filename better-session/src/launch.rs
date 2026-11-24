//! Resolving how to launch an app that is missing from a restored context.
//!
//! Resolution is deterministic and side-effect free: an explicit
//! `command_map` entry wins, then an installed Flatpak app id, then an
//! XDG `.desktop` file (`Exec=`, field codes stripped), then an executable on
//! `PATH`. Callers can inject the data/PATH directories and the set of
//! installed Flatpaks, which keeps tests independent from the host.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where a resolved launch command came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchSource {
    CommandMap,
    Flatpak,
    DesktopFile,
    Executable,
}

impl LaunchSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CommandMap => "command_map",
            Self::Flatpak => "flatpak",
            Self::DesktopFile => "desktop",
            Self::Executable => "executable",
        }
    }
}

/// A command that can be handed to niri's `spawn-sh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLaunch {
    pub command: String,
    pub source: LaunchSource,
    /// Desktop file path or Flatpak id, for reports.
    pub detail: Option<String>,
}

/// Environment used for launch resolution.
///
/// `detect()` reads the process environment. Tests build it directly so no
/// host `.desktop` file or installed app can change the result.
#[derive(Debug, Clone, Default)]
pub struct LaunchEnv {
    /// XDG data roots (without the trailing `applications`).
    pub data_dirs: Vec<PathBuf>,
    /// Directories searched like `PATH`.
    pub path_dirs: Vec<PathBuf>,
    /// Installed Flatpak application ids. `None` discovers them from the
    /// Flatpak export directories below `data_dirs`.
    pub flatpak_apps: Option<BTreeSet<String>>,
}

fn env_paths(key: &str) -> Vec<PathBuf> {
    std::env::var_os(key)
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

impl LaunchEnv {
    /// Reads XDG data dirs, `PATH`, and Flatpak export locations.
    pub fn detect() -> Self {
        let mut data_dirs = Vec::new();
        if let Some(home) = home_dir() {
            data_dirs.push(home.join(".local/share"));
        }
        if let Some(home) = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".local/share")))
            && !data_dirs.contains(&home)
        {
            data_dirs.insert(0, home);
        }
        for dir in env_paths("XDG_DATA_DIRS") {
            if !data_dirs.contains(&dir) {
                data_dirs.push(dir);
            }
        }
        data_dirs.push(PathBuf::from("/usr/local/share"));
        data_dirs.push(PathBuf::from("/usr/share"));
        if let Some(home) = home_dir() {
            let flatpak = home.join(".local/share/flatpak/exports/share");
            if !data_dirs.contains(&flatpak) {
                data_dirs.push(flatpak);
            }
        }
        data_dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));

        let path_dirs = env_paths("PATH");
        Self {
            data_dirs,
            path_dirs,
            flatpak_apps: None,
        }
    }

    /// A resolver rooted in explicit directories.
    pub fn with_dirs(data_dirs: Vec<PathBuf>, path_dirs: Vec<PathBuf>) -> Self {
        Self {
            data_dirs,
            path_dirs,
            flatpak_apps: None,
        }
    }

    fn installed_flatpaks(&self) -> BTreeSet<String> {
        if let Some(apps) = &self.flatpak_apps {
            return apps.clone();
        }
        let mut apps = BTreeSet::new();
        for dir in &self.data_dirs {
            let applications = dir.join("flatpak/exports/share/applications");
            let Ok(entries) = std::fs::read_dir(applications) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("desktop")
                    && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
                {
                    apps.insert(stem.to_owned());
                }
            }
        }
        apps
    }

    /// Whether `app_id` is an installed Flatpak application.
    pub fn flatpak_installed(&self, app_id: &str) -> bool {
        self.installed_flatpaks().contains(app_id)
    }

    /// Whether a native executable named `app_id` exists on `path_dirs`.
    pub fn native_binary(&self, app_id: &str) -> bool {
        is_executable_in(&self.path_dirs, app_id)
    }

    /// Resolves the launch command for `app_id`.
    ///
    /// The empty string in `command_map` is treated as "not configured".
    pub fn resolve(
        &self,
        app_id: &str,
        command_map: &BTreeMap<String, String>,
    ) -> Option<ResolvedLaunch> {
        if app_id.trim().is_empty() {
            return None;
        }
        if let Some(command) = command_map.get(app_id)
            && !command.trim().is_empty()
        {
            return Some(ResolvedLaunch {
                command: command.clone(),
                source: LaunchSource::CommandMap,
                detail: None,
            });
        }

        // A Flatpak app id is only used when no native binary shadows it.
        if self.flatpak_installed(app_id) && !self.native_binary(app_id) {
            return Some(ResolvedLaunch {
                command: format!("flatpak run {app_id}"),
                source: LaunchSource::Flatpak,
                detail: Some(app_id.to_owned()),
            });
        }

        if let Some((path, exec)) = self.desktop_exec(app_id) {
            return Some(ResolvedLaunch {
                command: exec,
                source: LaunchSource::DesktopFile,
                detail: Some(path.display().to_string()),
            });
        }

        if self.native_binary(app_id) {
            return Some(ResolvedLaunch {
                command: app_id.to_owned(),
                source: LaunchSource::Executable,
                detail: None,
            });
        }
        None
    }

    fn desktop_exec(&self, app_id: &str) -> Option<(PathBuf, String)> {
        let mut exact: Option<(PathBuf, String)> = None;
        let mut wm_class: Option<(PathBuf, String)> = None;
        for dir in &self.data_dirs {
            let Ok(entries) = std::fs::read_dir(dir.join("applications")) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
                    continue;
                }
                let Some(entry_data) = parse_desktop_file(&path) else {
                    continue;
                };
                let Some(exec) = entry_data.exec else {
                    continue;
                };
                let stem = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or_default();
                if stem.eq_ignore_ascii_case(app_id) {
                    exact = Some((path, exec));
                    break;
                }
                if entry_data
                    .startup_wm_class
                    .as_deref()
                    .is_some_and(|class| class.eq_ignore_ascii_case(app_id))
                    && wm_class.is_none()
                {
                    wm_class = Some((path, exec));
                }
            }
            if exact.is_some() {
                break;
            }
        }
        exact.or(wm_class)
    }
}

struct DesktopEntry {
    exec: Option<String>,
    startup_wm_class: Option<String>,
}

fn parse_desktop_file(path: &Path) -> Option<DesktopEntry> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let mut exec = None;
    let mut startup_wm_class = None;
    let mut hidden = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if in_entry {
                break;
            }
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "Exec" if exec.is_none() => exec = Some(strip_field_codes(value.trim())),
                "StartupWMClass" => startup_wm_class = Some(value.trim().to_owned()),
                "Hidden" => hidden = value.trim().eq_ignore_ascii_case("true"),
                "NoDisplay" => hidden = value.trim().eq_ignore_ascii_case("true"),
                _ => (),
            }
        }
    }
    if hidden {
        return None;
    }
    let exec = exec.filter(|exec| !exec.is_empty());
    Some(DesktopEntry {
        exec,
        startup_wm_class,
    })
}

/// Removes freedesktop field codes from an `Exec=` value.
///
/// `%%` becomes a literal `%`; other `%x` codes are dropped.
pub fn strip_field_codes(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some(_) => {
                chars.next();
            }
            None => (),
        }
    }
    out.trim().to_owned()
}

fn is_executable_in(dirs: &[PathBuf], name: &str) -> bool {
    if name.contains('/') {
        return is_executable(Path::new(name));
    }
    dirs.iter().any(|dir| is_executable(&dir.join(name)))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "better-session-launch-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_desktop(root: &Path, id: &str, body: &str) -> PathBuf {
        let dir = root.join("applications");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.desktop"));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    fn write_executable(dir: &Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
    }

    #[test]
    fn strips_field_codes() {
        assert_eq!(strip_field_codes("firefox %u"), "firefox".to_owned());
        assert_eq!(
            strip_field_codes("app --flag %U --literal %%"),
            "app --flag  --literal %"
        );
        assert_eq!(strip_field_codes("app %f %F %i %c %k"), "app");
    }

    #[test]
    fn command_map_wins_over_desktop_and_flatpak() {
        let root = temp_dir("command-map");
        write_desktop(&root, "firefox", "[Desktop Entry]\nExec=firefox %u\n");
        let env = LaunchEnv {
            data_dirs: vec![root],
            path_dirs: vec![],
            flatpak_apps: Some(BTreeSet::from(["firefox".to_owned()])),
        };
        let mut commands = BTreeMap::new();
        commands.insert("firefox".to_owned(), "my-browser --new".to_owned());
        let resolved = env.resolve("firefox", &commands).unwrap();
        assert_eq!(resolved.command, "my-browser --new");
        assert_eq!(resolved.source, LaunchSource::CommandMap);
    }

    #[test]
    fn resolves_desktop_exec_and_startup_wm_class() {
        let root = temp_dir("desktop");
        let path = write_desktop(
            &root,
            "org.example.Editor",
            "[Desktop Entry]\nExec=editor --new-window %F\nStartupWMClass=editor\n",
        );
        let env = LaunchEnv::with_dirs(vec![root], vec![]);
        let resolved = env.resolve("editor", &BTreeMap::new()).unwrap();
        assert_eq!(resolved.command, "editor --new-window");
        assert_eq!(resolved.source, LaunchSource::DesktopFile);
        assert_eq!(resolved.detail.as_deref(), Some(path.to_str().unwrap()));
    }

    #[test]
    fn flatpak_is_used_only_without_a_native_binary() {
        let root = temp_dir("flatpak");
        let bin = root.join("bin");
        let env = LaunchEnv {
            data_dirs: vec![root.clone()],
            path_dirs: vec![bin.clone()],
            flatpak_apps: Some(BTreeSet::from(["org.example.Game".to_owned()])),
        };
        let resolved = env.resolve("org.example.Game", &BTreeMap::new()).unwrap();
        assert_eq!(resolved.command, "flatpak run org.example.Game");
        assert_eq!(resolved.source, LaunchSource::Flatpak);

        write_executable(&bin, "org.example.Game");
        assert_eq!(
            env.resolve("org.example.Game", &BTreeMap::new())
                .unwrap()
                .source,
            LaunchSource::Executable
        );
    }

    #[test]
    fn detects_flatpak_exports_without_running_flatpak() {
        let root = temp_dir("flatpak-export");
        let exports = root.join("flatpak/exports/share/applications");
        std::fs::create_dir_all(&exports).unwrap();
        std::fs::write(
            exports.join("org.example.App.desktop"),
            "[Desktop Entry]\nExec=/usr/bin/flatpak run --branch=stable org.example.App\n",
        )
        .unwrap();
        let env = LaunchEnv::with_dirs(vec![root], vec![]);
        assert!(env.flatpak_installed("org.example.App"));
        assert_eq!(
            env.resolve("org.example.App", &BTreeMap::new())
                .unwrap()
                .command,
            "flatpak run org.example.App"
        );
    }

    #[test]
    fn unknown_apps_do_not_resolve() {
        let env = LaunchEnv::with_dirs(vec![temp_dir("unknown")], vec![]);
        assert!(
            env.resolve("definitely-not-installed", &BTreeMap::new())
                .is_none()
        );
        assert!(env.resolve("", &BTreeMap::new()).is_none());
    }
}
