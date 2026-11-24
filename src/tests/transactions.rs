use std::fmt::Write as _;

use insta::assert_snapshot;
use niri_ipc::SizeChange;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::ClientId;
use super::*;
use crate::layout::LayoutElement;
use crate::niri::Niri;

fn format_window_sizes(niri: &Niri) -> String {
    let mut buf = String::new();
    for (_out, mapped) in niri.layout.windows() {
        let size = mapped.size();
        writeln!(&mut buf, "{} × {}", size.w, size.h).unwrap();
    }
    buf
}

fn create_window(f: &mut Fixture, id: ClientId, w: u16, h: u16) -> WlSurface {
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(w, h);
    window.ack_last_and_commit();
    f.roundtrip(id);

    surface
}

fn window_sizes(niri: &Niri) -> Vec<(i32, i32)> {
    let mut sizes: Vec<_> = niri
        .layout
        .windows()
        .map(|(_, mapped)| {
            let size = mapped.size();
            (size.w, size.h)
        })
        .collect();
    sizes.sort();
    sizes
}

/// Sets up two 100×100 and 200×200 windows in one column and consumes one into the other, leaving
/// both windows settled before the test starts.
fn two_window_column(f: &mut Fixture, id: ClientId) -> (WlSurface, WlSurface) {
    let surface1 = create_window(f, id, 100, 100);
    let surface2 = create_window(f, id, 200, 200);
    f.double_roundtrip(id);

    let _ = f.client(id).window(&surface1).recent_configures();
    let _ = f.client(id).window(&surface2).recent_configures();

    f.niri().layout.consume_or_expel_window_left(None);
    f.double_roundtrip(id);

    let window = f.client(id).window(&surface1);
    let _ = window.recent_configures();
    window.ack_last_and_commit();
    let window = f.client(id).window(&surface2);
    let _ = window.recent_configures();
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    (surface1, surface2)
}

#[test]
fn surface_barriers_hold_until_all_participants_commit() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let (surface1, surface2) = two_window_column(&mut f, id);

    // Issue a resize, which creates a transaction spanning both windows.
    f.niri()
        .layout
        .set_column_width(SizeChange::AdjustFixed(10));
    f.double_roundtrip(id);

    // Commit only the first window. Its transaction is still waiting on the second window, so the
    // surface barrier is registered and the committed state must not be applied.
    let window = f.client(id).window(&surface1);
    window.set_size(300, 300);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    assert_eq!(window_sizes(f.niri()), vec![(100, 100), (200, 200)]);
    assert!(
        f.niri().surface_barriers.pending_count() > 0,
        "surface barrier should be registered while the transaction is pending"
    );

    // Commit the second window. Both surfaces become ready, so the barrier releases and applies
    // both committed states atomically.
    let window = f.client(id).window(&surface2);
    window.set_size(400, 400);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    assert_eq!(window_sizes(f.niri()), vec![(300, 300), (400, 400)]);
    assert_eq!(
        f.niri().surface_barriers.pending_count(),
        0,
        "released surface barriers should be dropped from the registry"
    );
}

#[test]
fn column_resize_waits_for_both_windows() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));
    let id = f.add_client();

    let surface1 = create_window(&mut f, id, 100, 100);
    let surface2 = create_window(&mut f, id, 200, 200);
    f.double_roundtrip(id);

    let _ = f.client(id).window(&surface1).recent_configures();
    let _ = f.client(id).window(&surface2).recent_configures();

    // Consume into one column.
    f.niri().layout.consume_or_expel_window_left(None);
    f.double_roundtrip(id);

    // Commit for the column consume.
    let window = f.client(id).window(&surface1);
    assert_snapshot!(
        window.format_recent_configures(),
        @"size: 936 × 516, bounds: 1888 × 1048, states: []"
    );
    window.ack_last_and_commit();

    let window = f.client(id).window(&surface2);
    assert_snapshot!(
        window.format_recent_configures(),
        @"size: 936 × 516, bounds: 1888 × 1048, states: [Activated]"
    );
    window.ack_last_and_commit();

    f.double_roundtrip(id);

    // This should say 100 × 100 and 200 × 200.
    assert_snapshot!(format_window_sizes(f.niri()), @r"
    100 × 100
    200 × 200
    ");

    // Issue a resize.
    f.niri()
        .layout
        .set_column_width(SizeChange::AdjustFixed(10));
    f.double_roundtrip(id);

    // Commit window 1 in response to resize.
    let window = f.client(id).window(&surface1);
    window.set_size(300, 300);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    // This should still say 100 × 100 as we're waiting in a transaction for the second window.
    assert_snapshot!(format_window_sizes(f.niri()), @r"
    100 × 100
    200 × 200
    ");

    // Commit window 2 in response to resize.
    let window = f.client(id).window(&surface2);
    window.set_size(400, 400);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    // This should say 300 × 300 and 400 × 400 as the transaction completed.
    assert_snapshot!(format_window_sizes(f.niri()), @r"
    300 × 300
    400 × 400
    ");
}
