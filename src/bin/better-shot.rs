#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("better-shot", "better-shot/Cargo.toml");
}
