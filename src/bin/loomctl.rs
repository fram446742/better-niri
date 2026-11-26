#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loomctl", "loom-companion/Cargo.toml");
}
