#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("better-companion", "better-companion/Cargo.toml");
}
