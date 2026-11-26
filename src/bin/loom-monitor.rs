#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-monitor", "loom-monitor/Cargo.toml");
}
