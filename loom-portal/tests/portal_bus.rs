mod common;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_io::Timer;
use common::*;
use futures_util::StreamExt as _;
use zbus::Proxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

async fn create_session(
    screencast: &Proxy<'static>,
    session: &OwnedObjectPath,
) -> zbus::Result<(u32, HashMap<String, OwnedValue>)> {
    screencast
        .call(
            "CreateSession",
            &(
                path(&unique_name("request")),
                session.clone(),
                "app.id".to_owned(),
                options(None),
            ),
        )
        .await
}

async fn select_sources(
    screencast: &Proxy<'static>,
    session: &OwnedObjectPath,
    types: u32,
) -> zbus::Result<(u32, HashMap<String, OwnedValue>)> {
    screencast
        .call(
            "SelectSources",
            &(
                path(&unique_name("request")),
                session.clone(),
                "app.id".to_owned(),
                options(Some(types)),
            ),
        )
        .await
}

async fn start_session(
    screencast: &Proxy<'static>,
    session: &OwnedObjectPath,
) -> zbus::Result<(u32, HashMap<String, OwnedValue>)> {
    screencast
        .call(
            "Start",
            &(
                path(&unique_name("request")),
                session.clone(),
                "app.id".to_owned(),
                String::new(),
                options(None),
            ),
        )
        .await
}

#[test]
fn screencast_full_lifecycle_and_local_close() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let _portal = Portal::start(&bus, None).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        assert_eq!(screencast.get_property::<u32>("Version").await.unwrap(), 4);
        assert_eq!(
            screencast
                .get_property::<u32>("AvailableSourceTypes")
                .await
                .unwrap(),
            SOURCE_TYPE_MONITOR | SOURCE_TYPE_WINDOW
        );
        assert_eq!(
            screencast
                .get_property::<u32>("AvailableCursorModes")
                .await
                .unwrap(),
            7
        );

        let session = path(&unique_name("session"));
        let (code, results) = create_session(&screencast, &session).await.unwrap();
        assert_eq!(code, RESPONSE_SUCCESS);
        assert!(results.contains_key("session_id"));

        let (code, _) = select_sources(&screencast, &session, SOURCE_TYPE_MONITOR)
            .await
            .unwrap();
        assert_eq!(code, RESPONSE_SUCCESS);

        let (code, results) = start_session(&screencast, &session).await.unwrap();
        assert_eq!(code, RESPONSE_SUCCESS);
        assert!(results.contains_key("streams"));

        assert_eq!(niri.calls(), ["CreateSession", "SelectSources", "Start"]);

        let session = session_proxy(&connection, &session).await;
        let mut closed = session.receive_signal("Closed").await.unwrap();
        session.call::<_, _, ()>("Close", &()).await.unwrap();

        let signal = with_timeout(closed.next(), 5).await;
        assert!(signal.is_some(), "the session must emit Closed");
        assert_eq!(niri.session_close_calls(), 1);
        assert!(
            session.call::<_, _, ()>("Close", &()).await.is_err(),
            "the session object must be gone after Close"
        );
    });
}

#[test]
fn screencast_session_closed_by_the_compositor() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let _portal = Portal::start(&bus, None).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let session_path = path(&unique_name("session"));
        let (code, _) = create_session(&screencast, &session_path).await.unwrap();
        assert_eq!(code, RESPONSE_SUCCESS);

        let session = session_proxy(&connection, &session_path).await;
        let mut closed = session.receive_signal("Closed").await.unwrap();

        // Let the portal install its niri-side signal match rule.
        Timer::after(Duration::from_millis(300)).await;
        niri.close_session(&session_path).await;

        let signal = with_timeout(closed.next(), 5).await;
        assert!(signal.is_some(), "loom-portal must re-emit niri's Closed");
        assert!(
            session.call::<_, _, ()>("Close", &()).await.is_err(),
            "the re-exported session object must be removed"
        );
    });
}

#[test]
fn screencast_session_is_closed_when_niri_vanishes() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let _portal = Portal::start(&bus, None).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let session_path = path(&unique_name("session"));
        let (code, _) = create_session(&screencast, &session_path).await.unwrap();
        assert_eq!(code, RESPONSE_SUCCESS);

        let session = session_proxy(&connection, &session_path).await;
        let mut closed = session.receive_signal("Closed").await.unwrap();
        Timer::after(Duration::from_millis(300)).await;

        let niri_connection = niri.connection.clone();
        drop(niri);
        niri_connection.close().await.unwrap();

        let signal = with_timeout(closed.next(), 10).await;
        assert!(
            signal.is_some(),
            "losing niri must close the re-exported session"
        );
    });
}

