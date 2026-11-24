//! Validate the installed artifacts and the install/smoke scripts without
//! touching root-owned system directories.
//!
//! These tests parse the shipped `.portal`, D-Bus service, systemd unit, and
//! `portals.conf` snippet, and drive `scripts/install.sh` with overridden paths
//! into temporary staging roots (`--stage`, `--print-paths`, `--dry-run`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.better";
const PORTAL_OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    std::fs::read_to_string(manifest_dir().join(relative))
        .unwrap_or_else(|err| panic!("read {relative}: {err}"))
}

fn temp_dir(name: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "better-portal-artifacts-{name}-{}-{id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_script(name: &str, args: &[&str], envs: &[(&str, &Path)]) -> Output {
    let mut command = Command::new("bash");
    command
        .arg(manifest_dir().join("scripts").join(name))
        .args(args);
    for variable in [
        "PREFIX",
        "DESTDIR",
        "BINDIR",
        "DATADIR",
        "PORTALS_DIR",
        "DBUS_SERVICES_DIR",
        "SYSTEMD_USER_DIR",
        "PORTALS_CONF",
        "CARGO_TARGET_DIR",
        "XDG_CONFIG_HOME",
        "BETTER_PORTAL_BINDIR",
        "BETTER_PORTAL_DATADIR",
        "BETTER_PORTAL_PORTALS_DIR",
        "BETTER_PORTAL_DBUS_SERVICES_DIR",
        "BETTER_PORTAL_SYSTEMD_USER_DIR",
        "BETTER_PORTAL_PORTALS_CONF",
        "BETTER_PORTAL_BINARY_DIR",
        "BETTER_PORTAL_BIN",
    ] {
        command.env_remove(variable);
    }
    command.env("HOME", manifest_dir().join("target").join("test-home"));
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("run script")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed: {}\n{}",
        output.status,
        stderr(output)
    );
}

fn parse_key_values(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

#[derive(Debug, Default)]
struct Ini {
    sections: HashMap<String, HashMap<String, String>>,
}

impl Ini {
    fn parse(text: &str) -> Self {
        let mut ini = Self::default();
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                section = name.trim().to_owned();
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                ini.sections
                    .entry(section.clone())
                    .or_default()
                    .insert(key.trim().to_owned(), value.trim().to_owned());
            }
        }
        ini
    }

    fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections.get(section)?.get(key).map(String::as_str)
    }

    fn section(&self, section: &str) -> &HashMap<String, String> {
        self.sections
            .get(section)
            .unwrap_or_else(|| panic!("missing section [{section}] in {self:?}"))
    }
}

fn staged(stage: &Path, absolute: &str) -> PathBuf {
    stage.join(absolute.strip_prefix('/').expect("absolute path"))
}

#[test]
fn portal_descriptor_matches_the_backend() {
    let ini = Ini::parse(&read("better.portal"));

    assert_eq!(
        ini.get("portal", "DBusName"),
        Some(better_portal::BUS_NAME),
        "the .portal DBusName must match the served bus name"
    );
    assert_eq!(better_portal::BUS_NAME, BUS_NAME);
    assert_eq!(better_portal::OBJECT_PATH, PORTAL_OBJECT_PATH);

    let interfaces: Vec<&str> = ini
        .get("portal", "Interfaces")
        .expect("Interfaces")
        .split(';')
        .filter(|interface| !interface.is_empty())
        .collect();
    assert!(
        interfaces.contains(&better_portal::SCREENSHOT_INTERFACE),
        "missing Screenshot interface: {interfaces:?}"
    );
    assert!(
        interfaces.contains(&better_portal::SCREENCAST_INTERFACE),
        "missing ScreenCast interface: {interfaces:?}"
    );
    assert!(
        !interfaces
            .iter()
            .any(|interface| interface.contains("PickColor")),
        "PickColor is a Screenshot method, not an impl interface: {interfaces:?}"
    );

    let use_in: Vec<&str> = ini
        .get("portal", "UseIn")
        .expect("UseIn")
        .split(';')
        .filter(|entry| !entry.is_empty())
        .collect();
    assert!(
        use_in.contains(&"niri"),
        "UseIn must select niri: {use_in:?}"
    );
}

