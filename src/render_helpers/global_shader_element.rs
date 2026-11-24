use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::{Element, Id, Kind, RenderElement};
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture, Uniform};
use smithay::backend::renderer::utils::CommitCounter;
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::{
    CustomUniform, CustomUniformValue, VulkanError, VulkanFrame, VulkanPixelProgram, VulkanTexture,
};
use smithay::backend::renderer::{Frame as _, FrameContext as _, Offscreen, Texture as _};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Logical, Physical, Rectangle, Scale, Transform};

use crate::backend::tty::{TtyFrame, TtyRenderer, TtyRendererError};
use crate::render_helpers::capture;
use crate::render_helpers::renderer::AsGlesFrame as _;
use crate::render_helpers::shader_element::{ShaderRenderElement, gles_texture_map};
#[cfg(feature = "vulkan")]
use crate::render_helpers::shaders;
use crate::render_helpers::shaders::{ProgramType, Shaders};

/// Per-pass feedback + offscreen handles cloned from `OutputState` for one frame.
#[derive(Debug, Clone)]
pub struct GlobalPassState {
    /// This pass's output last frame (its `niri_prev`).
    pub prev: Option<GlesTexture>,
    /// Sink for this pass's output this frame.
    pub result: Rc<RefCell<Option<GlesTexture>>>,
    /// Display offscreen (intermediate passes render here; unused for the last pass).
    pub pass_offscreen: Rc<crate::render_helpers::offscreen::OffscreenBuffer>,
    /// Dedicated `global_buffer` offscreen.
    pub buffer: Rc<crate::render_helpers::offscreen::OffscreenBuffer>,
    /// This pass's dedicated buffer last frame (its `niri_buffer` when it has a buffer program).
    pub buffer_prev: Option<GlesTexture>,
    /// Sink for this pass's dedicated buffer this frame.
    pub buffer_result: Rc<RefCell<Option<GlesTexture>>>,
}

/// A render element that captures the composited framebuffer below it and re-draws it through a
/// chain of global post-process programs (one or more passes).
///
/// Each pass reads the prior pass's output as `niri_screen` (pass 0 reads the real composited
/// screen) and the original unfiltered screen as `niri_source`. Intermediate passes render into an
/// offscreen texture that feeds the next pass; the last pass composites into the output. After
/// `draw()`, each pass's rendered texture is written into its shared `result` handle so the caller
/// (`OutputState`) can ping-pong it into that pass's `niri_prev` for the next frame.
#[derive(Debug, Clone)]
pub struct GlobalShaderElement {
    id: Id,
    commit: CommitCounter,
    area: Rectangle<f64, Logical>,
    scale: f32,
    time: f32,
    cursor: (f32, f32),
    /// Region this element covers in output-normalised coords: [origin.x, origin.y, w, h].
    /// `[0.0, 0.0, 1.0, 1.0]` = whole output.
    region_norm: [f32; 4],
    /// True full-output size in physical px (for the `niri_output_size` uniform).
    output_size_phys: (f32, f32),
    /// Previous frame's screen capture, bound as `niri_screen_prev` (frame-level, all passes).
    screen_prev: Option<GlesTexture>,
    /// Sink for this frame's screen capture, ping-ponged like a pass result.
    screen_result: Rc<RefCell<Option<GlesTexture>>>,
    /// One entry per pass, in execution order.
    passes: Vec<GlobalPassState>,
    /// Vulkan-only per-element textures (screen capture plus per-pass offscreens), used by
    /// [`Self::draw_vulkan`]. Elements are rebuilt every frame, so these start empty on each
    /// draw; they are kept on the element (rather than in `OutputState` like the GLES
    /// ping-pong handles) so the Vulkan draw path needs no extra shared state.
    #[cfg(feature = "vulkan")]
    vulkan: VulkanGlobalState,
}

