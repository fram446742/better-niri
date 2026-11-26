use fixture::Fixture;

mod client;
mod fixture;
mod server;
mod test_input_backend;

mod animations;
mod binds;
mod color_management;
mod commit_timing;
mod confine_mouse;
mod floating;
mod fullscreen;
#[cfg(feature = "navigation")]
mod grid_actions;
#[cfg(feature = "navigation")]
mod grid_activation;
#[cfg(feature = "navigation")]
mod grid_flow;
#[cfg(feature = "navigation")]
mod grid_focus_follows_mouse;
#[cfg(feature = "navigation")]
mod grid_input;
#[cfg(feature = "navigation")]
mod grid_scroll;

mod keyboard_config;
mod layer_shell;
mod remove_output;
mod renderer_cleanup;
mod suspended;
mod transactions;
mod virtual_output;
mod virtual_pointer;
mod window_opening;
