#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-portal-picker", "loom-portal/Cargo.toml");
}