#[test]
fn dbus_service_and_systemd_unit_agree_with_the_portal() {
    let service = Ini::parse(&read(
        "packaging/org.freedesktop.impl.portal.desktop.better.service",
    ));
    assert_eq!(
        service.get("D-BUS Service", "Name"),
        Some(BUS_NAME),
        "D-Bus activation name"
    );
    assert_eq!(
        service.get("D-BUS Service", "Exec"),
        Some("@BETTER_PORTAL@"),
        "Exec must be rendered by install.sh"
    );
    let unit_name = service
        .get("D-BUS Service", "SystemdService")
        .expect("SystemdService");

    let unit = Ini::parse(&read(&format!("packaging/{unit_name}")));
    assert_eq!(
        unit.get("Service", "ExecStart"),
        Some("@BETTER_PORTAL@"),
        "unit ExecStart must be rendered by install.sh"
    );
    assert!(
        unit.get("Unit", "PartOf").is_some_and(|value| value
            .split(';')
            .any(|target| target == "graphical-session.target")),
        "unit should be tied to graphical-session.target"
    );
    assert!(
        unit.get("Install", "WantedBy").is_some_and(|value| value
            .split(';')
            .any(|target| target == "graphical-session.target")),
        "unit should be wanted by graphical-session.target"
    );
}

#[test]
fn portals_conf_prefers_better_for_screenshot_and_screencast() {
    let ini = Ini::parse(&read("packaging/portals.conf"));
    let preferred = ini.section("preferred");

    for interface in ["Screenshot", "ScreenCast"] {
        let key = format!("org.freedesktop.impl.portal.{interface}");
        let value = preferred
            .get(&key)
            .unwrap_or_else(|| panic!("missing {key} preference"));
        assert!(
            value
                .split(';')
                .any(|implementation| implementation == "better"),
            "{key} should prefer better: {value}"
        );
    }

    assert!(
        !preferred.contains_key("org.freedesktop.impl.portal.PickColor"),
        "PickColor is provided by the Screenshot interface; do not invent a key"
    );
}

#[test]
fn install_script_resolves_path_overrides_without_writing() {
    let tmp = temp_dir("print-paths");
    let bindir = tmp.join("bin");
    let datadir = tmp.join("share");
    let systemd_dir = tmp.join("systemd-user");
    let portals_conf = tmp.join("config/portals.conf");
    let binary_dir = tmp.join("release");

    let output = run_script(
        "install.sh",
        &["--print-paths"],
        &[
            ("BETTER_PORTAL_BINDIR", &bindir),
            ("BETTER_PORTAL_DATADIR", &datadir),
            ("BETTER_PORTAL_SYSTEMD_USER_DIR", &systemd_dir),
            ("BETTER_PORTAL_PORTALS_CONF", &portals_conf),
            ("BETTER_PORTAL_BINARY_DIR", &binary_dir),
        ],
    );
    assert_success(&output, "install.sh --print-paths");
    let paths = parse_key_values(&stdout(&output));

    assert_eq!(
        paths["PORTAL_BIN"],
        bindir.join("better-portal").display().to_string()
    );
    assert_eq!(
        paths["PICKER_BIN"],
        bindir.join("better-portal-picker").display().to_string()
    );
    assert_eq!(
        paths["PORTAL_FILE"],
        datadir
            .join("xdg-desktop-portal/portals/better.portal")
            .display()
            .to_string()
    );
    assert_eq!(
        paths["DBUS_SERVICE"],
        datadir
            .join(format!("dbus-1/services/{BUS_NAME}.service"))
            .display()
            .to_string()
    );
    assert_eq!(
        paths["SYSTEMD_UNIT"],
        systemd_dir
            .join("better-portal.service")
            .display()
            .to_string()
    );
    assert_eq!(paths["PORTALS_CONF"], portals_conf.display().to_string());
    assert_eq!(paths["BINARY_DIR"], binary_dir.display().to_string());

    assert!(!bindir.exists(), "--print-paths must not write binaries");
    assert!(!datadir.exists(), "--print-paths must not write data");
    assert!(!systemd_dir.exists(), "--print-paths must not write units");
    assert!(
        !portals_conf.exists(),
        "--print-paths must not write config"
    );
}

