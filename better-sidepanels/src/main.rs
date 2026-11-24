//! `better-sidepanels`: the Wayland layer-shell daemon.
//!
//! Creates one persistent panel per output on the configured side, with an
//! exclusive zone so tiled windows do not overlap it. Clicking a row runs its
//! command; module rows refresh on their interval.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result};
use better_sidepanels::{config_path, Panel, PanelConfig, Side};
use calloop::timer::{TimeoutAction, Timer};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_registry,
    registry_handlers,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

const BTN_LEFT: u32 = 0x110;

struct PanelSurface {
    output: wl_output::WlOutput,
    layer: LayerSurface,
    pool: SlotPool,
    size: (u32, u32),
    configured: bool,
    hovered: Option<usize>,
}

struct Sidepanels {
    panel: Panel,
    registry_state: RegistryState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    seat_state: SeatState,
    output_state: OutputState,
    surfaces: Vec<PanelSurface>,
    pointer: Option<wl_pointer::WlPointer>,
    exit: bool,
}

fn usage() -> &'static str {
    "better-sidepanels [--config PATH] [--check|--init]\n\n\
     Creates a persistent layer-shell side panel on every output. Without a\n\
     config file, a small default panel is used.\n\n\
     --check   Print the loaded configuration and exit.\n\
     --init    Write the default configuration to PATH and exit.\n\
     Config defaults to $BETTER_SIDEPANELS_CONFIG, then\n\
     $XDG_CONFIG_HOME/better-sidepanels/config.json."
}

fn main() -> Result<()> {
    let mut path = std::env::var_os("BETTER_SIDEPANELS_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(config_path);
    let mut check = false;
    let mut init = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => {
                path = PathBuf::from(args.next().context("--config needs a path")?);
            }
            "--check" => check = true,
            "--init" => init = true,
            "--help" | "-h" => {
                print!("{}", usage());
                return Ok(());
            }
            other => anyhow::bail!("unknown argument `{other}`\n\n{}", usage()),
        }
    }

    if init {
        let config = PanelConfig::default();
        config.save(&path).map_err(anyhow::Error::msg)?;
        println!("wrote {}", path.display());
        return Ok(());
    }
    let config = PanelConfig::load(&path).map_err(anyhow::Error::msg)?;
    if check {
        println!("{}", serde_json::to_string_pretty(&config)?);
        return Ok(());
    }
    run(config)
}

fn run(config: PanelConfig) -> Result<()> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<Sidepanels>(&conn)?;
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;
    let shm = Shm::bind(&globals, &qh)?;

    let mut state = Sidepanels {
        panel: Panel::new(config),
        registry_state: RegistryState::new(&globals),
        compositor,
        layer_shell,
        shm,
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        surfaces: Vec::new(),
        pointer: None,
        exit: false,
    };

    queue.roundtrip(&mut state)?;
    let outputs: Vec<_> = state.output_state.outputs().collect();
    for output in outputs {
        state.create_surface(&qh, output);
    }
    state.panel.refresh_all();
    queue.roundtrip(&mut state)?;
    state.redraw_all(&qh);

    let mut event_loop: EventLoop<Sidepanels> =
        EventLoop::try_new().map_err(|err| anyhow::anyhow!("calloop: {err}"))?;
    let handle = event_loop.handle();
    let signal = event_loop.get_signal();

    // Module refresh tick; a bounded timer wakes the loop while the panel idles.
    let tick = Duration::from_millis(250);
    let qh_tick = qh.clone();
    handle
        .insert_source(
            Timer::from_duration(tick),
            move |_, _, state: &mut Sidepanels| {
                if state.panel.refresh() {
                    state.redraw_all(&qh_tick);
                }
                TimeoutAction::ToDuration(tick)
            },
        )
        .map_err(|err| anyhow::anyhow!("calloop timer: {err}"))?;

    WaylandSource::new(conn, queue)
        .insert(handle.clone())
        .map_err(|err| anyhow::anyhow!("calloop wayland source: {err}"))?;

    event_loop
        .run(None, &mut state, move |state| {
            if state.exit {
                signal.stop();
            }
        })
        .map_err(|err| anyhow::anyhow!("calloop: {err}"))?;
    Ok(())
}

