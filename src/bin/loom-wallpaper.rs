#[path = "shims/common.rs"]
mod common;

fn main() {
    common::run("loom-wallpaper", "loom-wallpaper/Cargo.toml");
}