/// Vulkan ping-pong/offscreen textures for one [`GlobalShaderElement`] draw.
#[cfg(feature = "vulkan")]
#[derive(Debug, Clone, Default)]
struct VulkanGlobalState {
    /// Previous frame's screen capture (`niri_screen_prev`); always empty for now since the
    /// element does not outlive a frame.
    screen_prev: RefCell<Option<VulkanTexture>>,
    /// This draw's screen capture, kept available as the next frame's `niri_screen_prev`
    /// once the element state is persisted across frames.
    screen_result: RefCell<Option<VulkanTexture>>,
    /// Per-pass display destinations (intermediate passes write here; the final pass renders
    /// to the framebuffer).
    display: RefCell<Vec<Option<VulkanTexture>>>,
    /// Per-pass dedicated `global_buffer` destinations, indexed by pass.
    buffers: RefCell<Vec<Option<VulkanTexture>>>,
}

impl GlobalShaderElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: Id,
        area: Rectangle<f64, Logical>,
        scale: f32,
        time: f32,
        cursor: (f32, f32),
        region_norm: [f32; 4],
        output_size_phys: (f32, f32),
        screen_prev: Option<GlesTexture>,
        screen_result: Rc<RefCell<Option<GlesTexture>>>,
        passes: Vec<GlobalPassState>,
    ) -> Self {
        Self {
            id,
            commit: CommitCounter::default(),
            area,
            scale,
            time,
            cursor,
            region_norm,
            output_size_phys,
            screen_prev,
            screen_result,
            passes,
            #[cfg(feature = "vulkan")]
            vulkan: VulkanGlobalState::default(),
        }
    }
}

impl Element for GlobalShaderElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        let size = self.area.size.to_buffer(1., Transform::Normal);
        Rectangle::from_size(size)
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.area.to_physical_precise_round(scale)
    }

    // We intentionally do not override `opaque_regions`: the default returns empty regions, which
    // is what we want since the shader may produce translucency and reads what's below.
}

