use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use glam::Mat3;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ImportMem;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName,
    UniformType, UniformValue,
};
#[cfg(feature = "vulkan")]
use smithay::backend::renderer::vulkan::{
    CustomUniformDecl, CustomUniformKind, VulkanPixelProgram, VulkanRenderer,
    texture_bindings_glsl, uniform_block_glsl,
};
use smithay::utils::Size;

use super::renderer::NiriRenderer;
use super::shader_element::ShaderProgram;
use crate::render_helpers::blur::BlurProgram;

pub struct Shaders {
    pub texture_hdr: Option<GlesTexProgram>,
    pub texture_hdr_to_sdr: Option<GlesTexProgram>,
    pub border: Option<ShaderProgram>,
    pub panel: Option<ShaderProgram>,
    pub decoration_light: Option<ShaderProgram>,
    pub decorations: RefCell<HashMap<u64, ShaderProgram>>,
    pub shadow: Option<ShaderProgram>,
    pub clipped_surface: Option<GlesTexProgram>,
    pub postprocess_and_clip: Option<GlesTexProgram>,
    pub resize: Option<ShaderProgram>,
    pub gradient_fade: Option<GlesTexProgram>,
    pub blur: Option<BlurProgram>,
    pub custom_resize: RefCell<Option<ShaderProgram>>,
    pub custom_close: RefCell<Option<ShaderProgram>>,
    pub custom_open: RefCell<Option<ShaderProgram>>,
    pub custom_global_passes: RefCell<Vec<ShaderProgram>>,
    pub custom_global_pass_buffers: RefCell<Vec<Option<ShaderProgram>>>,
    pub scoped: RefCell<HashMap<u64, Vec<ShaderProgram>>>,
    /// 1×1 transparent-black texture used to seed a feedback shader's first-frame niri_prev /
    /// niri_buffer (an "empty feedback buffer"), instead of ingesting the live screen.
    pub black_texture: GlesTexture,
}

