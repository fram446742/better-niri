#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-settingsctl", "loom-settings/Cargo.toml");
}