impl RenderElement<GlesRenderer> for GlobalShaderElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let _span = tracy_client::span!("GlobalShaderElement::draw");

        let buffer_size = dst.size.to_logical(1).to_buffer(1, Transform::Normal);

        // Capture the composited screen below: niri_source for all passes, niri_screen for pass 0.
        let source_tex = {
            let mut guard = frame.renderer();
            guard
                .as_mut()
                .create_buffer(Fourcc::Abgr8888, buffer_size)?
            // guard dropped here so the GlesFrame binding is restored before the blit.
        };
        capture::capture_framebuffer_region(frame, dst, &source_tex)?;
        // Stash this frame's screen capture for next frame's niri_screen_prev.
        *self.screen_result.borrow_mut() = Some(source_tex.clone());

        let n = self.passes.len();
        // No chain, or any pass program missing => passthrough the captured screen unchanged.
        let chain_ready = n > 0
            && (0..n).all(|i| {
                Shaders::get_from_frame(frame)
                    .program(ProgramType::GlobalPass(i))
                    .is_some()
            });
        if !chain_ready {
            return frame.render_texture_from_to(
                &source_tex,
                Rectangle::from_size(source_tex.size().to_f64()),
                dst,
                damage,
                &[],
                frame.transformation().invert(),
                1.,
                None,
                &[],
            );
        }

        // Empty-feedback seed: on the first frame a pass's niri_prev / niri_buffer has no prior
        // value; seed with black so accumulators start empty instead of ingesting the live screen.
        let black = Shaders::get_from_frame(frame).black_texture.clone();

        // Previous frame's screen; falls back to this frame's screen on the first frame.
        let screen_prev_tex = self
            .screen_prev
            .clone()
            .unwrap_or_else(|| source_tex.clone());

        let uniforms: Rc<[Uniform<'static>]> = Rc::new([
            Uniform::new("niri_time", self.time),
            Uniform::new("niri_cursor", (self.cursor.0, self.cursor.1)),
            Uniform::new(
                "niri_region",
                (
                    self.region_norm[0],
                    self.region_norm[1],
                    self.region_norm[2],
                    self.region_norm[3],
                ),
            ),
            Uniform::new(
                "niri_output_size",
                (self.output_size_phys.0, self.output_size_phys.1),
            ),
        ]);

        // The input to the current pass: pass 0 reads the real screen, later passes read the prior
        // pass's output.
        let mut input = source_tex.clone();

        for (i, pass) in self.passes.iter().enumerate() {
            // This pass's own previous-frame output (its niri_prev); on the first frame fall back
            // to black so accumulators start empty instead of ingesting the live screen.
            let prev_tex = pass.prev.clone().unwrap_or_else(|| black.clone());

            // --- Dedicated buffer sub-pass for this pass (if it defines global_buffer) ---
            // Render the GlobalPassBuffer(i) program into a clean offscreen (reading last frame's
            // buffer) and feed THAT as niri_buffer. Otherwise niri_buffer aliases niri_prev.
            let buffer_tex = if Shaders::get_from_frame(frame)
                .program(ProgramType::GlobalPassBuffer(i))
                .is_some()
            {
                // First frame: seed niri_buffer with black (empty accumulator), not the screen.
                let buf_prev = pass.buffer_prev.clone().unwrap_or_else(|| black.clone());

                let mut buf_textures = HashMap::new();
                buf_textures.insert("niri_screen".to_string(), input.clone());
                buf_textures.insert("niri_source".to_string(), source_tex.clone());
                buf_textures.insert("niri_prev".to_string(), prev_tex.clone());
                buf_textures.insert("niri_screen_prev".to_string(), screen_prev_tex.clone());
                buf_textures.insert("niri_buffer".to_string(), buf_prev);

                let buf_element = ShaderRenderElement::new(
                    ProgramType::GlobalPassBuffer(i),
                    self.area.size,
                    None,
                    self.scale,
                    1.,
                    uniforms.clone(),
                    gles_texture_map(buf_textures),
                    Kind::Unspecified,
                );

                let mut guard = frame.renderer();
                let renderer = guard.as_mut();
                match pass
                    .buffer
                    .render(renderer, Scale::from(self.scale as f64), &[buf_element])
                {
                    Ok((off_elem, _sync, _data)) => {
                        let next = match off_elem.texture_gles() {
                            Some(texture) => texture.clone(),
                            None => {
                                warn!("global_buffer pass {i} produced a non-GLES texture");
                                prev_tex.clone()
                            }
                        };
                        drop(guard);
                        // DO NOT remove this retained clone. It forces OffscreenBuffer to allocate
                        // a distinct write texture next frame (its
                        // is_unique_reference check); without an external
                        // clone alive, next frame's buffer pass would read and write the
                        // same texture and corrupt the feedback. The clone also lets the owner move
                        // this texture into the pass's buffer_prev after submit.
                        *pass.buffer_result.borrow_mut() = Some(next.clone());
                        next
                    }
                    Err(err) => {
                        drop(guard);
                        warn!("global_buffer pass {i} failed: {err:?}");
                        prev_tex.clone()
                    }
                }
            } else {
                prev_tex.clone()
            };

            // --- This pass's display program ---
            let mut textures = HashMap::new();
            textures.insert("niri_screen".to_string(), input.clone());
            textures.insert("niri_source".to_string(), source_tex.clone());
            textures.insert("niri_prev".to_string(), prev_tex.clone());
            textures.insert("niri_screen_prev".to_string(), screen_prev_tex.clone());
            textures.insert("niri_buffer".to_string(), buffer_tex);

            let element = ShaderRenderElement::new(
                ProgramType::GlobalPass(i),
                self.area.size,
                None,
                self.scale,
                1.,
                uniforms.clone(),
                gles_texture_map(textures),
                Kind::Unspecified,
            );

            if i + 1 < n {
                // Intermediate pass: render into this pass's display offscreen. Its texture is the
                // next pass's input AND this pass's next-frame niri_prev (same retained-clone
                // ping-pong as the buffer sub-pass).
                let mut guard = frame.renderer();
                let renderer = guard.as_mut();
                match pass.pass_offscreen.render(
                    renderer,
                    Scale::from(self.scale as f64),
                    &[element],
                ) {
                    Ok((off_elem, _sync, _data)) => {
                        let next = match off_elem.texture_gles() {
                            Some(texture) => texture.clone(),
                            None => {
                                warn!("global pass {i} produced a non-GLES texture");
                                input.clone()
                            }
                        };
                        drop(guard);
                        *pass.result.borrow_mut() = Some(next.clone());
                        input = next;
                    }
                    Err(err) => {
                        drop(guard);
                        warn!("global pass {i} failed: {err:?}");
                        // Best effort: feed the input forward unchanged.
                    }
                }
            } else {
                // Last pass: composite into the output framebuffer, then capture for niri_prev.
                // ShaderRenderElement::src() is the unit rectangle, so pass a unit src for a 1:1
                // full-screen mapping of the captured texture onto dst.
                RenderElement::<GlesRenderer>::draw(
                    &element,
                    frame,
                    Rectangle::from_size((1., 1.).into()),
                    dst,
                    damage,
                    &[],
                    None,
                )?;
                let result_tex = {
                    let mut guard = frame.renderer();
                    guard
                        .as_mut()
                        .create_buffer(Fourcc::Abgr8888, buffer_size)?
                };
                capture::capture_framebuffer_region(frame, dst, &result_tex)?;
                *pass.result.borrow_mut() = Some(result_tex);
            }
        }

        Ok(())
    }
}