/// A GLES texture program, or its Vulkan counterpart.
#[derive(Debug, Clone)]
pub enum NiriTexProgram {
    Gles(GlesTexProgram),
    #[cfg(feature = "vulkan")]
    Vulkan(VulkanPixelProgram),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramType {
    Border,
    Panel,
    DecorationLight,
    Decoration(u64),
    Shadow,
    Resize,
    Close,
    Open,
    Global,
    GlobalBuffer,
    GlobalPass(usize),
    GlobalPassBuffer(usize),
    Scoped(u64, usize),
}

impl Shaders {
    fn compile(renderer: &mut GlesRenderer) -> Self {
        let _span = tracy_client::span!("Shaders::compile");

        let texture_hdr = renderer
            .compile_custom_texture_shader(
                concat!(include_str!("texture_hdr.frag"), include_str!("hdr.frag"),),
                &[
                    UniformName::new("niri_hdr_pq", UniformType::_1f),
                    UniformName::new("niri_ref_lum_scale", UniformType::_1f),
                    UniformName::new("niri_linear", UniformType::_1f),
                    UniformName::new("niri_linear_scale", UniformType::_1f),
                    UniformName::new("niri_linear_to_ref", UniformType::_1f),
                    UniformName::new("niri_hdr_to_sdr", UniformType::_1f),
                ],
            )
            .map_err(|err| {
                warn!("error compiling HDR texture shader: {err:?}");
            })
            .ok();

        let texture_hdr_to_sdr = renderer
            .compile_custom_texture_shader(
                include_str!("texture_hdr_to_sdr.frag"),
                &[UniformName::new("niri_ref_lum_scale", UniformType::_1f)],
            )
            .map_err(|err| {
                warn!("error compiling HDR-to-SDR texture shader: {err:?}");
            })
            .ok();

        let border = compile_decoration_program(renderer, None)
            .map_err(|err| warn!("error compiling border shader: {err:?}"))
            .ok();

        let decoration_light = ShaderProgram::compile(
            renderer,
            include_str!("decoration_light.frag"),
            &[
                UniformName::new("light_gain", UniformType::_1f),
                UniformName::new("light_tex_scale", UniformType::_2f),
            ],
            &["light_texture"],
        )
        .map_err(|err| warn!("error compiling decoration light shader: {err:?}"))
        .ok();

        let panel = ShaderProgram::compile(
            renderer,
            include_str!("panel.frag"),
            &[
                UniformName::new("niri_panel_inv", UniformType::Matrix3x3),
                UniformName::new("niri_panel_dim", UniformType::_1f),
            ],
            &["niri_panel_tex"],
        )
        .map_err(|err| {
            warn!("error compiling panel shader: {err:?}");
        })
        .ok();

        let shadow = ShaderProgram::compile(
            renderer,
            concat!(
                include_str!("shadow.frag"),
                include_str!("rounding_alpha.frag")
            ),
            &[
                UniformName::new("shadow_color", UniformType::_4f),
                UniformName::new("sigma", UniformType::_1f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("corner_radius", UniformType::_4f),
                UniformName::new("window_input_to_geo", UniformType::Matrix3x3),
                UniformName::new("window_geo_size", UniformType::_2f),
                UniformName::new("window_corner_radius", UniformType::_4f),
            ],
            &[],
        )
        .map_err(|err| {
            warn!("error compiling shadow shader: {err:?}");
        })
        .ok();

        let clipped_surface = renderer
            .compile_custom_texture_shader(
                concat!(
                    include_str!("clipped_surface.frag"),
                    include_str!("rounding_alpha.frag"),
                    include_str!("hdr.frag"),
                    "\nvec4 postprocess(vec4 color) { return color; }",
                ),
                &[
                    UniformName::new("niri_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                    UniformName::new("lg_refraction_strength", UniformType::_1f),
                    UniformName::new("lg_power_factor", UniformType::_1f),
                    UniformName::new("lg_refraction_a", UniformType::_1f),
                    UniformName::new("lg_refraction_b", UniformType::_1f),
                    UniformName::new("lg_refraction_c", UniformType::_1f),
                    UniformName::new("lg_refraction_d", UniformType::_1f),
                    UniformName::new("lg_refraction_power", UniformType::_1f),
                    UniformName::new("lg_physical_refraction", UniformType::_1f),
                    UniformName::new("lg_glow_weight", UniformType::_1f),
                    UniformName::new("lg_glow_bias", UniformType::_1f),
                    UniformName::new("lg_glow_edge0", UniformType::_1f),
                    UniformName::new("lg_glow_edge1", UniformType::_1f),
                    UniformName::new("lg_edge_lighting", UniformType::_1f),
                    UniformName::new("lg_fringing", UniformType::_1f),
                    UniformName::new("lg_refraction_dilute", UniformType::_1f),
                    UniformName::new("lg_dilute_strength", UniformType::_1f),
                    UniformName::new("lg_dilute_fringing", UniformType::_1f),
                    UniformName::new("lg_lens_distortion", UniformType::_1f),
                    UniformName::new("lg_brightness", UniformType::_1f),
                    UniformName::new("lg_contrast", UniformType::_1f),
                    UniformName::new("lg_saturation", UniformType::_1f),
                    UniformName::new("lg_vibrancy", UniformType::_1f),
                    UniformName::new("lg_adaptive_dim", UniformType::_1f),
                    UniformName::new("lg_adaptive_boost", UniformType::_1f),
                    UniformName::new("lg_edge_thickness", UniformType::_1f),
                    UniformName::new("lg_padding_pixels", UniformType::_1f),
                    UniformName::new("niri_hdr_pq", UniformType::_1f),
                    UniformName::new("niri_ref_lum_scale", UniformType::_1f),
                    UniformName::new("niri_linear", UniformType::_1f),
                    UniformName::new("niri_linear_scale", UniformType::_1f),
                    UniformName::new("niri_linear_to_ref", UniformType::_1f),
                    UniformName::new("niri_hdr_to_sdr", UniformType::_1f),
                ],
            )
            .map_err(|err| {
                warn!("error compiling clipped surface shader: {err:?}");
            })
            .ok();

        let postprocess_and_clip = renderer
            .compile_custom_texture_shader(
                concat!(
                    include_str!("clipped_surface.frag"),
                    include_str!("rounding_alpha.frag"),
                    include_str!("postprocess.frag"),
                    include_str!("hdr.frag"),
                ),
                &[
                    UniformName::new("niri_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                    UniformName::new("noise", UniformType::_1f),
                    UniformName::new("saturation", UniformType::_1f),
                    UniformName::new("bg_color", UniformType::_4f),
                    UniformName::new("lg_refraction_strength", UniformType::_1f),
                    UniformName::new("lg_power_factor", UniformType::_1f),
                    UniformName::new("lg_refraction_a", UniformType::_1f),
                    UniformName::new("lg_refraction_b", UniformType::_1f),
                    UniformName::new("lg_refraction_c", UniformType::_1f),
                    UniformName::new("lg_refraction_d", UniformType::_1f),
                    UniformName::new("lg_refraction_power", UniformType::_1f),
                    UniformName::new("lg_physical_refraction", UniformType::_1f),
                    UniformName::new("lg_glow_weight", UniformType::_1f),
                    UniformName::new("lg_glow_bias", UniformType::_1f),
                    UniformName::new("lg_glow_edge0", UniformType::_1f),
                    UniformName::new("lg_glow_edge1", UniformType::_1f),
                    UniformName::new("lg_edge_lighting", UniformType::_1f),
                    UniformName::new("lg_fringing", UniformType::_1f),
                    UniformName::new("lg_refraction_dilute", UniformType::_1f),
                    UniformName::new("lg_dilute_strength", UniformType::_1f),
                    UniformName::new("lg_dilute_fringing", UniformType::_1f),
                    UniformName::new("lg_lens_distortion", UniformType::_1f),
                    UniformName::new("lg_brightness", UniformType::_1f),
                    UniformName::new("lg_contrast", UniformType::_1f),
                    UniformName::new("lg_saturation", UniformType::_1f),
                    UniformName::new("lg_vibrancy", UniformType::_1f),
                    UniformName::new("lg_adaptive_dim", UniformType::_1f),
                    UniformName::new("lg_adaptive_boost", UniformType::_1f),
                    UniformName::new("lg_edge_thickness", UniformType::_1f),
                    UniformName::new("lg_padding_pixels", UniformType::_1f),
                    UniformName::new("niri_hdr_pq", UniformType::_1f),
                    UniformName::new("niri_ref_lum_scale", UniformType::_1f),
                    UniformName::new("niri_linear", UniformType::_1f),
                    UniformName::new("niri_linear_scale", UniformType::_1f),
                    UniformName::new("niri_linear_to_ref", UniformType::_1f),
                    UniformName::new("niri_hdr_to_sdr", UniformType::_1f),
                    UniformName::new("surface_to_geo", UniformType::Matrix3x3),
                    UniformName::new("alpha_mask_enabled", UniformType::_1i),
                    UniformName::new("alpha_threshold", UniformType::_1f),
                    UniformName::new("surface_tex", UniformType::_1i),
                ],
            )
            .map_err(|err| {
                warn!("error compiling postprocess_and_clip shader: {err:?}");
            })
            .ok();

        let resize = compile_resize_program(renderer, include_str!("resize.frag"))
            .map_err(|err| {
                warn!("error compiling resize shader: {err:?}");
            })
            .ok();

        let gradient_fade = renderer
            .compile_custom_texture_shader(
                concat!(include_str!("gradient_fade.frag"), include_str!("hdr.frag")),
                &[
                    UniformName::new("cutoff", UniformType::_2f),
                    UniformName::new("niri_hdr_pq", UniformType::_1f),
                    UniformName::new("niri_ref_lum_scale", UniformType::_1f),
                    UniformName::new("niri_linear", UniformType::_1f),
                    UniformName::new("niri_linear_scale", UniformType::_1f),
                    UniformName::new("niri_linear_to_ref", UniformType::_1f),
                    UniformName::new("niri_hdr_to_sdr", UniformType::_1f),
                ],
            )
            .map_err(|err| {
                warn!("error compiling gradient fade shader: {err:?}");
            })
            .ok();

        let blur = BlurProgram::compile(renderer)
            .map_err(|err| {
                warn!("error compiling blur shaders: {err:?}");
            })
            .ok();

        // 1×1 transparent black; sampled (clamped) it returns black for every uv at any size.
        let black_texture = renderer
            .import_memory(&[0u8, 0, 0, 0], Fourcc::Abgr8888, Size::from((1, 1)), false)
            .expect("importing a 1x1 black texture must not fail");

        Self {
            texture_hdr,
            texture_hdr_to_sdr,
            border,
            panel,
            decoration_light,
            decorations: RefCell::new(HashMap::new()),
            shadow,
            clipped_surface,
            postprocess_and_clip,
            resize,
            gradient_fade,
            blur,
            custom_resize: RefCell::new(None),
            custom_close: RefCell::new(None),
            custom_open: RefCell::new(None),
            custom_global_passes: RefCell::new(Vec::new()),
            custom_global_pass_buffers: RefCell::new(Vec::new()),
            scoped: RefCell::new(HashMap::new()),
            black_texture,
        }
    }

    pub fn get_from_frame<'a>(frame: &'a mut GlesFrame<'_, '_>) -> &'a Self {
        let data = frame.egl_context().user_data();
        data.get()
            .expect("shaders::init() must be called when creating the renderer")
    }

    /// Number of compiled global post-process passes on either backend.
    ///
    /// The render path uses this to size the per-pass chain state; it must not
    /// depend on a GLES renderer being present (the TTY Vulkan path has none).
    pub fn global_pass_count(renderer: &mut impl NiriRenderer) -> usize {
        if let Some(renderer) = renderer.as_gles_renderer() {
            return Self::get(renderer).custom_global_passes.borrow().len();
        }
        #[cfg(feature = "vulkan")]
        if let Some(renderer) = renderer.as_vulkan_renderer() {
            return get_vulkan_renderer(renderer)
                .map_or(0, |shaders| shaders.global_passes.borrow().len());
        }
        0
    }

    /// Whether the renderer has a compiled program of this type, on either backend.
    pub fn has_program(renderer: &mut impl NiriRenderer, program: ProgramType) -> bool {
        if let Some(renderer) = renderer.as_gles_renderer() {
            return Self::get(renderer).program(program).is_some();
        }
        #[cfg(feature = "vulkan")]
        if let Some(renderer) = renderer.as_vulkan_renderer() {
            return get_vulkan_renderer(renderer)
                .and_then(|shaders| shaders.program(program))
                .is_some();
        }
        false
    }

    pub fn get(renderer: &mut GlesRenderer) -> &Self {
        let data = renderer.egl_context().user_data();
        data.get()
            .expect("shaders::init() must be called when creating the renderer")
    }

    pub fn replace_custom_resize_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_resize.replace(program)
    }

