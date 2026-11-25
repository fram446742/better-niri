#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("better-settings", "better-settings/Cargo.toml");
}