#[cfg(feature = "vulkan")]
impl GlobalShaderElement {
    /// Vulkan equivalent of the GLES `draw` above: capture the region below, then run the
    /// configured global pass chain with the same sampler names and uniforms.
    fn draw_vulkan(
        &self,
        frame: &mut VulkanFrame<'_, '_>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
    ) -> Result<(), VulkanError> {
        let _span = tracy_client::span!("GlobalShaderElement::draw_vulkan");

        let buffer_size = dst.size.to_logical(1).to_buffer(1, Transform::Normal);

        // Resolve the compiled programs once. `get_vulkan_frame` borrows the frame, so this must
        // finish before any mutable frame calls.
        let Some(programs) = ({
            shaders::get_vulkan_frame(frame).and_then(|shaders| {
                let n = self.passes.len();
                if n == 0 {
                    return None;
                }
                let programs: Option<Vec<VulkanPixelProgram>> = (0..n)
                    .map(|i| shaders.program(ProgramType::GlobalPass(i)))
                    .collect();
                programs.map(|programs| {
                    let buffers = (0..n)
                        .map(|i| shaders.program(ProgramType::GlobalPassBuffer(i)))
                        .collect::<Vec<_>>();
                    (programs, buffers)
                })
            })
        }) else {
            // No usable Vulkan chain: capture and blit the region back unchanged.
            let source_tex = capture::create_vulkan_texture(frame, buffer_size)?;
            capture::capture_framebuffer_region_vulkan(frame, dst, &source_tex)?;
            *self.vulkan.screen_result.borrow_mut() = Some(source_tex.clone());
            return frame.render_texture_from_to(
                &source_tex,
                Rectangle::from_size(source_tex.size().to_f64()),
                dst,
                damage,
                &[],
                Transform::Normal,
                1.,
            );
        };
        let (programs, buffer_programs) = programs;

        // Capture the composited screen below: niri_source for all passes, niri_screen for pass 0.
        let source_tex = capture::create_vulkan_texture(frame, buffer_size)?;
        capture::capture_framebuffer_region_vulkan(frame, dst, &source_tex)?;
        *self.vulkan.screen_result.borrow_mut() = Some(source_tex.clone());

        // First-frame feedback seeds: niri_screen_prev falls back to this frame's capture,
        // while niri_prev / niri_buffer start from transparent black.
        let black = shaders::get_vulkan_frame(frame)
            .and_then(|shaders| shaders.black_texture.clone())
            .unwrap_or_else(|| source_tex.clone());
        let screen_prev_tex = self
            .vulkan
            .screen_prev
            .borrow()
            .clone()
            .unwrap_or_else(|| source_tex.clone());

        let uniforms = self.vulkan_uniforms(dst.size);
        let n = programs.len();
        {
            let mut display = self.vulkan.display.borrow_mut();
            display.resize(n, None);
            let mut buffers = self.vulkan.buffers.borrow_mut();
            buffers.resize(n, None);
        }

        // The input to the current pass: pass 0 reads the real screen, later passes read the prior
        // pass's output.
        let mut input = source_tex.clone();

        for (i, program) in programs.iter().enumerate() {
            let prev_tex = black.clone();

            // --- Dedicated buffer sub-pass for this pass (if it defines global_buffer) ---
            let buffer_tex = if let Some(buffer_program) = &buffer_programs[i] {
                let textures = [
                    ("niri_screen", &input),
                    ("niri_source", &source_tex),
                    ("niri_prev", &prev_tex),
                    ("niri_screen_prev", &screen_prev_tex),
                    ("niri_buffer", &black),
                ];
                match capture::render_custom_pass_to_texture_vulkan(
                    frame,
                    buffer_size,
                    buffer_program,
                    &uniforms,
                    &textures,
                ) {
                    Ok(texture) => {
                        if let Some(slot) = self.vulkan.buffers.borrow_mut().get_mut(i) {
                            *slot = Some(texture.clone());
                        }
                        texture
                    }
                    Err(err) => {
                        warn!("global_buffer pass {i} failed on Vulkan: {err:?}");
                        prev_tex.clone()
                    }
                }
            } else {
                prev_tex.clone()
            };

            // --- This pass's display program ---
            let textures = [
                ("niri_screen", &input),
                ("niri_source", &source_tex),
                ("niri_prev", &prev_tex),
                ("niri_screen_prev", &screen_prev_tex),
                ("niri_buffer", &buffer_tex),
            ];

            if i + 1 < n {
                // Intermediate pass: render into an offscreen that feeds the next pass.
                match capture::render_custom_pass_to_texture_vulkan(
                    frame,
                    buffer_size,
                    program,
                    &uniforms,
                    &textures,
                ) {
                    Ok(next) => {
                        if let Some(slot) = self.vulkan.display.borrow_mut().get_mut(i) {
                            *slot = Some(next.clone());
                        }
                        input = next;
                    }
                    Err(err) => {
                        warn!("global pass {i} failed on Vulkan: {err:?}");
                        // Best effort: feed the input forward unchanged.
                    }
                }
            } else {
                // Last pass: composite into the output framebuffer, then capture the result for a
                // future frame's niri_prev (same as the GLES path).
                frame.render_custom(program, dst, damage, &uniforms, &textures, 1.0)?;
                let result_tex = capture::create_vulkan_texture(frame, buffer_size)?;
                capture::capture_framebuffer_region_vulkan(frame, dst, &result_tex)?;
                if let Some(slot) = self.vulkan.display.borrow_mut().get_mut(i) {
                    *slot = Some(result_tex);
                }
            }
        }

        Ok(())
    }

