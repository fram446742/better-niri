#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("companionctl", "better-companion/Cargo.toml");
}