#[test]
fn screencast_monitor_picker_sets_and_clears_the_selection() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let dir = temp_dir("picker-monitor");
        let picker = script(
            &dir,
            "picker",
            "#!/bin/sh\nif [ \"$1\" = \"--screencast\" ]; then printf '{\"output\":\"eDP-1\"}'\nelse exit 9\nfi\n",
        );
        let _portal = Portal::start(&bus, Some(&picker)).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let session = path(&unique_name("session"));
        assert_eq!(create_session(&screencast, &session).await.unwrap().0, 0);
        assert_eq!(
            select_sources(&screencast, &session, SOURCE_TYPE_MONITOR)
                .await
                .unwrap()
                .0,
            0
        );
        assert_eq!(start_session(&screencast, &session).await.unwrap().0, 0);

        let state = niri.state.lock().unwrap();
        assert_eq!(
            state.set_selection,
            vec![(
                session.clone(),
                vec![(SOURCE_TYPE_MONITOR, "eDP-1".to_owned())],
                0
            )]
        );
        assert_eq!(state.clear_selection, vec![session.clone()]);
    });
}

#[test]
fn screencast_window_picker_sets_a_window_selection() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let dir = temp_dir("picker-window");
        let picker = script(
            &dir,
            "picker",
            "#!/bin/sh\nif [ \"$1\" = \"--screencast-window\" ]; then printf '{\"window\":42}'\nelse exit 9\nfi\n",
        );
        let _portal = Portal::start(&bus, Some(&picker)).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let session = path(&unique_name("session"));
        assert_eq!(create_session(&screencast, &session).await.unwrap().0, 0);
        assert_eq!(
            select_sources(&screencast, &session, SOURCE_TYPE_WINDOW)
                .await
                .unwrap()
                .0,
            0
        );
        assert_eq!(start_session(&screencast, &session).await.unwrap().0, 0);

        let state = niri.state.lock().unwrap();
        assert_eq!(
            state.set_selection,
            vec![(
                session.clone(),
                vec![(SOURCE_TYPE_WINDOW, "42".to_owned())],
                0
            )]
        );
        assert_eq!(state.clear_selection, vec![session.clone()]);
    });
}

#[test]
fn screencast_virtual_source_uses_the_output_picker() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let dir = temp_dir("picker-virtual");
        let picker = script(
            &dir,
            "picker",
            "#!/bin/sh\nif [ \"$1\" = \"--screencast\" ]; then printf '{\"output\":\"HEADLESS-1\"}'\nelse exit 9\nfi\n",
        );
        let _portal = Portal::start(&bus, Some(&picker)).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let session = path(&unique_name("session"));
        assert_eq!(create_session(&screencast, &session).await.unwrap().0, 0);
        assert_eq!(
            select_sources(&screencast, &session, SOURCE_TYPE_VIRTUAL)
                .await
                .unwrap()
                .0,
            0
        );
        assert_eq!(start_session(&screencast, &session).await.unwrap().0, 0);

        let state = niri.state.lock().unwrap();
        assert_eq!(
            state.set_selection,
            vec![(
                session.clone(),
                vec![(SOURCE_TYPE_MONITOR, "HEADLESS-1".to_owned())],
                0
            )]
        );
    });
}

#[test]
fn screencast_picker_cancel_cancels_start() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let dir = temp_dir("picker-cancel");
        let picker = script(&dir, "picker", "#!/bin/sh\nexit 2\n");
        let _portal = Portal::start(&bus, Some(&picker)).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let session = path(&unique_name("session"));
        assert_eq!(create_session(&screencast, &session).await.unwrap().0, 0);
        assert_eq!(
            select_sources(&screencast, &session, SOURCE_TYPE_MONITOR)
                .await
                .unwrap()
                .0,
            0
        );

        let (code, _) = start_session(&screencast, &session).await.unwrap();
        assert_eq!(code, RESPONSE_CANCELLED);
        assert!(
            !niri.calls().iter().any(|call| call == "Start"),
            "niri must not start a cast after a picker cancellation"
        );
    });
}

#[test]
fn screencast_invalid_picker_selection_falls_back_to_niri() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let dir = temp_dir("picker-invalid");
        // A monitor-only request receives a window: the selection is rejected
        // and niri's own picker is used instead.
        let picker = script(
            &dir,
            "picker",
            "#!/bin/sh\nif [ \"$1\" = \"--screencast\" ]; then printf '{\"window\":5}'\nelse exit 9\nfi\n",
        );
        let _portal = Portal::start(&bus, Some(&picker)).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let session = path(&unique_name("session"));
        assert_eq!(create_session(&screencast, &session).await.unwrap().0, 0);
        assert_eq!(
            select_sources(&screencast, &session, SOURCE_TYPE_MONITOR)
                .await
                .unwrap()
                .0,
            0
        );
        assert_eq!(start_session(&screencast, &session).await.unwrap().0, 0);

        let state = niri.state.lock().unwrap();
        assert!(state.set_selection.is_empty());
        assert!(state.clear_selection.is_empty());
        assert!(state.calls.iter().any(|call| call == "Start"));
    });
}