    /// Builds the uniform set expected by the compiled global pass programs.
    fn vulkan_uniforms(
        &self,
        dst_size: smithay::utils::Size<i32, Physical>,
    ) -> Vec<CustomUniform<'static>> {
        let mut uniforms = vec![
            CustomUniform {
                name: "niri_time",
                value: CustomUniformValue::Float(self.time),
            },
            CustomUniform {
                name: "niri_cursor",
                value: CustomUniformValue::Vec2([self.cursor.0, self.cursor.1]),
            },
            CustomUniform {
                name: "niri_region",
                value: CustomUniformValue::Vec4(self.region_norm),
            },
            CustomUniform {
                name: "niri_output_size",
                value: CustomUniformValue::Vec2([self.output_size_phys.0, self.output_size_phys.1]),
            },
            CustomUniform {
                name: "niri_size",
                value: CustomUniformValue::Vec2([dst_size.w as f32, dst_size.h as f32]),
            },
            CustomUniform {
                name: "niri_scale",
                value: CustomUniformValue::Float(self.scale),
            },
        ];
        // SDR blend-space defaults; Vulkan HDR output is not wired yet.
        for (name, value) in [
            ("niri_hdr_pq", 0.0),
            ("niri_ref_lum_scale", 1.0),
            ("niri_linear", 0.0),
            ("niri_linear_scale", 0.0),
            ("niri_linear_to_ref", 0.0),
            ("niri_hdr_to_sdr", 0.0),
        ] {
            uniforms.push(CustomUniform {
                name,
                value: CustomUniformValue::Float(value),
            });
        }
        uniforms
    }
}