impl Sidepanels {
    fn create_surface(&mut self, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        if self.surfaces.iter().any(|surface| surface.output == output) {
            return;
        }
        let config = &self.panel.config;
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Top,
            Some("better-sidepanels"),
            Some(&output),
        );
        let anchor = match config.side {
            Side::Left => Anchor::LEFT,
            Side::Right => Anchor::RIGHT,
        };
        layer.set_anchor(anchor | Anchor::TOP | Anchor::BOTTOM);
        layer.set_exclusive_zone(config.exclusive_zone() as i32);
        layer.set_size(config.width, 0);
        let margin = config.margin as i32;
        layer.set_margin(
            0,
            if config.side == Side::Right {
                margin
            } else {
                0
            },
            0,
            if config.side == Side::Left { margin } else { 0 },
        );
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();

        let pool = match SlotPool::new((config.width as usize * 64).max(1024), &self.shm) {
            Ok(pool) => pool,
            Err(err) => {
                eprintln!("better-sidepanels: shm pool: {err}");
                return;
            }
        };
        self.surfaces.push(PanelSurface {
            output,
            layer,
            pool,
            size: (config.width, 1),
            configured: false,
            hovered: None,
        });
    }

    fn redraw_all(&mut self, qh: &QueueHandle<Self>) {
        for index in 0..self.surfaces.len() {
            self.draw(qh, index);
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>, index: usize) {
        let Some(surface) = self.surfaces.get_mut(index) else {
            return;
        };
        if !surface.configured {
            return;
        }
        let (width, height) = surface.size;
        if width == 0 || height == 0 {
            return;
        }
        let image = match self.panel.render_with(width, height, surface.hovered) {
            Ok(image) => image,
            Err(err) => {
                eprintln!("better-sidepanels: render: {err}");
                return;
            }
        };
        let stride = width as i32 * 4;
        let (buffer, canvas) = match surface.pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) {
            Ok(buffer) => buffer,
            Err(err) => {
                eprintln!("better-sidepanels: buffer: {err}");
                return;
            }
        };
        let len = canvas.len().min(image.data.len());
        canvas[..len].copy_from_slice(&image.data[..len]);

        let wl_surface = surface.layer.wl_surface();
        wl_surface.damage_buffer(0, 0, width as i32, height as i32);
        if buffer.attach_to(wl_surface).is_ok() {
            surface.layer.commit();
        }
        let _ = qh;
    }

    fn index_of(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.surfaces
            .iter()
            .position(|entry| entry.layer.wl_surface() == surface)
    }
}

impl CompositorHandler for Sidepanels {
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
        if let Some(index) = self.index_of(surface) {
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

impl OutputHandler for Sidepanels {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.create_surface(qh, output);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        self.surfaces.retain(|surface| surface.output != output);
    }
}

impl LayerShellHandler for Sidepanels {
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
        let Some(index) = self.index_of(layer.wl_surface()) else {
            return;
        };
        let width = NonZeroU32::new(configure.new_size.0)
            .map(NonZeroU32::get)
            .unwrap_or(self.panel.config.width);
        let height = NonZeroU32::new(configure.new_size.1)
            .map(NonZeroU32::get)
            .unwrap_or(1);
        self.surfaces[index].size = (width, height);
        self.surfaces[index].configured = true;
        self.draw(qh, index);
    }
}

impl SeatHandler for Sidepanels {
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
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => self.pointer = Some(pointer),
                Err(err) => eprintln!("better-sidepanels: pointer: {err}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Sidepanels {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(index) = self.index_of(&event.surface) else {
                continue;
            };
            match event.kind {
                PointerEventKind::Leave { .. } => {
                    if self.surfaces[index].hovered.take().is_some() {
                        self.draw(qh, index);
                    }
                }
                PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                    let hovered = self.panel.hit(event.position.0, event.position.1);
                    if self.surfaces[index].hovered != hovered {
                        self.surfaces[index].hovered = hovered;
                        self.draw(qh, index);
                    }
                }
                PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                    if let Some(row) = self.panel.hit(event.position.0, event.position.1) {
                        if let Err(err) = self.panel.activate(row) {
                            eprintln!("better-sidepanels: {err}");
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for Sidepanels {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Sidepanels {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}
delegate_registry!(Sidepanels);
smithay_client_toolkit::delegate_dispatch2!(Sidepanels);