    pub fn replace_custom_close_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_close.replace(program)
    }

    pub fn replace_custom_open_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_open.replace(program)
    }

    pub fn program(&self, program: ProgramType) -> Option<ShaderProgram> {
        match program {
            ProgramType::Border => self.border.clone(),
            ProgramType::Panel => self.panel.clone(),
            ProgramType::DecorationLight => self.decoration_light.clone(),
            ProgramType::Decoration(key) => self
                .decorations
                .borrow()
                .get(&key)
                .cloned()
                .or_else(|| self.border.clone()),
            ProgramType::Shadow => self.shadow.clone(),
            ProgramType::Resize => self
                .custom_resize
                .borrow()
                .clone()
                .or_else(|| self.resize.clone()),
            ProgramType::Close => self.custom_close.borrow().clone(),
            ProgramType::Open => self.custom_open.borrow().clone(),
            ProgramType::Global => self.custom_global_passes.borrow().first().cloned(),
            ProgramType::GlobalBuffer => self
                .custom_global_pass_buffers
                .borrow()
                .first()
                .cloned()
                .flatten(),
            ProgramType::GlobalPass(i) => self.custom_global_passes.borrow().get(i).cloned(),
            ProgramType::GlobalPassBuffer(i) => self
                .custom_global_pass_buffers
                .borrow()
                .get(i)
                .cloned()
                .flatten(),
            ProgramType::Scoped(key, i) => self
                .scoped
                .borrow()
                .get(&key)
                .and_then(|chain| chain.get(i))
                .cloned(),
        }
    }
}

pub fn init(renderer: &mut GlesRenderer) {
    let shaders = Shaders::compile(renderer);
    let data = renderer.egl_context().user_data();
    if !data.insert_if_missing(|| shaders) {
        error!("shaders were already compiled");
    }
}

fn compile_resize_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("resize_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("resize_epilogue.frag"));
    program.push_str(include_str!("rounding_alpha.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_curr_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_to_prev_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_to_next_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex_prev", UniformType::Matrix3x3),
            UniformName::new("niri_geo_to_tex_next", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_corner_radius", UniformType::_4f),
            UniformName::new("niri_clip_to_geometry", UniformType::_1f),
        ],
        &["niri_tex_prev", "niri_tex_next"],
    )
}

pub fn set_custom_resize_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_resize_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom resize shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).replace_custom_resize_program(program)
        && let Err(err) = prev.destroy(renderer)
    {
        warn!("error destroying previous custom resize shader: {err:?}");
    }
}

fn compile_close_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("close_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("close_epilogue.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_geo", UniformType::Matrix3x3),
            UniformName::new("niri_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_random_seed", UniformType::_1f),
        ],
        &["niri_tex"],
    )
}

pub fn set_custom_close_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_close_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom close shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).replace_custom_close_program(program)
        && let Err(err) = prev.destroy(renderer)
    {
        warn!("error destroying previous custom close shader: {err:?}");
    }
}

fn compile_open_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("open_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("open_epilogue.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_geo", UniformType::Matrix3x3),
            UniformName::new("niri_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_random_seed", UniformType::_1f),
        ],
        &["niri_tex"],
    )
}

pub fn set_custom_open_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_open_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom open shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).replace_custom_open_program(program)
        && let Err(err) = prev.destroy(renderer)
    {
        warn!("error destroying previous custom open shader: {err:?}");
    }
}

fn compile_global_program(
    renderer: &mut GlesRenderer,
    src: &str,
    hyprland: bool,
) -> Result<ShaderProgram, GlesError> {
    let mut program = if hyprland {
        include_str!("global_hypr_prelude.frag").to_string()
    } else {
        include_str!("global_prelude.frag").to_string()
    };
    program.push_str(src);
    if !hyprland {
        program.push_str(include_str!("global_epilogue.frag"));
    }

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_time", UniformType::_1f),
            UniformName::new("niri_cursor", UniformType::_2f),
            UniformName::new("niri_region", UniformType::_4f),
            UniformName::new("niri_output_size", UniformType::_2f),
        ],
        &[
            "niri_screen",
            "niri_prev",
            "niri_screen_prev",
            "niri_buffer",
            "niri_source",
        ],
    )
}

fn compile_global_buffer_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("global_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("global_buffer_epilogue.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_time", UniformType::_1f),
            UniformName::new("niri_cursor", UniformType::_2f),
            UniformName::new("niri_region", UniformType::_4f),
            UniformName::new("niri_output_size", UniformType::_2f),
        ],
        &[
            "niri_screen",
            "niri_prev",
            "niri_screen_prev",
            "niri_buffer",
            "niri_source",
        ],
    )
}

