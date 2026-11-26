#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-sidepanels", "loom-sidepanels/Cargo.toml");
}