#[test]
fn staged_install_renders_services_with_the_overridden_binary_path() {
    let tmp = temp_dir("stage");
    let stage = tmp.join("root");
    let bindir = Path::new("/opt/better-portal-test/bin");
    let datadir = Path::new("/opt/better-portal-test/share");
    let systemd_dir = Path::new("/etc/systemd/user");
    let portals_conf = Path::new("/etc/xdg/xdg-desktop-portal/portals.conf");

    let output = run_script(
        "install.sh",
        &["--stage", stage.to_str().unwrap()],
        &[
            ("BETTER_PORTAL_BINDIR", bindir),
            ("BETTER_PORTAL_DATADIR", datadir),
            ("BETTER_PORTAL_SYSTEMD_USER_DIR", systemd_dir),
            ("BETTER_PORTAL_PORTALS_CONF", portals_conf),
        ],
    );
    assert_success(&output, "install.sh --stage");

    let portal = Ini::parse(&read_file(&staged(
        &stage,
        "/opt/better-portal-test/share/xdg-desktop-portal/portals/better.portal",
    )));
    assert_eq!(portal.get("portal", "DBusName"), Some(BUS_NAME));

    let service = Ini::parse(&read_file(&staged(
        &stage,
        &format!("/opt/better-portal-test/share/dbus-1/services/{BUS_NAME}.service"),
    )));
    assert_eq!(
        service.get("D-BUS Service", "Exec"),
        Some("/opt/better-portal-test/bin/better-portal")
    );

    let unit = Ini::parse(&read_file(&staged(
        &stage,
        "/etc/systemd/user/better-portal.service",
    )));
    assert_eq!(
        unit.get("Service", "ExecStart"),
        Some("/opt/better-portal-test/bin/better-portal")
    );

    let preferences = Ini::parse(&read_file(&staged(
        &stage,
        "/etc/xdg/xdg-desktop-portal/portals.conf",
    )));
    assert_eq!(
        preferences.get("preferred", "org.freedesktop.impl.portal.Screenshot"),
        Some("better;")
    );
    assert_eq!(
        preferences.get("preferred", "org.freedesktop.impl.portal.ScreenCast"),
        Some("better;")
    );

    let uninstall = run_script(
        "install.sh",
        &["--uninstall", "--stage", stage.to_str().unwrap()],
        &[
            ("BETTER_PORTAL_BINDIR", bindir),
            ("BETTER_PORTAL_DATADIR", datadir),
            ("BETTER_PORTAL_SYSTEMD_USER_DIR", systemd_dir),
            ("BETTER_PORTAL_PORTALS_CONF", portals_conf),
        ],
    );
    assert_success(&uninstall, "install.sh --uninstall --stage");
    assert!(
        !staged(
            &stage,
            "/opt/better-portal-test/share/dbus-1/services/org.freedesktop.impl.portal.desktop.better.service"
        )
        .exists(),
        "uninstall should remove the D-Bus service file"
    );
    assert!(
        !staged(&stage, "/etc/xdg/xdg-desktop-portal/portals.conf").exists(),
        "uninstall should remove a config file that install created"
    );
}

