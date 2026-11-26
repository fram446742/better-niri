#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-session", "loom-session/Cargo.toml");
}
