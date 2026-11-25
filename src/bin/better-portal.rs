#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("better-portal", "better-portal/Cargo.toml");
}
