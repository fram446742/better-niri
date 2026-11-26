#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-companion", "loom-companion/Cargo.toml");
}
