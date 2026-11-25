#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("better-monitor", "better-monitor/Cargo.toml");
}
