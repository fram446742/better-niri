#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-portal", "loom-portal/Cargo.toml");
}
