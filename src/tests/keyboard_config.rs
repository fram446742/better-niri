use niri_config::{Config, Xkb};

use super::Fixture;

fn config(text: &str) -> Config {
    Config::parse_mem(text).unwrap()
}

fn active_layout_name(state: &mut crate::niri::State) -> String {
    let keyboard = state.niri.seat.get_keyboard().unwrap();
    keyboard.with_xkb_state(state, |context| {
        let xkb = context.xkb().lock().unwrap();
        xkb.layout_name(xkb.active_layout()).to_owned()
    })
}

fn num_lock(state: &mut crate::niri::State) -> bool {
    let keyboard = state.niri.seat.get_keyboard().unwrap();
    keyboard.modifier_state().num_lock
}

const TWO_KEYBOARDS: &str = r#"
    input {
        keyboard {
            xkb {
                layout "gb"
            }
            numlock
        }
        keyboard "Ext" {
            xkb {
                layout "us"
            }
        }
    }
"#;

#[test]
fn switching_keyboard_config_applies_the_named_layout() {
    let config = config(TWO_KEYBOARDS);
    let ext = config.input.keyboard_named("Ext");
    let mut f = Fixture::with_config(config);
    let state = f.niri_state();
    assert_eq!(active_layout_name(state), "English (UK)");

    state.switch_keyboard_config(ext.clone());

    assert_eq!(active_layout_name(state), "English (US)");
    assert_eq!(state.niri.current_keyboard, ext);
}

#[test]
fn switching_keyboard_config_keeps_num_lock() {
    let config = config(TWO_KEYBOARDS);
    let ext = config.input.keyboard_named("Ext");
    let mut f = Fixture::with_config(config);
    let state = f.niri_state();
    assert!(
        num_lock(state),
        "numlock from the config must be on at startup"
    );

    state.switch_keyboard_config(ext);

    assert!(num_lock(state), "a keymap switch must not drop num lock");
}

#[test]
fn switching_back_to_an_unset_xkb_uses_locale1_settings() {
    let config = config(
        r#"
        input {
            keyboard "Ext" {
                xkb {
                    layout "us"
                }
            }
        }
        "#,
    );
    let ext = config.input.keyboard_named("Ext");
    let fallback = config.input.fallback_keyboard();
    let mut f = Fixture::with_config(config);
    let state = f.niri_state();
    state.niri.xkb_from_locale1 = Some(Xkb {
        layout: "de".to_owned(),
        ..Default::default()
    });

    state.switch_keyboard_config(ext);
    assert_eq!(active_layout_name(state), "English (US)");

    // The unnamed keyboard leaves xkb unset, so like a config reload it must
    // pick up the locale1 settings rather than the bare xkb defaults.
    state.switch_keyboard_config(fallback);
    assert_eq!(active_layout_name(state), "German");
}

#[test]
fn switching_keyboard_config_applies_repeat_settings() {
    let config = config(
        r#"
        input {
            keyboard {
                repeat-delay 600
                repeat-rate 25
            }
            keyboard "Ext" {
                repeat-delay 200
                repeat-rate 60
            }
        }
        "#,
    );
    let ext = config.input.keyboard_named("Ext");
    let mut f = Fixture::with_config(config);
    let state = f.niri_state();
    assert_eq!(state.niri.current_keyboard.repeat_rate, 25);
    assert_eq!(state.niri.current_keyboard.repeat_delay, 600);

    state.switch_keyboard_config(ext.clone());

    assert_eq!(state.niri.current_keyboard.repeat_rate, 60);
    assert_eq!(state.niri.current_keyboard.repeat_delay, 200);
    assert_eq!(state.niri.current_keyboard, ext);
}

#[test]
fn switching_the_same_config_twice_keeps_the_layout() {
    let config = config(TWO_KEYBOARDS);
    let ext = config.input.keyboard_named("Ext");
    let mut f = Fixture::with_config(config);
    let state = f.niri_state();

    state.switch_keyboard_config(ext.clone());
    assert_eq!(active_layout_name(state), "English (US)");

    // A second switch with an identical config is a no-op (it must not reset
    // the keymap or the modifiers).
    state.switch_keyboard_config(ext.clone());
    assert_eq!(active_layout_name(state), "English (US)");
    assert_eq!(state.niri.current_keyboard, ext);
}

#[test]
fn reverting_to_the_fallback_restores_the_default_layout() {
    let config = config(TWO_KEYBOARDS);
    let ext = config.input.keyboard_named("Ext");
    let fallback = config.input.fallback_keyboard();
    let mut f = Fixture::with_config(config);
    let state = f.niri_state();
    state.switch_keyboard_config(ext);
    assert_eq!(active_layout_name(state), "English (US)");

    // Simulates the active keyboard being unplugged.
    state.revert_to_fallback_keyboard();

    assert_eq!(active_layout_name(state), "English (UK)");
    assert_eq!(state.niri.current_keyboard, fallback);
    assert!(state.niri.current_keyboard_device.is_none());
}
