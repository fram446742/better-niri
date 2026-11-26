# Reference test coverage

This file records which tests from the source repositories were ported or
adapted, so a green suite points at behaviour that the references also exercise
instead of only our own assumptions. Entries that could not be ported say why.

| Feature | Reference test | Ours | Status |
| --- | --- | --- | --- |
| xdg-toplevel Suspended while locked/off | niri-spicy `83783afb` `src/tests/suspended.rs` (2 tests) | `src/tests/suspended.rs` | Ported verbatim (API adapted) |
| HDR peak-luminance metadata | niri-spicy `34722b2a` assertions in `backend/tty.rs` | `backend/tty.rs::hdr_metadata_luminance_priorities` | Assertions ported |
| Per-output tearing parse | niri-spicy `2a052fa9` `parse_tearing_mode` | `niri-config/src/output.rs::parse_tearing_mode` | Ported |
| HDR scanout colour pipeline | niri-spicy `96c7e4a7^` blend/colorimetry tests | `render_helpers/colorimetry.rs`, `blend.rs`, Vulkan renderer tests | Ported/adapted by the HDR port |
| NVIDIA frame-fence wait | niri-spicy `1bfaa833` | none | Reference has no tests for this path; hardware-only |
| Overview focus-ring/shadow overrides | demoji `36f6868f` config test | `niri-config/src/lib.rs::reference_test_ports` | Ported (config level) |
| Focused-workspace-only decorations | demoji `41022fe2` | covered indirectly by layout render tests | No dedicated reference test |
| Sticky focus switching/centering | biri `699e508b` (2 tests) | `layout/tests.rs::sticky_*` | Ported; the centering test is adapted to our `TestWindow` |
| Carousel pointer warp | biri `87b37916` `src/tests/carousel.rs` | none | Not ported: depends on biri-only fixture helpers (`move_cursor`, visual rectangle). The handler is exercised by session tests, not by a reference port. |
| Spawn floating at cursor | Naxdy `be7586dc` | none | The reference only updated call sites; it has no behavioural test |
| Blur brightness/contrast | Naxdy `5059f785` | `niri-config` config test | Config covered; reference had no render test |
| Window groups/tabbed tiles | Naxdy `src/layout/tests/animations.rs` group tests | `layout/tests/animations.rs` (`appear_group_indicator`, `move_window_into_and_out_of_group_*`) plus `layout/tests.rs` model/geometry tests | Animation ops ported; exact pixel snapshots adapted because our tabs reserve a title band and normalise position |
| Layer fade in/out | Naxdy `4df357ee`, `230e5705` | `layer::tests` | Reference has no fade tests (only the expected `Animations` config literal, ported); ours cover alpha progression, cleanup and blocked-out outputs |
| KDE Screenshot2 | Naxdy `99f238db` | `dbus::kwin_screenshot2` | Reference has no tests; ours cover metadata, pipe writer, PNG decode, introspection and a private-bus fd round trip |
| Floating sidepanel stacks | zgavin/niri-sidepanels test suite | `loom-sidepanels/stack` (104 tests) | Imported with the port |
| Companion rules/run-or-raise/scratchpads | Piri/nsticky test suites | `companion-rules`, `companion-niri`, `nsticky` tests | Behavioural ports, not line-by-line copies |
| Session restore/snapshots | nirinit/niri-session-manager tests | `loom-session` tests | Behavioural ports |
| Monitor profiles/rollback | niri_display_manager/nirilayout tests | `loom-monitor` tests | Behavioural ports |
| KDL editing | NiriMod tests | `loom-settings` tests | Behavioural ports |
| Audio routing | PWSW tests | `loom-audio` tests | Behavioural ports |
| Screenshots/annotations/pins | Nirisnap/niri-shot tests | `loom-shot` tests | Behavioural ports plus our pin tests |
| Noctalia widgets | Noctalia plugin examples | `loom-noctalia-plugins/verify.sh` | Syntax/manifest checks; live shell check manual |

## Rules

- When a feature comes from a reference with tests, port the applicable tests
  before claiming the feature is integrated.
- When a reference has no test for a behaviour (hardware paths, cursor warps,
  pure call-site changes), state that here instead of implying coverage.
- Behavioural ports (different language or architecture) list the test suite
  they were modelled on; they do not pretend to be verbatim copies.
