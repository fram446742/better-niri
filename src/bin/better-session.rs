#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("better-session", "better-session/Cargo.toml");
}
