#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-shot", "loom-shot/Cargo.toml");
}
