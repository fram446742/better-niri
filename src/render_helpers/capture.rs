use smithay::backend::renderer::Texture as _;
use smithay::backend::renderer::gles::{GlesError, GlesFrame, GlesTexture, ffi};
use smithay::utils::{Physical, Rectangle};

/// Blit the `dst` region of the frame's current DRAW framebuffer into `into`.
///
/// Binds a temporary FBO with `into` as `COLOR_ATTACHMENT0`, disables the scissor test,
/// blits the `dst` region into `(0, 0, size)` of `into`, then restores the previous state
/// and deletes the temporary FBO.
pub fn capture_framebuffer_region(
    frame: &mut GlesFrame<'_, '_>,
    dst: Rectangle<i32, Physical>,
    into: &GlesTexture,
) -> Result<(), GlesError> {
    let size = into.size();
    frame.with_context(|gl| unsafe {
        while gl.GetError() != ffi::NO_ERROR {}

        let mut current_fbo = 0i32;
        gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_fbo as *mut _);

        // BlitFramebuffer is affected by the scissor test, we don't want that.
        gl.Disable(ffi::SCISSOR_TEST);

        let mut fbo = 0;
        gl.GenFramebuffers(1, &mut fbo as *mut _);
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);

        gl.FramebufferTexture2D(
            ffi::DRAW_FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            into.tex_id(),
            0,
        );

        gl.BlitFramebuffer(
            dst.loc.x,
            dst.loc.y,
            dst.loc.x + dst.size.w,
            dst.loc.y + dst.size.h,
            0,
            0,
            size.w,
            size.h,
            ffi::COLOR_BUFFER_BIT,
            ffi::LINEAR,
        );

        // Restore state set by GlesFrame that we just modified.
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_fbo as u32);
        gl.Enable(ffi::SCISSOR_TEST);

        gl.DeleteFramebuffers(1, &mut fbo as *mut _);

        if gl.GetError() != ffi::NO_ERROR {
            Err(GlesError::BlitError)
        } else {
            Ok(())
        }
    })?
}

/// Vulkan counterpart of [`capture_framebuffer_region`]: blits `dst` from the
/// frame's bound target into a renderer-owned `VulkanTexture`, leaving it
/// shader-readable for later draws in the same frame.
#[cfg(feature = "vulkan")]
pub fn capture_framebuffer_region_vulkan(
    frame: &mut smithay::backend::renderer::vulkan::VulkanFrame<'_, '_>,
    dst: Rectangle<i32, Physical>,
    into: &smithay::backend::renderer::vulkan::VulkanTexture,
) -> Result<(), smithay::backend::renderer::vulkan::VulkanError> {
    use smithay::backend::renderer::Texture as _;
    use smithay::utils::Size;
    let size = into.size();
    frame.blit_framebuffer_to_texture(
        into,
        dst,
        Rectangle::from_size(Size::from((size.w, size.h))),
        smithay::backend::renderer::TextureFilter::Linear,
    )
}

