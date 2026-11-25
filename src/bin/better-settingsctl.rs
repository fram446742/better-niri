#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("better-settingsctl", "better-settings/Cargo.toml");
}
