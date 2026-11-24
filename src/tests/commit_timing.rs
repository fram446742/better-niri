//! Tests for the `wp-commit-timing-v1` wiring.

use std::time::Duration;

use smithay::utils::Size;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::ClientId;
use super::*;
use crate::layout::LayoutElement as _;
use crate::utils::get_monotonic_time;

fn create_window(f: &mut Fixture, id: ClientId, w: u16, h: u16) -> WlSurface {
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(w, h);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    surface
}

fn window_size(f: &Fixture) -> Size<i32, smithay::utils::Logical> {
    let niri = &f.state.server.state.niri;
    let (_, mapped) = niri
        .layout
        .windows()
        .find(|(_, mapped)| !mapped.is_minimized())
        .expect("no windows");
    mapped.size()
}

#[test]
fn global_is_advertised_and_bound() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    let id = f.add_client();
    f.double_roundtrip(id);

    assert!(
        f.client(id)
            .state
            .globals
            .iter()
            .any(|g| g.interface == "wp_commit_timing_manager_v1"),
        "wp_commit_timing_manager_v1 was not advertised"
    );
    assert!(
        f.client(id).state.commit_timing_manager.is_some(),
        "wp_commit_timing_manager_v1 was not bound"
    );
}

#[test]
fn past_timestamp_commit_applies() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    let id = f.add_client();
    let surface = create_window(&mut f, id, 100, 100);

    // Epoch in the presentation clock is always in the past.
    let timer = f.client(id).get_commit_timer(&surface);
    timer.set_timestamp(0, 0, 0);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(300, 300);
    window.commit();
    f.double_roundtrip(id);

    assert_eq!(window_size(&f), Size::from((300, 300)));
}

#[test]
fn future_timestamp_holds_commit() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    let id = f.add_client();
    let surface = create_window(&mut f, id, 100, 100);

    // A deadline far in the future must keep the commit blocked; nothing signals it yet.
    let deadline = get_monotonic_time() + Duration::from_secs(60);
    let timer = f.client(id).get_commit_timer(&surface);
    timer.set_timestamp(
        (deadline.as_secs() >> 32) as u32,
        deadline.as_secs() as u32,
        deadline.subsec_nanos(),
    );

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(300, 300);
    window.commit();
    f.double_roundtrip(id);

    assert_eq!(window_size(&f), Size::from((100, 100)));
}