#[test]
fn screencast_start_is_cancellable_while_the_picker_is_open() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let dir = temp_dir("picker-close");
        let picker = script(&dir, "picker", "#!/bin/sh\nsleep 30\n");
        let _portal = Portal::start(&bus, Some(&picker)).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let session = path(&unique_name("session"));
        assert_eq!(create_session(&screencast, &session).await.unwrap().0, 0);
        assert_eq!(
            select_sources(&screencast, &session, SOURCE_TYPE_MONITOR)
                .await
                .unwrap()
                .0,
            0
        );

        let handle = path(&unique_name("request"));
        let started = Instant::now();
        let body = (
            handle.clone(),
            session.clone(),
            "app.id".to_owned(),
            String::new(),
            options(None),
        );
        let start_call =
            screencast.call::<_, _, (u32, HashMap<String, OwnedValue>)>("Start", &body);
        let cancel = async {
            Timer::after(Duration::from_millis(300)).await;
            let request = request_proxy(&connection, &handle).await;
            request.call::<_, _, ()>("Close", &()).await.unwrap();
        };

        let (result, ()) = with_timeout(futures_util::future::join(start_call, cancel), 10).await;
        assert_eq!(result.unwrap().0, RESPONSE_CANCELLED);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancellation took {:?}",
            started.elapsed()
        );
        assert!(
            !niri.calls().iter().any(|call| call == "Start"),
            "the cancelled Start must not be forwarded"
        );
        assert!(
            request_proxy(&connection, &handle)
                .await
                .call::<_, _, ()>("Close", &())
                .await
                .is_err(),
            "the Start request object must be cleaned up"
        );
    });
}

#[test]
fn screencast_niri_unavailable_is_mapped_to_an_error_response() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let _portal = Portal::start(&bus, None).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let session = path(&unique_name("session"));
        let (code, results) = create_session(&screencast, &session).await.unwrap();
        assert_eq!(code, RESPONSE_OTHER);
        assert!(results.contains_key("error"));
        assert!(
            session_proxy(&connection, &session)
                .await
                .call::<_, _, ()>("Close", &())
                .await
                .is_err(),
            "a failed CreateSession must not leave a session object behind"
        );
    });
}

#[test]
fn screencast_rejects_unknown_sessions_and_invalid_source_types() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let _portal = Portal::start(&bus, None).await;
        let connection = connect(&bus.address).await;
        let screencast = portal_proxy(&connection, SCREENCAST_INTERFACE).await;

        let unknown = path(&unique_name("session"));
        assert!(
            select_sources(&screencast, &unknown, SOURCE_TYPE_MONITOR)
                .await
                .is_err(),
            "SelectSources must reject an unknown session"
        );
        assert!(
            start_session(&screencast, &unknown).await.is_err(),
            "Start must reject an unknown session"
        );

        let session = path(&unique_name("session"));
        assert_eq!(create_session(&screencast, &session).await.unwrap().0, 0);
        let (code, results) = select_sources(&screencast, &session, 0).await.unwrap();
        assert_eq!(code, RESPONSE_OTHER);
        assert!(results.contains_key("error"));
        assert!(!niri.calls().iter().any(|call| call == "SelectSources"));
    });
}

#[test]
fn screenshot_and_pick_color_use_request_objects() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let _niri = FakeNiri::start(&bus).await;
        let _portal = Portal::start(&bus, None).await;
        let connection = connect(&bus.address).await;
        let screenshot = portal_proxy(&connection, SCREENSHOT_INTERFACE).await;
        assert_eq!(screenshot.get_property::<u32>("Version").await.unwrap(), 2);

        let handle = path(&unique_name("request"));
        let (code, results): (u32, HashMap<String, OwnedValue>) = screenshot
            .call(
                "Screenshot",
                &(
                    handle.clone(),
                    "app.id".to_owned(),
                    String::new(),
                    HashMap::<String, OwnedValue>::new(),
                ),
            )
            .await
            .unwrap();
        assert_eq!(code, RESPONSE_SUCCESS);
        let uri = results["uri"].downcast_ref::<String>().unwrap();
        assert!(uri.starts_with("file:///tmp/loom-portal-fake-screenshot.png"));
        assert!(
            request_proxy(&connection, &handle)
                .await
                .call::<_, _, ()>("Close", &())
                .await
                .is_err(),
            "the Screenshot request object must be removed after the reply"
        );

        let handle = path(&unique_name("request"));
        let (code, results): (u32, HashMap<String, OwnedValue>) = screenshot
            .call(
                "PickColor",
                &(
                    handle.clone(),
                    "app.id".to_owned(),
                    String::new(),
                    HashMap::<String, OwnedValue>::new(),
                ),
            )
            .await
            .unwrap();
        assert_eq!(code, RESPONSE_SUCCESS, "results: {results:?}");
        let color = <(f64, f64, f64)>::try_from(results["color"].try_clone().unwrap()).unwrap();
        assert_eq!(color, (0.25, 0.5, 0.75));
        assert!(
            request_proxy(&connection, &handle)
                .await
                .call::<_, _, ()>("Close", &())
                .await
                .is_err()
        );
    });
}