/// Install a whole pass chain: for each pass compile its display program and (niri mode only,
/// when the source defines `global_buffer`) a dedicated-buffer program. Replaces both registry
/// vecs wholesale and destroys every previously-installed program. An empty slice disables the
/// chain. If any pass fails to compile, the whole chain is dropped (empty vecs) so we never run a
/// partial chain.
pub fn set_custom_global_passes(renderer: &mut GlesRenderer, passes: &[(String, bool)]) {
    let mut display = Vec::with_capacity(passes.len());
    let mut buffers = Vec::with_capacity(passes.len());
    let mut ok = true;

    for (src, hyprland) in passes {
        match compile_global_program(renderer, src, *hyprland) {
            Ok(p) => display.push(p),
            Err(err) => {
                warn!("error compiling global shader pass: {err:?}");
                ok = false;
                break;
            }
        }
        let buffer = match (src.contains("global_buffer"), *hyprland) {
            (true, false) => match compile_global_buffer_program(renderer, src) {
                Ok(p) => Some(p),
                Err(err) => {
                    warn!("error compiling global_buffer pass: {err:?}");
                    None
                }
            },
            _ => None,
        };
        buffers.push(buffer);
    }

    if !ok {
        // Destroy anything compiled this round before bailing.
        for p in display.drain(..) {
            let _ = p.destroy(renderer);
        }
        for p in buffers.drain(..).flatten() {
            let _ = p.destroy(renderer);
        }
        display = Vec::new();
        buffers = Vec::new();
    }

    let shaders = Shaders::get(renderer);
    let old_display = shaders.custom_global_passes.replace(display);
    let old_buffers = shaders.custom_global_pass_buffers.replace(buffers);
    for p in old_display {
        if let Err(err) = p.destroy(renderer) {
            warn!("error destroying previous global pass: {err:?}");
        }
    }
    for p in old_buffers.into_iter().flatten() {
        if let Err(err) = p.destroy(renderer) {
            warn!("error destroying previous global_buffer pass: {err:?}");
        }
    }
}

/// Stable hash of a resolved pass list — the cache key for a scoped shader chain. Pure function of
/// the source strings and hyprland flags (no nondeterminism).
pub fn scoped_key(passes: &[(String, bool)]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (src, hypr) in passes {
        src.hash(&mut h);
        hypr.hash(&mut h);
    }
    h.finish()
}

