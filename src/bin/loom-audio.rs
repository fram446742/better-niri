#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-audio", "loom-audio/Cargo.toml");
}
