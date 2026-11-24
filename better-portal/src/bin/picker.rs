//! `better-portal-picker`: a layer-shell rectangle picker for screenshots.
//!
//! Draws a full-output overlay on every output, lets the user drag a rectangle,
//! then prints it as `{"x","y","width","height"}` in global logical coordinates.
//! Escape/cancel exits with status 2.

use std::num::NonZeroU32;

use better_portal::{PickerMode as Mode, SelectionRect};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_registry, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

struct PickerSurface {
    output: wl_output::WlOutput,
    output_name: Option<String>,
    layer: LayerSurface,
    pool: SlotPool,
    size: (u32, u32),
    output_pos: (i32, i32),
    start: Option<(f64, f64)>,
    current: Option<(f64, f64)>,
    configured: bool,
}

struct Picker {
    mode: Mode,
    registry_state: RegistryState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    seat_state: SeatState,
    output_state: OutputState,
    surfaces: Vec<PickerSurface>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    exit: bool,
    result: Option<SelectionRect>,
    result_output: Option<String>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("better-portal-picker: {err}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mode = Mode::from_args(std::env::args().skip(1));
    if mode == Mode::Window {
        let path = std::env::var_os("NIRI_SOCKET")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("NIRI_SOCKET is not set"))?;
        match better_portal::pick_window_via_niri(&path).map_err(anyhow::Error::msg)? {
            Some(id) => {
                println!("{}", serde_json::json!({ "window": id }));
                return Ok(());
            }
            None => std::process::exit(2),
        }
    }
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Picker>(&conn)?;
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;

    let mut picker = Picker {
        mode,
        registry_state: RegistryState::new(&globals),
        compositor,
        layer_shell,
        shm,
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        surfaces: Vec::new(),
        keyboard: None,
        pointer: None,
        exit: false,
        result: None,
        result_output: None,
    };

    queue.roundtrip(&mut picker)?;
    let outputs: Vec<_> = picker.output_state.outputs().collect();
    for output in outputs {
        picker.create_surface(&qh, output);
    }
    queue.roundtrip(&mut picker)?;

    while !picker.exit {
        queue.blocking_dispatch(&mut picker)?;
    }

    match (picker.result, picker.result_output) {
        (_, Some(output)) => {
            println!("{}", serde_json::json!({ "output": output }));
            Ok(())
        }
        (Some(rect), None) => {
            println!("{}", serde_json::to_string(&rect)?);
            Ok(())
        }
        (None, None) => std::process::exit(2),
    }
}

impl Picker {
    fn create_surface(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if self.surfaces.iter().any(|s| s.output == output) {
            return;
        }
        let info = self.output_state.info(&output);
        let output_pos = info
            .as_ref()
            .and_then(|info| info.logical_position)
            .unwrap_or((0, 0));
        let output_name = info.and_then(|info| info.name);
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("better-portal-picker"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();

        let pool = SlotPool::new(1024, &self.shm).expect("create shm pool");
        self.surfaces.push(PickerSurface {
            output,
            output_name,
            layer,
            pool,
            size: (1, 1),
            output_pos,
            start: None,
            current: None,
            configured: false,
        });
    }

    fn update_output_positions(&mut self) {
        for surface in &mut self.surfaces {
            if let Some(pos) = self
                .output_state
                .info(&surface.output)
                .and_then(|info| info.logical_position)
            {
                surface.output_pos = pos;
            }
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>, index: usize) {
        let PickerSurface {
            layer,
            pool,
            size: (width, height),
            start,
            current,
            ..
        } = &mut self.surfaces[index];
        let (width, height) = (*width, *height);
        let (start, current) = (*start, *current);
        if width == 0 || height == 0 {
            return;
        }
        let stride = width as i32 * 4;
        let (buffer, canvas) = match pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                eprintln!("better-portal-picker: buffer error: {err}");
                return;
            }
        };

        for pixel in canvas.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[0, 0, 0, 0x66]);
        }

        if let (Some(start), Some(current)) = (start, current) {
            let x0 = start.0.min(current.0).max(0.) as u32;
            let y0 = start.1.min(current.1).max(0.) as u32;
            let x1 = (start.0.max(current.0).max(0.) as u32).min(width);
            let y1 = (start.1.max(current.1).max(0.) as u32).min(height);
            fill_rect(
                canvas,
                width,
                (x0, y0, x1, y1),
                [0x22, 0x22, 0x22, 0x44],
                false,
            );
            fill_rect(
                canvas,
                width,
                (x0, y0, x1, y1),
                [0xff, 0xff, 0xff, 0xff],
                true,
            );
        }
        if let Some((px, py)) = current {
            let x = px.max(0.) as u32;
            let y = py.max(0.) as u32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1)] {
                let cx = x.saturating_add(dx).min(width - 1);
                let cy = y.saturating_add(dy).min(height - 1);
                put_pixel(canvas, width, cx, cy, [0x00, 0xcc, 0xff, 0xff]);
            }
        }

        let wl_surface = layer.wl_surface();
        wl_surface.damage_buffer(0, 0, width as i32, height as i32);
        if buffer.attach_to(wl_surface).is_ok() {
            layer.commit();
        }
        let _ = qh;
    }

    fn finish(&mut self, surface_index: usize) {
        let surface = &self.surfaces[surface_index];
        let (Some(start), Some(current)) = (surface.start, surface.current) else {
            return;
        };
        if let Some(rect) = SelectionRect::from_drag(surface.output_pos, start, current) {
            self.result = Some(rect);
            self.exit = true;
        }
    }
}