/// Install the set of scoped shader chains. Each entry is a resolved `(source, hyprland)` pass
/// list. Compiles any key not already cached, and destroys any cached key no longer referenced.
/// Identical pass lists share one compiled chain (same key).
pub fn set_scoped_programs(renderer: &mut GlesRenderer, chains: &[Vec<(String, bool)>]) {
    use std::collections::HashSet;
    let wanted: HashSet<u64> = chains.iter().map(|c| scoped_key(c)).collect();

    // Drop + destroy stale keys: collect the chains first (releasing the borrow), then destroy.
    let stale_chains: Vec<Vec<ShaderProgram>> = {
        let mut scoped = Shaders::get(renderer).scoped.borrow_mut();
        let stale: Vec<u64> = scoped
            .keys()
            .copied()
            .filter(|k| !wanted.contains(k))
            .collect();
        stale
            .into_iter()
            .filter_map(|k| scoped.remove(&k))
            .collect()
    };
    for chain in stale_chains {
        for p in chain {
            if let Err(err) = p.destroy(renderer) {
                warn!("error destroying scoped shader program: {err:?}");
            }
        }
    }

    // Compile missing keys.
    for passes in chains {
        if passes.is_empty() {
            continue;
        }
        let key = scoped_key(passes);
        if Shaders::get(renderer).scoped.borrow().contains_key(&key) {
            continue;
        }
        let mut compiled = Vec::with_capacity(passes.len());
        let mut ok = true;
        for (src, hyprland) in passes {
            match compile_global_program(renderer, src, *hyprland) {
                Ok(p) => compiled.push(p),
                Err(err) => {
                    warn!("error compiling scoped shader: {err:?}");
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            Shaders::get(renderer)
                .scoped
                .borrow_mut()
                .insert(key, compiled);
        } else {
            for p in compiled {
                let _ = p.destroy(renderer);
            }
        }
    }
}

pub fn mat3_uniform(name: &str, mat: Mat3) -> Uniform<'_> {
    Uniform::new(
        name,
        UniformValue::Matrix3x3 {
            matrices: vec![mat.to_cols_array()],
            transpose: false,
        },
    )
}

/// Custom and legacy rings share a coordinate contract and the same editable wax source.
/// Vulkan-compiled niri shader programs.
///
/// The TTY Vulkan renderer draws the same GLSL sources as GLES, compiled to
/// SPIR-V through better-smithay's custom pixel pipeline. Textured programs
/// (panel, decoration light, blur, background composite) sample
/// renderer-owned `VulkanTexture`s; producers that only exist on the GLES
/// side (resize/close/open snapshots, xray backdrop buffers) still keep their
/// GLES path.
#[cfg(feature = "vulkan")]
pub struct VulkanShaders {
    pub border: Option<VulkanPixelProgram>,
    pub shadow: Option<VulkanPixelProgram>,
    pub clipped_surface: Option<VulkanPixelProgram>,
    /// Textured decoration light (`ShaderRenderElement` with `ProgramType::DecorationLight`).
    pub decoration_light: Option<VulkanPixelProgram>,
    /// Perspective-tilted carousel panel (`ProgramType::Panel`).
    pub panel: Option<VulkanPixelProgram>,
    /// Dual-Kawase downsample/upsample passes for the Vulkan background blur.
    pub blur_down: Option<VulkanPixelProgram>,
    pub blur_up: Option<VulkanPixelProgram>,
    /// Background-effect composite (`clipped_surface.frag` + `postprocess.frag`), used by
    /// `FramebufferEffectElement` on Vulkan and carrying the liquid-glass uniforms.
    pub postprocess_and_clip: Option<VulkanPixelProgram>,
    pub decorations: RefCell<HashMap<u64, VulkanPixelProgram>>,
    pub global_passes: RefCell<Vec<VulkanPixelProgram>>,
    pub global_buffers: RefCell<Vec<Option<VulkanPixelProgram>>>,
    pub scoped: RefCell<HashMap<u64, Vec<VulkanPixelProgram>>>,
    /// 1×1 transparent-black texture used to seed a global/feedback shader's first-frame
    /// `niri_prev` / `niri_buffer` (an "empty feedback buffer").
    pub black_texture: Option<smithay::backend::renderer::vulkan::VulkanTexture>,
}

#[cfg(feature = "vulkan")]
impl VulkanShaders {
    fn compile(renderer: &mut VulkanRenderer) -> Self {
        let _span = tracy_client::span!("VulkanShaders::compile");

        let mut border_src = String::from(
            "#define WAX_STRENGTH rainbow_ripple.y\n\
             #define WAX_BRIGHTNESS rainbow_ripple.z\n\
             #define WAX_PHASE rainbow_ripple.x\n",
        );
        border_src.push_str(include_str!("border.frag"));
        border_src.push_str(include_str!("rounding_alpha.frag"));
        border_src.push_str(include_str!(
            "../../../resources/shaders/focus-ring/rainbow-ripple.frag"
        ));
        let border = compile_vulkan_program(
            renderer,
            &border_src,
            &[
                UniformName::new("colorspace", UniformType::_1f),
                UniformName::new("hue_interpolation", UniformType::_1f),
                UniformName::new("color_from", UniformType::_4f),
                UniformName::new("color_to", UniformType::_4f),
                UniformName::new("grad_offset", UniformType::_2f),
                UniformName::new("grad_width", UniformType::_1f),
                UniformName::new("grad_vec", UniformType::_2f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("outer_radius", UniformType::_4f),
                UniformName::new("border_width", UniformType::_1f),
                UniformName::new("rainbow_ripple", UniformType::_4f),
                UniformName::new("ring_width", UniformType::_1f),
                UniformName::new("emission_threshold", UniformType::_1f),
                UniformName::new("niri_time", UniformType::_1f),
            ],
            &[],
        )
        .map_err(|err| warn!("error compiling Vulkan border shader: {err:?}"))
        .ok();

        let shadow = compile_vulkan_program(
            renderer,
            concat!(
                include_str!("shadow.frag"),
                include_str!("rounding_alpha.frag")
            ),
            &[
                UniformName::new("shadow_color", UniformType::_4f),
                UniformName::new("sigma", UniformType::_1f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("corner_radius", UniformType::_4f),
                UniformName::new("window_input_to_geo", UniformType::Matrix3x3),
                UniformName::new("window_geo_size", UniformType::_2f),
                UniformName::new("window_corner_radius", UniformType::_4f),
            ],
            &[],
        )
        .map_err(|err| warn!("error compiling Vulkan shadow shader: {err:?}"))
        .ok();

        let clipped_surface = {
            let src = concat!(
                include_str!("clipped_surface.frag"),
                include_str!("rounding_alpha.frag"),
                "\nvec4 postprocess(vec4 color) { return color; }",
            );
            compile_vulkan_program(renderer, src, &clipped_surface_uniform_names(), &["tex"])
                .map_err(|err| warn!("error compiling Vulkan clipped surface shader: {err:?}"))
                .ok()
        };

        let decoration_light = compile_vulkan_program(
            renderer,
            include_str!("decoration_light.frag"),
            &[
                UniformName::new("light_gain", UniformType::_1f),
                UniformName::new("light_tex_scale", UniformType::_2f),
            ],
            &["light_texture"],
        )
        .map_err(|err| warn!("error compiling Vulkan decoration light shader: {err:?}"))
        .ok();

        let panel = compile_vulkan_program(
            renderer,
            include_str!("panel.frag"),
            &[
                UniformName::new("niri_panel_inv", UniformType::Matrix3x3),
                UniformName::new("niri_panel_dim", UniformType::_1f),
            ],
            &["niri_panel_tex"],
        )
        .map_err(|err| warn!("error compiling Vulkan panel shader: {err:?}"))
        .ok();

        let blur_down = compile_vulkan_program(
            renderer,
            include_str!("blur_down.frag"),
            &[
                UniformName::new("half_pixel", UniformType::_2f),
                UniformName::new("offset", UniformType::_1f),
            ],
            &["tex"],
        )
        .map_err(|err| warn!("error compiling Vulkan blur_down shader: {err:?}"))
        .ok();
        let blur_up = compile_vulkan_program(
            renderer,
            include_str!("blur_up.frag"),
            &[
                UniformName::new("half_pixel", UniformType::_2f),
                UniformName::new("offset", UniformType::_1f),
            ],
            &["tex"],
        )
        .map_err(|err| warn!("error compiling Vulkan blur_up shader: {err:?}"))
        .ok();

        // Background-effect composite: same GLSL as the GLES `postprocess_and_clip`
        // program. The custom-uniform block has no integer kind, so the alpha-mask flag
        // is shipped as a float and the shader's `== 1` test is rewritten to `> 0.5`.
        let postprocess_and_clip = {
            let mut src = String::from(include_str!("clipped_surface.frag"));
            src.push_str(include_str!("rounding_alpha.frag"));
            src.push_str(
                &include_str!("postprocess.frag")
                    .replace(
                        "uniform int alpha_mask_enabled;",
                        "uniform float alpha_mask_enabled;",
                    )
                    .replace("alpha_mask_enabled == 1", "alpha_mask_enabled > 0.5"),
            );
            let mut uniforms = clipped_surface_uniform_names();
            uniforms.extend([
                UniformName::new("noise", UniformType::_1f),
                UniformName::new("saturation", UniformType::_1f),
                UniformName::new("bg_color", UniformType::_4f),
                UniformName::new("surface_to_geo", UniformType::Matrix3x3),
                UniformName::new("alpha_mask_enabled", UniformType::_1f),
                UniformName::new("alpha_threshold", UniformType::_1f),
            ]);
            compile_vulkan_program(renderer, &src, &uniforms, &["tex", "surface_tex"])
                .map_err(|err| warn!("error compiling Vulkan postprocess shader: {err:?}"))
                .ok()
        };

        Self {
            border,
            shadow,
            clipped_surface,
            decoration_light,
            panel,
            blur_down,
            blur_up,
            postprocess_and_clip,
            decorations: RefCell::new(HashMap::new()),
            global_passes: RefCell::new(Vec::new()),
            global_buffers: RefCell::new(Vec::new()),
            scoped: RefCell::new(HashMap::new()),
            // 1×1 transparent black; sampled (clamped) it returns black for every uv.
            black_texture: renderer
                .import_memory(&[0u8, 0, 0, 0], Fourcc::Abgr8888, Size::from((1, 1)), false)
                .map_err(|err| warn!("error creating Vulkan black texture: {err:?}"))
                .ok(),
        }
    }

    pub fn program(&self, program: ProgramType) -> Option<VulkanPixelProgram> {
        match program {
            ProgramType::Border => self.border.clone(),
            ProgramType::Shadow => self.shadow.clone(),
            ProgramType::Panel => self.panel.clone(),
            ProgramType::DecorationLight => self.decoration_light.clone(),
            ProgramType::Decoration(key) => self.decorations.borrow().get(&key).cloned(),
            ProgramType::GlobalPass(pass) => self.global_passes.borrow().get(pass).cloned(),
            ProgramType::GlobalPassBuffer(pass) => self
                .global_buffers
                .borrow()
                .get(pass)
                .and_then(|program| program.clone()),
            ProgramType::Scoped(key, pass) => self
                .scoped
                .borrow()
                .get(&key)
                .and_then(|chain| chain.get(pass))
                .cloned(),
            _ => None,
        }
    }
}

/// The uniform set of the clipped-surface texture program (shared by both backends).
fn clipped_surface_uniform_names() -> Vec<UniformName<'static>> {
    let mut uniforms = vec![
        UniformName::new("niri_scale", UniformType::_1f),
        UniformName::new("geo_size", UniformType::_2f),
        UniformName::new("corner_radius", UniformType::_4f),
        UniformName::new("input_to_geo", UniformType::Matrix3x3),
        UniformName::new("alpha", UniformType::_1f),
        UniformName::new("tint", UniformType::_1f),
    ];
    for name in [
        "lg_refraction_strength",
        "lg_power_factor",
        "lg_refraction_a",
        "lg_refraction_b",
        "lg_refraction_c",
        "lg_refraction_d",
        "lg_refraction_power",
        "lg_physical_refraction",
        "lg_glow_weight",
        "lg_glow_bias",
        "lg_glow_edge0",
        "lg_glow_edge1",
        "lg_edge_lighting",
        "lg_fringing",
        "lg_refraction_dilute",
        "lg_dilute_strength",
        "lg_dilute_fringing",
        "lg_lens_distortion",
        "lg_brightness",
        "lg_contrast",
        "lg_saturation",
        "lg_vibrancy",
        "lg_adaptive_dim",
        "lg_adaptive_boost",
        "lg_edge_thickness",
        "lg_padding_pixels",
    ] {
        uniforms.push(UniformName::new(name, UniformType::_1f));
    }
    uniforms
}

/// The shader sources referenced by the configuration, keyed by content hash.
fn decoration_shader_sources(config: &niri_config::Config) -> Vec<(u64, Option<&str>)> {
    let layout_parts = config
        .outputs
        .0
        .iter()
        .filter_map(|o| o.layout.as_ref())
        .chain(
            config
                .workspaces
                .iter()
                .filter_map(|w| w.layout.as_ref().map(|l| &l.0)),
        );
    let layout_shaders = layout_parts
        .flat_map(|l| l.focus_ring.iter().chain(l.border.iter()))
        .filter_map(|r| r.shader.as_ref());
    let shaders = config
        .layout
        .focus_ring
        .shader
        .iter()
        .chain(config.layout.border.shader.iter())
        .chain(layout_shaders)
        .chain(
            config
                .window_rules
                .iter()
                .flat_map(|r| r.focus_ring.shader.iter().chain(r.border.shader.iter())),
        );
    shaders
        .filter_map(|shader| Some((shader.key()?, shader.source())))
        .collect()
}

/// Compiles and stores the Vulkan shaders on the renderer, including the
/// configured per-window decoration shaders.
#[cfg(feature = "vulkan")]
pub fn init_vulkan(renderer: &mut VulkanRenderer, config: &niri_config::Config) {
    let mut shaders = VulkanShaders::compile(renderer);
    for (key, source) in decoration_shader_sources(config) {
        match compile_vulkan_decoration_program(renderer, source) {
            Ok(program) => {
                shaders.decorations.get_mut().insert(key, program);
            }
            Err(err) => warn!("error compiling Vulkan decoration shader: {err:?}"),
        }
    }

    #[cfg(feature = "effects")]
    {
        // Global and per-window scoped shader passes: compile the same sources and
        // keep them keyed exactly like the GLES registries so the element draw path
        // can look them up by ProgramType.
        let passes = crate::niri::global_shader_pass_sources(&config.global_shader);
        let mut global_passes = Vec::new();
        let mut global_buffers = Vec::new();
        for (source, hyprland) in &passes {
            match compile_vulkan_global(renderer, source, *hyprland) {
                Ok(program) => global_passes.push(program),
                Err(err) => {
                    warn!("error compiling Vulkan global shader pass: {err:?}");
                    global_passes.clear();
                    break;
                }
            }
            let buffer = match (source.contains("global_buffer"), *hyprland) {
                (true, false) => compile_vulkan_global(renderer, source, false).ok(),
                _ => None,
            };
            global_buffers.push(buffer);
        }
        *shaders.global_passes.get_mut() = global_passes;
        *shaders.global_buffers.get_mut() = global_buffers;

        for chain in crate::niri::scoped_shader_chains(config) {
            if chain.is_empty() {
                continue;
            }
            let key = scoped_key(&chain);
            let mut compiled = Vec::with_capacity(chain.len());
            let mut ok = true;
            for (source, hyprland) in &chain {
                match compile_vulkan_global(renderer, source, *hyprland) {
                    Ok(program) => compiled.push(program),
                    Err(err) => {
                        warn!("error compiling Vulkan scoped shader: {err:?}");
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                shaders.scoped.get_mut().insert(key, compiled);
            }
        }
    }

    renderer.user_data().insert_if_missing(|| shaders);
}

#[cfg(feature = "vulkan")]
fn compile_vulkan_global(
    renderer: &mut VulkanRenderer,
    src: &str,
    hyprland: bool,
) -> anyhow::Result<VulkanPixelProgram> {
    let mut program = if hyprland {
        include_str!("global_hypr_prelude.frag").to_string()
    } else {
        include_str!("global_prelude.frag").to_string()
    };
    program.push_str(src);
    if !hyprland {
        program.push_str(include_str!("global_epilogue.frag"));
    }
    compile_vulkan_program(
        renderer,
        &program,
        &[
            UniformName::new("niri_time", UniformType::_1f),
            UniformName::new("niri_cursor", UniformType::_2f),
            UniformName::new("niri_region", UniformType::_4f),
            UniformName::new("niri_output_size", UniformType::_2f),
        ],
        &[
            "niri_screen",
            "niri_prev",
            "niri_screen_prev",
            "niri_buffer",
            "niri_source",
        ],
    )
}

#[cfg(feature = "vulkan")]
fn compile_vulkan_decoration_program(
    renderer: &mut VulkanRenderer,
    source: Option<&str>,
) -> anyhow::Result<VulkanPixelProgram> {
    let mut program = if source.is_some() {
        "#define CUSTOM_DECORATION\n".to_owned()
    } else {
        "#define WAX_STRENGTH rainbow_ripple.y\n#define WAX_BRIGHTNESS rainbow_ripple.z\n#define WAX_PHASE rainbow_ripple.x\n".to_owned()
    };
    program.push_str(include_str!("border.frag"));
    program.push_str(include_str!("rounding_alpha.frag"));
    program.push_str(source.unwrap_or(include_str!(
        "../../../resources/shaders/focus-ring/rainbow-ripple.frag"
    )));
    compile_vulkan_program(
        renderer,
        &program,
        &[
            UniformName::new("colorspace", UniformType::_1f),
            UniformName::new("hue_interpolation", UniformType::_1f),
            UniformName::new("color_from", UniformType::_4f),
            UniformName::new("color_to", UniformType::_4f),
            UniformName::new("grad_offset", UniformType::_2f),
            UniformName::new("grad_width", UniformType::_1f),
            UniformName::new("grad_vec", UniformType::_2f),
            UniformName::new("input_to_geo", UniformType::Matrix3x3),
            UniformName::new("geo_size", UniformType::_2f),
            UniformName::new("outer_radius", UniformType::_4f),
            UniformName::new("border_width", UniformType::_1f),
            UniformName::new("rainbow_ripple", UniformType::_4f),
            UniformName::new("ring_width", UniformType::_1f),
            UniformName::new("emission_threshold", UniformType::_1f),
            UniformName::new("niri_time", UniformType::_1f),
        ],
        &[],
    )
}

#[cfg(feature = "vulkan")]
pub fn get_vulkan_frame<'a>(
    frame: &'a smithay::backend::renderer::vulkan::VulkanFrame<'_, '_>,
) -> Option<&'a VulkanShaders> {
    frame.user_data().get()
}

#[cfg(feature = "vulkan")]
pub fn get_vulkan_renderer(renderer: &VulkanRenderer) -> Option<&VulkanShaders> {
    renderer.user_data().get()
}

/// Maps a GLES uniform declaration to a custom Vulkan uniform declaration.
#[cfg(feature = "vulkan")]
fn uniform_name_to_decl(uniform: &UniformName<'_>) -> Option<CustomUniformDecl> {
    let kind = match uniform.type_ {
        UniformType::_1f => CustomUniformKind::Float,
        UniformType::_2f => CustomUniformKind::Vec2,
        UniformType::_3f => CustomUniformKind::Vec3,
        UniformType::_4f => CustomUniformKind::Vec4,
        UniformType::Matrix3x3 => CustomUniformKind::Mat3,
        _ => return None,
    };
    Some(CustomUniformDecl {
        name: uniform.name.clone().into_owned(),
        kind,
    })
}

/// Transforms a GLES-dialect niri fragment shader into Vulkan GLSL.
#[cfg(feature = "vulkan")]
fn vulkanize_fragment(src: &str, decls: &[CustomUniformDecl], textures: &[&str]) -> String {
    let mut out = String::from(
        "#version 450
         #define DEBUG_FLAGS
         #define texture2D texture
",
    );
    out.push_str(&texture_bindings_glsl(textures));
    out.push_str(&uniform_block_glsl(decls));
    out.push_str(
        "layout(location = 0) in vec2 niri_v_coords;
         layout(location = 0) out vec4 niri_frag_color;
         #define gl_FragColor niri_frag_color
         layout(push_constant) uniform NiriPush {
             vec4 niri_pc0; vec4 niri_pc1; vec4 niri_pc2; vec4 niri_pc3; vec4 niri_pc4; vec4 niri_pc5;
         };
         #define niri_alpha niri_pc2.z
         #define niri_tint niri_pc2.w
         #define v_coords niri_v_coords
",
    );
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#version")
            || trimmed.starts_with("#extension")
            || trimmed.starts_with("precision ")
            || trimmed.starts_with("varying ")
            || trimmed.starts_with("uniform ")
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Compiles a niri shader program for the Vulkan renderer from its GLES-dialect
/// source. Must be given the same uniforms as the GLES compile, including the
/// six blend-space ones that `hdr.frag` references.
#[cfg(feature = "vulkan")]
pub(super) fn compile_vulkan_program(
    renderer: &mut VulkanRenderer,
    src: &str,
    uniforms: &[UniformName<'_>],
    texture_uniforms: &[&str],
) -> anyhow::Result<VulkanPixelProgram> {
    let mut uniforms = uniforms.to_vec();
    uniforms.push(UniformName::new("niri_hdr_pq", UniformType::_1f));
    uniforms.push(UniformName::new("niri_ref_lum_scale", UniformType::_1f));
    uniforms.push(UniformName::new("niri_linear", UniformType::_1f));
    uniforms.push(UniformName::new("niri_linear_scale", UniformType::_1f));
    uniforms.push(UniformName::new("niri_linear_to_ref", UniformType::_1f));
    uniforms.push(UniformName::new("niri_hdr_to_sdr", UniformType::_1f));

    let mut decls: Vec<CustomUniformDecl> =
        uniforms.iter().filter_map(uniform_name_to_decl).collect();
    for extra in ["niri_size", "niri_scale"] {
        if !decls.iter().any(|d| d.name == extra) {
            decls.push(CustomUniformDecl {
                name: extra.to_owned(),
                kind: if extra == "niri_scale" {
                    CustomUniformKind::Float
                } else {
                    CustomUniformKind::Vec2
                },
            });
        }
    }

    let mut full = src.to_string();
    full.push_str(include_str!("hdr.frag"));
    let vulkan_src = vulkanize_fragment(&full, &decls, texture_uniforms);
    renderer
        .compile_custom_pixel_shader(&vulkan_src, &decls, texture_uniforms)
        .map_err(|err| {
            if std::env::var_os("NIRI_DUMP_SHADERS").is_some() {
                eprintln!("vulkan shader error: {err}");
                for (i, line) in vulkan_src.lines().enumerate() {
                    eprintln!("{:4} {line}", i + 1);
                }
            }
            anyhow::anyhow!("error compiling vulkan shader: {err}")
        })
}

fn compile_decoration_program(
    renderer: &mut GlesRenderer,
    source: Option<&str>,
) -> Result<ShaderProgram, GlesError> {
    let mut program = if source.is_some() {
        "#define CUSTOM_DECORATION\n".to_owned()
    } else {
        "#define WAX_STRENGTH rainbow_ripple.y\n#define WAX_BRIGHTNESS rainbow_ripple.z\n#define WAX_PHASE rainbow_ripple.x\n".to_owned()
    };
    program.push_str(include_str!("border.frag"));
    program.push_str(include_str!("rounding_alpha.frag"));
    program.push_str(source.unwrap_or(include_str!(
        "../../../resources/shaders/focus-ring/rainbow-ripple.frag"
    )));
    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("colorspace", UniformType::_1f),
            UniformName::new("hue_interpolation", UniformType::_1f),
            UniformName::new("color_from", UniformType::_4f),
            UniformName::new("color_to", UniformType::_4f),
            UniformName::new("grad_offset", UniformType::_2f),
            UniformName::new("grad_width", UniformType::_1f),
            UniformName::new("grad_vec", UniformType::_2f),
            UniformName::new("input_to_geo", UniformType::Matrix3x3),
            UniformName::new("geo_size", UniformType::_2f),
            UniformName::new("outer_radius", UniformType::_4f),
            UniformName::new("border_width", UniformType::_1f),
            UniformName::new("rainbow_ripple", UniformType::_4f),
            UniformName::new("ring_width", UniformType::_1f),
            UniformName::new("emission_threshold", UniformType::_1f),
            UniformName::new("niri_time", UniformType::_1f),
        ],
        &[],
    )
}

/// Compile content-addressed programs once on load. Drop stale GPU resources on reload.
/// A bad program falls back to the normal border; other windows keep their own programs.
pub fn set_decoration_programs(renderer: &mut GlesRenderer, config: &niri_config::Config) {
    let layout_parts = config
        .outputs
        .0
        .iter()
        .filter_map(|o| o.layout.as_ref())
        .chain(
            config
                .workspaces
                .iter()
                .filter_map(|w| w.layout.as_ref().map(|l| &l.0)),
        );
    let layout_shaders = layout_parts
        .flat_map(|l| l.focus_ring.iter().chain(l.border.iter()))
        .filter_map(|r| r.shader.as_ref());
    let shaders = config
        .layout
        .focus_ring
        .shader
        .iter()
        .chain(config.layout.border.shader.iter())
        .chain(layout_shaders)
        .chain(
            config
                .window_rules
                .iter()
                .flat_map(|r| r.focus_ring.shader.iter().chain(r.border.shader.iter())),
        );
    let wanted: HashMap<_, _> = shaders.filter_map(|s| Some((s.key()?, s))).collect();
    let stale: Vec<_> = {
        let mut cache = Shaders::get(renderer).decorations.borrow_mut();
        let keys: Vec<_> = cache
            .keys()
            .copied()
            .filter(|k| !wanted.contains_key(k))
            .collect();
        keys.into_iter().filter_map(|k| cache.remove(&k)).collect()
    };
    for program in stale {
        let _ = program.destroy(renderer);
    }
    for (key, shader) in wanted {
        if Shaders::get(renderer)
            .decorations
            .borrow()
            .contains_key(&key)
        {
            continue;
        }
        match compile_decoration_program(renderer, shader.source()) {
            Ok(program) => {
                Shaders::get(renderer)
                    .decorations
                    .borrow_mut()
                    .insert(key, program);
            }
            Err(err) => {
                warn!(path = ?shader.path, "error compiling decoration shader; using configured colours: {err:?}")
            }
        }
    }
}

#[cfg(all(test, feature = "vulkan"))]
mod tests {
    use smithay::backend::vulkan::version::Version;
    use smithay::backend::vulkan::{Instance, PhysicalDevice};

    use super::*;

    /// Compiles the border and shadow GLSL sources through the Vulkan custom
    /// pixel pipeline. Skips silently on machines without a usable Vulkan
    /// device; on real hardware this catches GLSL→SPIR-V regressions without
    /// needing a compositor.
    #[test]
    fn vulkan_offscreen_capture_and_custom_draw() {
        use smithay::backend::renderer::vulkan::{
            CustomUniform, CustomUniformValue, VulkanRenderer,
        };
        use smithay::backend::renderer::{Bind, ExportMem, Frame, Offscreen, Renderer};
        use smithay::utils::{Rectangle, Size, Transform};

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

            // Compile a trivial red-fill pixel program (the shape of the
            // global/scoped passes, without textures).
            let program = compile_vulkan_program(
                &mut renderer,
                "void main() { gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }",
                &[],
                &[],
            )
            .expect("custom program");

            let buffer_size = Size::<i32, smithay::utils::Buffer>::from((8, 8));
            let output_size = Size::<i32, smithay::utils::Physical>::from((8, 8));
            let mut target = renderer
                .create_buffer(smithay::backend::allocator::Fourcc::Argb8888, buffer_size)
                .expect("offscreen buffer");
            let mut framebuffer = renderer.bind(&mut target).expect("bind");

            let mut frame = renderer
                .render(&mut framebuffer, output_size, Transform::Normal)
                .expect("begin frame");
            let dst = Rectangle::from_size(output_size);
            let damage = [dst];
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
            frame
                .render_custom(&program, dst, &damage, &uniforms, &[], 1.0)
                .expect("custom draw");
            frame.finish().expect("finish");

            let mapping = renderer
                .copy_framebuffer(
                    &framebuffer,
                    Rectangle::from_size(buffer_size),
                    smithay::backend::allocator::Fourcc::Argb8888,
                )
                .expect("copy framebuffer");
            let pixels = renderer.map_texture(&mapping).expect("map");
            // Argb8888 little-endian: B, G, R, A.
            assert_eq!(&pixels[0..4], &[0, 0, 255, 255], "unexpected pixel");
            eprintln!(
                "Vulkan offscreen capture + custom draw OK on {}",
                device.name()
            );
            return;
        }
    }

    #[test]
    fn vulkan_border_and_shadow_shaders_compile() {
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
            init_vulkan(&mut renderer, &niri_config::Config::default());
            let shaders = renderer.user_data().get::<VulkanShaders>().unwrap();
            assert!(
                shaders.border.is_some(),
                "border shader failed to compile on {}",
                device.name()
            );
            assert!(
                shaders.shadow.is_some(),
                "shadow shader failed to compile on {}",
                device.name()
            );
            assert!(
                shaders.clipped_surface.is_some(),
                "clipped-surface shader failed to compile on {}",
                device.name()
            );
            assert!(
                shaders.panel.is_some(),
                "panel shader failed to compile on {}",
                device.name()
            );
            assert!(
                shaders.decoration_light.is_some(),
                "decoration-light shader failed to compile on {}",
                device.name()
            );
            assert!(
                shaders.blur_down.is_some() && shaders.blur_up.is_some(),
                "blur shaders failed to compile on {}",
                device.name()
            );
            assert!(
                shaders.postprocess_and_clip.is_some(),
                "background-effect postprocess shader failed to compile on {}",
                device.name()
            );
            // Global/scoped programs need five samplers; better-smithay now
            // exposes MAX_CUSTOM_TEXTURES = 8, so they compile for real.
            assert!(
                compile_vulkan_global(
                    &mut renderer,
                    "vec4 global_color(vec3 coord) { return vec4(coord, 1.0); }",
                    false
                )
                .is_ok(),
                "global shader failed to compile on {}",
                device.name()
            );
            eprintln!(
                "compiled Vulkan border, shadow and clipped-surface shaders on {}",
                device.name()
            );
            return;
        }
    }
}
