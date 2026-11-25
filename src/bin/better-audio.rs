#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("better-audio", "better-audio/Cargo.toml");
}
