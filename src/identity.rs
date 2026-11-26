//! Runtime identity for the compositor.
//!
//! The same binary can be installed as `niri` or as `loom-niri`. The program
//! name selects the XDG directories, IPC socket, portal bus and session unit
//! so a Loom session can coexist with an installed niri.

use std::ffi::OsStr;
use std::path::Path;
use std::sync::OnceLock;

/// One compositor identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// Executable name, also used in help and version output.
    pub program: &'static str,
    /// XDG project name (`~/.config/<project>`, state and cache dirs).
    pub project: &'static str,
    /// Prefix of the IPC socket inside `$XDG_RUNTIME_DIR`.
    pub socket_prefix: &'static str,
    /// Environment variable holding the IPC socket path.
    pub socket_env: &'static str,
    /// Environment variable holding the config path.
    pub config_env: &'static str,
    /// `XDG_CURRENT_DESKTOP` for the session.
    pub desktop: &'static str,
    /// D-Bus name used by the built-in portal backend.
    pub portal_bus: &'static str,
    /// User session binary.
    pub session: &'static str,
    /// systemd user unit.
    pub service: &'static str,
    /// systemd shutdown target.
    pub shutdown_target: &'static str,
    /// dinit service name.
    pub dinit_service: &'static str,
    /// System-wide config path.
    pub system_config: &'static str,
}

/// Identity used by the coexisting `loom-niri` package.
pub const LOOM: Identity = Identity {
    program: "loom-niri",
    project: "loom-niri",
    socket_prefix: "loom-niri",
    socket_env: "LOOM_NIRI_SOCKET",
    config_env: "LOOM_NIRI_CONFIG",
    desktop: "loom-niri",
    portal_bus: "org.freedesktop.impl.portal.desktop.loom-niri",
    session: "loom-niri-session",
    service: "loom-niri.service",
    shutdown_target: "loom-niri-shutdown.target",
    dinit_service: "loom-niri",
    system_config: "/etc/loom-niri/config.kdl",
};

/// Identity used by the package that replaces niri.
pub const NIRI: Identity = Identity {
    program: "niri",
    project: "niri",
    socket_prefix: "niri",
    socket_env: "NIRI_SOCKET",
    config_env: "NIRI_CONFIG",
    desktop: "niri",
    portal_bus: "org.freedesktop.impl.portal.desktop.niri",
    session: "niri-session",
    service: "niri.service",
    shutdown_target: "niri-shutdown.target",
    dinit_service: "niri",
    system_config: "/etc/niri/config.kdl",
};

impl Identity {
    /// The environment variable to try first, then the compatibility name.
    pub const fn socket_envs(self) -> [&'static str; 2] {
        [self.socket_env, "NIRI_SOCKET"]
    }
}

/// Identity selected by the executable name.
pub fn current() -> &'static Identity {
    static CURRENT: OnceLock<&'static Identity> = OnceLock::new();
    CURRENT.get_or_init(|| from_arg0(std::env::args_os().next().as_deref()))
}

/// Detects an identity from an argv[0]-style path.
///
/// `LOOM_IDENTITY` can force `loom-niri` or `niri`, which is useful for a
/// source build that has not been symlinked yet.
pub fn from_arg0(arg0: Option<&OsStr>) -> &'static Identity {
    if let Some(forced) = std::env::var_os("LOOM_IDENTITY") {
        match forced.to_str() {
            Some("loom-niri" | "loom") => return &LOOM,
            Some("niri") => return &NIRI,
            _ => {}
        }
    }

    let name = arg0
        .and_then(|arg0| Path::new(arg0).file_name())
        .and_then(OsStr::to_str);

    if name == Some(LOOM.program) {
        &LOOM
    } else {
        &NIRI
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv0_selects_the_identity() {
        assert_eq!(from_arg0(Some(OsStr::new("/usr/bin/loom-niri"))), &LOOM);
        assert_eq!(from_arg0(Some(OsStr::new("/usr/bin/niri"))), &NIRI);
        assert_eq!(from_arg0(Some(OsStr::new("target/debug/loom-niri"))), &LOOM);
        assert_eq!(from_arg0(None), &NIRI);
    }

    #[test]
    fn both_identities_keep_the_niri_socket_compatibility() {
        assert_eq!(LOOM.socket_envs(), ["LOOM_NIRI_SOCKET", "NIRI_SOCKET"]);
        assert_eq!(NIRI.socket_envs(), ["NIRI_SOCKET", "NIRI_SOCKET"]);
    }
}