#[test]
fn interactive_screenshot_uses_the_region_picker() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let dir = temp_dir("screenshot-region");
        let picker = script(
            &dir,
            "picker",
            "#!/bin/sh\nprintf '{\"x\":100,\"y\":200,\"width\":300,\"height\":400}'\n",
        );
        let _portal = Portal::start(&bus, Some(&picker)).await;
        let connection = connect(&bus.address).await;
        let screenshot = portal_proxy(&connection, SCREENSHOT_INTERFACE).await;

        let mut options = HashMap::new();
        options.insert("interactive".to_owned(), owned(true));
        let (code, results) = screenshot_call(&screenshot, options).await.unwrap();
        assert_eq!(code, RESPONSE_SUCCESS);
        assert!(results.contains_key("uri"));

        let state = niri.state.lock().unwrap();
        assert_eq!(state.area_calls, vec![(100, 200, 300, 400)]);
    });
}

#[test]
fn interactive_screenshot_can_be_cancelled_with_the_request() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        let _portal = Portal::start(&bus, Some(std::path::Path::new("/nonexistent/picker"))).await;
        let connection = connect(&bus.address).await;
        let screenshot = portal_proxy(&connection, SCREENSHOT_INTERFACE).await;

        let handle = path(&unique_name("request"));
        let mut options = HashMap::new();
        options.insert("interactive".to_owned(), owned(true));
        let body = (handle.clone(), "app.id".to_owned(), String::new(), options);
        let screenshot_call =
            screenshot.call::<_, _, (u32, HashMap<String, OwnedValue>)>("Screenshot", &body);
        let cancel = async {
            Timer::after(Duration::from_millis(300)).await;
            request_proxy(&connection, &handle)
                .await
                .call::<_, _, ()>("Close", &())
                .await
                .unwrap();
        };

        let (result, ()) =
            with_timeout(futures_util::future::join(screenshot_call, cancel), 10).await;
        assert_eq!(result.unwrap().0, RESPONSE_CANCELLED);

        let state = niri.state.lock().unwrap();
        assert!(
            state
                .calls
                .iter()
                .any(|call| call == "InteractiveScreenshot")
        );
        assert!(state.calls.iter().any(|call| call == "CancelScreenshot"));
    });
}

#[test]
fn pick_color_cancellation_is_reported_as_cancelled() {
    async_io::block_on(async {
        let Some(bus) = TestBus::start() else {
            return;
        };
        let niri = FakeNiri::start(&bus).await;
        niri.state.lock().unwrap().pick_color_error = Some("no color picked".to_owned());
        let _portal = Portal::start(&bus, None).await;
        let connection = connect(&bus.address).await;
        let screenshot = portal_proxy(&connection, SCREENSHOT_INTERFACE).await;

        let (code, _results): (u32, HashMap<String, OwnedValue>) = screenshot
            .call(
                "PickColor",
                &(
                    path(&unique_name("request")),
                    "app.id".to_owned(),
                    String::new(),
                    HashMap::<String, OwnedValue>::new(),
                ),
            )
            .await
            .unwrap();
        assert_eq!(code, RESPONSE_CANCELLED);
    });
}

/// Helper mirroring the Screenshot interface call with an arbitrary options
/// dictionary.
async fn screenshot_call(
    screenshot: &Proxy<'static>,
    options: HashMap<String, OwnedValue>,
) -> zbus::Result<(u32, HashMap<String, OwnedValue>)> {
    screenshot
        .call(
            "Screenshot",
            &(
                path(&unique_name("request")),
                "app.id".to_owned(),
                String::new(),
                options,
            ),
        )
        .await
}

const SOURCE_TYPE_MONITOR: u32 = 1;
const SOURCE_TYPE_WINDOW: u32 = 2;
const SOURCE_TYPE_VIRTUAL: u32 = 4;