#[test]
fn staged_install_merges_an_existing_portals_conf_and_restores_it() {
    let tmp = temp_dir("merge");
    let stage = tmp.join("root");
    let config = staged(&stage, "/etc/xdg/xdg-desktop-portal/portals.conf");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let original = "[preferred]\n\
default=gnome;gtk;\n\
org.freedesktop.impl.portal.Screenshot=niri;\n\
org.freedesktop.impl.portal.Secret=oo7-portal;\n\
\n\
[extra]\n\
keep=yes\n";
    std::fs::write(&config, original).unwrap();

    let envs = [
        (
            "BETTER_PORTAL_BINDIR",
            Path::new("/opt/better-portal-test/bin"),
        ),
        (
            "BETTER_PORTAL_DATADIR",
            Path::new("/opt/better-portal-test/share"),
        ),
        (
            "BETTER_PORTAL_SYSTEMD_USER_DIR",
            Path::new("/etc/systemd/user"),
        ),
        (
            "BETTER_PORTAL_PORTALS_CONF",
            Path::new("/etc/xdg/xdg-desktop-portal/portals.conf"),
        ),
    ];

    let install = run_script("install.sh", &["--stage", stage.to_str().unwrap()], &envs);
    assert_success(&install, "install.sh merge");

    let merged = read_file(&config);
    let ini = Ini::parse(&merged);
    assert_eq!(ini.get("preferred", "default"), Some("gnome;gtk;"));
    assert_eq!(
        ini.get("preferred", "org.freedesktop.impl.portal.Screenshot"),
        Some("better;")
    );
    assert_eq!(
        ini.get("preferred", "org.freedesktop.impl.portal.ScreenCast"),
        Some("better;")
    );
    assert_eq!(
        ini.get("preferred", "org.freedesktop.impl.portal.Secret"),
        Some("oo7-portal;"),
        "unrelated preference must survive"
    );
    assert_eq!(ini.get("extra", "keep"), Some("yes"));
    assert!(
        read_file(&config.with_extension("conf.better-portal.bak")) == original,
        "merge must back up the original file"
    );

    let uninstall = run_script(
        "install.sh",
        &["--uninstall", "--stage", stage.to_str().unwrap()],
        &envs,
    );
    assert_success(&uninstall, "install.sh uninstall after merge");
    assert_eq!(
        read_file(&config),
        original,
        "uninstall must restore the pre-install portals.conf"
    );
}

#[test]
fn install_dry_run_prints_actions_and_writes_nothing() {
    let tmp = temp_dir("dry-run");
    let bindir = tmp.join("bin");
    let datadir = tmp.join("share");
    let systemd_dir = tmp.join("systemd-user");
    let portals_conf = tmp.join("config/portals.conf");

    let output = run_script(
        "install.sh",
        &["--dry-run", "--no-build", "--no-binaries"],
        &[
            ("BETTER_PORTAL_BINDIR", &bindir),
            ("BETTER_PORTAL_DATADIR", &datadir),
            ("BETTER_PORTAL_SYSTEMD_USER_DIR", &systemd_dir),
            ("BETTER_PORTAL_PORTALS_CONF", &portals_conf),
        ],
    );
    assert_success(&output, "install.sh --dry-run");
    let text = stdout(&output);
    assert!(text.contains("install -Dm644"), "{text}");
    assert!(
        text.contains(&format!("(Exec={}/better-portal)", bindir.display())),
        "rendered Exec path missing from plan: {text}"
    );
    assert!(text.contains("Dry run: wrote nothing."), "{text}");
    assert!(!bindir.exists() && !datadir.exists());
    assert!(!systemd_dir.exists() && !portals_conf.exists());
}

#[test]
fn smoke_script_has_a_dry_run_plan() {
    let tmp = temp_dir("smoke");
    let missing_binary = tmp.join("missing-better-portal");
    let output = run_script(
        "smoke.sh",
        &["--dry-run"],
        &[("BETTER_PORTAL_BIN", &missing_binary)],
    );
    assert_success(&output, "smoke.sh --dry-run");
    let text = stdout(&output);
    assert!(text.contains(BUS_NAME), "{text}");
    assert!(text.contains("Screenshot"), "{text}");
    assert!(text.contains("private"), "{text}");
    assert!(text.contains("no bus started"), "{text}");
    assert!(!missing_binary.exists());
}

fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}
