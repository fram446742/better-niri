#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-settings", "loom-settings/Cargo.toml");
}