/// Runs one fullscreen/region global shader pass on Vulkan: captures the
/// frame region into `capture`, then draws `program` over the same region
/// sampling the capture as `niri_screen`.
#[cfg(feature = "vulkan")]
#[allow(clippy::too_many_arguments)]
pub fn run_global_pass_vulkan(
    frame: &mut smithay::backend::renderer::vulkan::VulkanFrame<'_, '_>,
    region: Rectangle<i32, Physical>,
    capture: &smithay::backend::renderer::vulkan::VulkanTexture,
    program: &smithay::backend::renderer::vulkan::VulkanPixelProgram,
    uniforms: &[smithay::backend::renderer::vulkan::CustomUniform<'_>],
    alpha: f32,
) -> Result<(), smithay::backend::renderer::vulkan::VulkanError> {
    capture_framebuffer_region_vulkan(frame, region, capture)?;
    let textures = [("niri_screen", capture)];
    frame.render_custom(program, region, &[region], uniforms, &textures, alpha)
}

/// Creates a renderer-owned offscreen texture for a Vulkan shader pass.
#[cfg(feature = "vulkan")]
pub fn create_vulkan_texture(
    frame: &mut smithay::backend::renderer::vulkan::VulkanFrame<'_, '_>,
    size: smithay::utils::Size<i32, smithay::utils::Buffer>,
) -> Result<
    smithay::backend::renderer::vulkan::VulkanTexture,
    smithay::backend::renderer::vulkan::VulkanError,
> {
    use smithay::backend::renderer::{FrameContext as _, Offscreen as _};
    let mut guard = frame.renderer();
    guard
        .as_mut()
        .create_buffer(smithay::backend::allocator::Fourcc::Abgr8888, size)
}

/// Runs one custom shader pass into a fresh renderer-owned offscreen texture of `size`,
/// returning it shader-readable for later draws in the same frame.
///
/// This is the Vulkan counterpart of rendering one pass into an `OffscreenBuffer`: the
/// destination is fully covered, contents are discarded, and blending is disabled.
#[cfg(feature = "vulkan")]
pub fn render_custom_pass_to_texture_vulkan(
    frame: &mut smithay::backend::renderer::vulkan::VulkanFrame<'_, '_>,
    size: smithay::utils::Size<i32, smithay::utils::Buffer>,
    program: &smithay::backend::renderer::vulkan::VulkanPixelProgram,
    uniforms: &[smithay::backend::renderer::vulkan::CustomUniform<'_>],
    textures: &[(&str, &smithay::backend::renderer::vulkan::VulkanTexture)],
) -> Result<
    smithay::backend::renderer::vulkan::VulkanTexture,
    smithay::backend::renderer::vulkan::VulkanError,
> {
    use smithay::backend::renderer::vulkan::CustomPass;
    let dst = create_vulkan_texture(frame, size)?;
    frame.render_custom_passes(&[CustomPass {
        dst: &dst,
        program,
        uniforms,
        textures,
    }])?;
    Ok(dst)
}

#[cfg(all(test, feature = "vulkan"))]
mod vulkan_tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::vulkan::VulkanRenderer;
    use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};
    use smithay::utils::{Rectangle, Size, Transform};

    use super::capture_framebuffer_region_vulkan;

    #[test]
    fn capture_then_global_shader_pass_reads_back() {
        use smithay::backend::renderer::vulkan::{
            CustomUniform, CustomUniformValue, VulkanRenderer,
        };
        use smithay::backend::vulkan::version::Version;
        use smithay::backend::vulkan::{Instance, PhysicalDevice};

        use super::run_global_pass_vulkan;

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
            let program = match super::super::shaders::compile_vulkan_program(
                &mut renderer,
                "void main() { vec4 c = texture2D(niri_screen, niri_v_coords); gl_FragColor = vec4(c.g, 0.0, 0.0, 1.0); }",
                &[],
                &["niri_screen"],
            ) {
                Ok(program) => program,
                Err(_) => continue,
            };
            let size = Size::<i32, smithay::utils::Buffer>::from((8, 8));
            let output_size = Size::<i32, smithay::utils::Physical>::from((8, 8));
            let full = Rectangle::from_size(output_size);

            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, size)
                .expect("target");
            let capture = renderer
                .create_buffer(Fourcc::Argb8888, size)
                .expect("capture");
            let mut fb = renderer.bind(&mut target).expect("bind");
            let mut frame = renderer
                .render(&mut fb, output_size, Transform::Normal)
                .expect("frame");
            frame
                .clear(Color32F::new(0.0, 1.0, 0.0, 1.0), &[full])
                .expect("clear");
            let mut uniforms = vec![
                CustomUniform {
                    name: "niri_size",
                    value: CustomUniformValue::Vec2([8.0, 8.0]),
                },
                CustomUniform {
                    name: "niri_scale",
                    value: CustomUniformValue::Float(1.0),
                },
            ];
            for (name, value) in [
                ("niri_hdr_pq", 0.0),
                ("niri_ref_lum_scale", 1.0),
                ("niri_hdr_ref_scale", 1.0),
                ("niri_linear", 0.0),
                ("niri_linear_scale", 0.0),
                ("niri_linear_to_ref", 0.0),
                ("niri_hdr_to_sdr", 0.0),
                ("niri_pq_gamut", 0.0),
                ("niri_use_gamut", 0.0),
                ("niri_tonemap", 0.0),
                ("niri_tm_v", 0.0),
                ("niri_tm_ref_scale", 0.0),
                ("niri_tm_out_scale", 0.0),
            ] {
                uniforms.push(CustomUniform {
                    name,
                    value: CustomUniformValue::Float(value),
                });
            }
            uniforms.push(CustomUniform {
                name: "niri_gamut",
                value: CustomUniformValue::Mat3([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]),
            });
            run_global_pass_vulkan(&mut frame, full, &capture, &program, &uniforms, 1.0)
                .expect("global pass");
            frame.finish().expect("finish");

            let mapping = renderer
                .copy_framebuffer(&fb, Rectangle::from_size(size), Fourcc::Argb8888)
                .expect("copy");
            let pixels = renderer.map_texture(&mapping).expect("map");
            assert_eq!(&pixels[0..4], &[0, 0, 255, 255], "expected red");
            eprintln!(
                "Vulkan capture + global shader pass OK on {}",
                device.name()
            );
            return;
        }
    }

    #[test]
    fn captures_a_framebuffer_region_into_a_vulkan_texture() {
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
            let size = Size::<i32, smithay::utils::Buffer>::from((8, 8));
            let output_size = Size::<i32, smithay::utils::Physical>::from((8, 8));
            let full = Rectangle::from_size(output_size);

            let mut target = renderer
                .create_buffer(Fourcc::Argb8888, size)
                .expect("target");
            let mut capture = renderer
                .create_buffer(Fourcc::Argb8888, size)
                .expect("capture");
            let mut fb = renderer.bind(&mut target).expect("bind target");
            let mut frame = renderer
                .render(&mut fb, output_size, Transform::Normal)
                .expect("begin frame");
            frame
                .clear(Color32F::new(0.0, 1.0, 0.0, 1.0), &[full])
                .expect("clear");
            capture_framebuffer_region_vulkan(&mut frame, full, &capture).expect("capture region");
            frame.finish().expect("finish");

            let capture_fb = renderer.bind(&mut capture).expect("bind capture");
            let mapping = renderer
                .copy_framebuffer(&capture_fb, Rectangle::from_size(size), Fourcc::Argb8888)
                .expect("copy capture");
            let pixels = renderer.map_texture(&mapping).expect("map");
            // Argb8888 little-endian: B, G, R, A -> green.
            assert_eq!(&pixels[0..4], &[0, 255, 0, 255], "unexpected pixel");
            eprintln!("Vulkan framebuffer region capture OK on {}", device.name());
            return;
        }
    }
}