fn put_pixel(canvas: &mut [u8], width: u32, x: u32, y: u32, color: [u8; 4]) {
    if x >= width {
        return;
    }
    let index = (y * width + x) as usize * 4;
    if index + 4 <= canvas.len() {
        canvas[index..index + 4].copy_from_slice(&color);
    }
}

fn fill_rect(
    canvas: &mut [u8],
    width: u32,
    (x0, y0, x1, y1): (u32, u32, u32, u32),
    color: [u8; 4],
    border_only: bool,
) {
    for y in y0..y1 {
        for x in x0..x1 {
            if border_only && x != x0 && x + 1 != x1 && y != y0 && y + 1 != y1 {
                continue;
            }
            put_pixel(canvas, width, x, y, color);
        }
    }
}

impl CompositorHandler for Picker {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(index) = self
            .surfaces
            .iter()
            .position(|s| s.layer.wl_surface() == surface)
        {
            self.draw(qh, index);
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Picker {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.create_surface(qh, output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.update_output_positions();
    }

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.surfaces.retain(|s| s.output != output);
    }
}

impl LayerShellHandler for Picker {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(index) = self
            .surfaces
            .iter()
            .position(|s| s.layer.wl_surface() == layer.wl_surface())
        else {
            return;
        };
        let width = NonZeroU32::new(configure.new_size.0)
            .map(NonZeroU32::get)
            .unwrap_or(1);
        let height = NonZeroU32::new(configure.new_size.1)
            .map(NonZeroU32::get)
            .unwrap_or(1);
        self.surfaces[index].size = (width, height);
        self.surfaces[index].configured = true;
        self.draw(qh, index);
    }
}

impl SeatHandler for Picker {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && self.keyboard.is_none()
            && let Ok(keyboard) = self.seat_state.get_keyboard(qh, &seat, None)
        {
            self.keyboard = Some(keyboard);
        }
        if capability == Capability::Pointer
            && self.pointer.is_none()
            && let Ok(pointer) = self.seat_state.get_pointer(qh, &seat)
        {
            self.pointer = Some(pointer);
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
        if capability == Capability::Pointer
            && let Some(pointer) = self.pointer.take()
        {
            pointer.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Picker {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if event.keysym == Keysym::Escape {
            self.exit = true;
        }
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl PointerHandler for Picker {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(index) = self
                .surfaces
                .iter()
                .position(|s| s.layer.wl_surface() == &event.surface)
            else {
                continue;
            };
            match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    self.surfaces[index].current = Some(event.position);
                    if self.surfaces[index].start.is_some() {
                        self.draw(qh, index);
                    }
                }
                PointerEventKind::Press { button: 0x110, .. } => {
                    if self.mode == Mode::Output {
                        self.result_output = self.surfaces[index].output_name.clone();
                        self.exit = true;
                        continue;
                    }
                    self.surfaces[index].start = Some(event.position);
                    self.surfaces[index].current = Some(event.position);
                    self.draw(qh, index);
                }
                PointerEventKind::Release { button: 0x110, .. } => {
                    self.surfaces[index].current = Some(event.position);
                    self.draw(qh, index);
                    self.finish(index);
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for Picker {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
delegate_registry!(Picker);
smithay_client_toolkit::delegate_dispatch2!(Picker);

impl ProvidesRegistryState for Picker {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