impl<'render> RenderElement<TtyRenderer<'render>> for GlobalShaderElement {
    fn draw(
        &self,
        frame: &mut TtyFrame<'_, '_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), TtyRendererError<'render>> {
        #[cfg(feature = "vulkan")]
        if let TtyFrame::Vulkan(multi) = frame {
            let vk_frame: &mut VulkanFrame<'_, '_> = multi.as_mut();
            return self.draw_vulkan(vk_frame, dst, damage).map_err(|err| {
                TtyRendererError::Vulkan(smithay::backend::renderer::multigpu::Error::Render(err))
            });
        }

        let Some(gles_frame) = frame.as_gles_frame() else {
            return Ok(());
        };
        RenderElement::<GlesRenderer>::draw(
            self,
            gles_frame,
            src,
            dst,
            damage,
            opaque_regions,
            cache,
        )?;
        Ok(())
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame as _, Offscreen, Renderer};
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Rectangle, Size, Transform};

    use super::*;
    use crate::render_helpers::offscreen::OffscreenBuffer;
    use crate::render_helpers::shaders;

    fn test_element(passes: usize) -> GlobalShaderElement {
        GlobalShaderElement::new(
            Id::new(),
            Rectangle::from_size(Size::<f64, Logical>::from((8.0, 8.0))),
            1.0,
            0.0,
            (0.0, 0.0),
            [0.0, 0.0, 1.0, 1.0],
            (8.0, 8.0),
            None,
            Rc::new(RefCell::new(None)),
            (0..passes)
                .map(|_| GlobalPassState {
                    prev: None,
                    result: Rc::new(RefCell::new(None)),
                    pass_offscreen: Rc::new(OffscreenBuffer::default()),
                    buffer: Rc::new(OffscreenBuffer::default()),
                    buffer_prev: None,
                    buffer_result: Rc::new(RefCell::new(None)),
                })
                .collect(),
        )
    }

    /// Runs a real two-pass global shader chain through the element's Vulkan draw path.
    ///
    /// Pass 0 reads the captured green screen and writes green into its offscreen; pass 1
    /// samples that offscreen (proving the intermediate ping-pong) and writes red to the
    /// output. Skips silently when no Vulkan device is usable.
    #[test]
    fn global_element_vulkan_two_pass_chain_reads_back() {
        let Ok(instance) = Instance::new(Version::VERSION_1_3, None) else {
            return;
        };
        let Ok(devices) = PhysicalDevice::enumerate(&instance) else {
            return;
        };
        for device in devices {
            let Ok(mut renderer) = VulkanRenderer::new(&device) else {
                continue;
            };
            let config = niri_config::Config::parse_mem(
                r##"
                global-shader {
                    enable
                    pass {
                        source "vec4 global_color(vec3 c) { vec4 s = tex2D_screen(c.xy); return vec4(0.0, s.g, 0.0, 1.0); }"
                    }
                    pass {
                        source "vec4 global_color(vec3 c) { vec4 s = tex2D_screen(c.xy); return vec4(s.g, 0.0, 0.0, 1.0); }"
                    }
                }
                "##,
            )
            .expect("parse test config");
            shaders::init_vulkan(&mut renderer, &config);
            assert!(
                shaders::get_vulkan_renderer(&renderer)
                    .and_then(|s| s.program(ProgramType::GlobalPass(1)))
                    .is_some(),
                "global pass 1 failed to compile on {}",
                device.name()
            );

            let size = Size::<i32, Buffer>::from((8, 8));
            let output_size = Size::<i32, Physical>::from((8, 8));
            let full = Rectangle::from_size(output_size);

            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, size)
                .expect("target");
            let mut fb = renderer.bind(&mut target).expect("bind");
            let mut frame = renderer
                .render(&mut fb, output_size, Transform::Normal)
                .expect("frame");
            frame
                .clear(Color32F::new(0.0, 1.0, 0.0, 1.0), &[full])
                .expect("clear");

            let element = test_element(2);
            element
                .draw_vulkan(&mut frame, full, &[full])
                .expect("vulkan global chain");
            let _sync = frame.finish().expect("finish");

            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size(size), Fourcc::Argb8888)
                .expect("copy");
            let pixels = renderer.map_texture(&mapping).expect("map");
            assert_eq!(&pixels[0..4], &[0, 0, 255, 255], "expected red");
            eprintln!(
                "Vulkan two-pass global element chain OK on {}",
                device.name()
            );
            return;
        }
    }
}
