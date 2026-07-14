//! Production WebGL2 backend for [`crust_renderer`] command frames.
//!
//! The engine-facing renderer owns no browser resources. This module copies
//! its pointer-free vertices into a WebGL buffer and owns the small mapping
//! from stable [`TextureHandle`] values to browser textures.

#![allow(
    dead_code,
    reason = "the live stage uses the backend core; cache lifecycle and detailed diagnostics remain available for later retail scene integration"
)]

use core::fmt;
use core::ops::Range;
use std::collections::{BTreeMap, BTreeSet};

use crust_renderer::cache::{CachedTexture, TextureHandle};
use crust_renderer::command::{
    AlphaTest, BlendEquation, BlendFactor, BlendMode, CommandDiagnostics, DrawBatch,
    GeneratedFrame, GpuVertex, PrimitiveStyle, RenderPass, render_passes,
};
use crust_renderer::texture::DecodedTexture;
use js_sys::Float32Array;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use web_sys::{
    HtmlCanvasElement, WebGl2RenderingContext, WebGlBuffer, WebGlProgram, WebGlShader,
    WebGlTexture, WebGlUniformLocation,
};

const FLOATS_PER_VERTEX: usize = 9;
const BYTES_PER_FLOAT: usize = size_of::<f32>();
const VERTEX_STRIDE_BYTES: i32 = 36;
const POSITION_OFFSET_BYTES: i32 = 0;
const COLOR_OFFSET_BYTES: i32 = 12;
const UV_OFFSET_BYTES: i32 = 28;
const MAX_REPORTED_GL_ERRORS: usize = 32;

const VERTEX_SHADER: &str = r"#version 300 es
precision highp float;

in vec3 a_position;
in vec4 a_color;
in vec2 a_uv;

out vec4 v_color;
out vec2 v_uv;

void main() {
  v_color = a_color;
  v_uv = a_uv;
  gl_Position = vec4(a_position, 1.0);
}
";

const FRAGMENT_SHADER: &str = r"#version 300 es
precision highp float;

uniform sampler2D u_texture;
uniform int u_textured;
uniform int u_alpha_test;
uniform float u_alpha_scale;

in vec4 v_color;
in vec2 v_uv;
out vec4 out_color;

void main() {
  vec4 color;
  if (u_textured != 0) {
    vec4 texel = texture(u_texture, v_uv);
    color = vec4(min(v_color.rgb * texel.rgb * 2.0, vec3(1.0)),
                 texel.a);
  } else {
    color = vec4(v_color.rgb, u_alpha_scale);
  }

  // WebGL2 removed fixed-function alpha tests, so reproduce the two tests
  // used by textured PSX subtractive primitives in the fragment shader.
  if (u_alpha_test == 1 && color.a <= 0.75) {
    discard;
  }
  if (u_alpha_test == 2 && color.a >= 0.75) {
    discard;
  }
  out_color = color;
}
";

/// Pixel-space rectangle for `viewport` and optional scissor state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelViewport {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl PixelViewport {
    /// Construct a viewport with nonnegative dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::InvalidViewport`] for negative dimensions.
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Result<Self, BackendError> {
        let viewport = Self {
            x,
            y,
            width,
            height,
        };
        viewport.validate()?;
        Ok(viewport)
    }

    fn validate(self) -> Result<(), BackendError> {
        if self.width < 0 || self.height < 0 {
            return Err(BackendError::InvalidViewport(self));
        }
        Ok(())
    }
}

/// Per-frame framebuffer behavior. Omitted viewport means the drawing buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RenderOptions {
    pub viewport: Option<PixelViewport>,
    pub scissor: Option<PixelViewport>,
    /// When set, clear the color buffer before drawing.
    pub clear_color: Option<[f32; 4]>,
    /// Draw a full-viewport black source-alpha pass after every scene batch.
    pub black_overlay_alpha: u8,
}

/// Per-frame WebGL work and error counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendFrameDiagnostics {
    pub frame_index: u64,
    pub renderer: CommandDiagnostics,
    pub submitted_batches: u64,
    pub drawn_batches: u64,
    pub skipped_missing_texture_batches: u64,
    pub unique_triangles_drawn: u64,
    /// Counts a triangle again when a blend mode requires a second pass.
    pub triangle_passes: u64,
    pub draw_calls: u64,
    pub vertices_uploaded: u64,
    pub vertex_upload_bytes: u64,
    pub missing_textures: Vec<TextureHandle>,
    pub black_overlay_drawn: bool,
    pub preexisting_gl_errors: Vec<u32>,
    pub gl_errors: Vec<u32>,
}

/// Lifetime backend metrics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendTotals {
    pub frames: u64,
    pub batches_drawn: u64,
    pub batches_skipped_missing_texture: u64,
    pub unique_triangles_drawn: u64,
    pub triangle_passes: u64,
    pub draw_calls: u64,
    pub vertices_uploaded: u64,
    pub vertex_upload_bytes: u64,
    pub texture_uploads: u64,
    pub texture_replacements: u64,
    pub texture_removals: u64,
    pub resident_texture_bytes: u64,
    pub gl_errors: u64,
}

#[derive(Debug)]
struct ShaderInputs {
    position: u32,
    color: u32,
    uv: u32,
    texture: WebGlUniformLocation,
    textured: WebGlUniformLocation,
    alpha_test: WebGlUniformLocation,
    alpha_scale: WebGlUniformLocation,
}

#[derive(Debug)]
struct TextureRecord {
    texture: WebGlTexture,
    width: u32,
    height: u32,
    bytes: usize,
    last_used_frame: u64,
}

/// Stateful WebGL2 consumer for renderer-generated frames.
#[derive(Debug)]
pub struct RendererBackend {
    gl: WebGl2RenderingContext,
    program: WebGlProgram,
    vertex_buffer: WebGlBuffer,
    inputs: ShaderInputs,
    textures: BTreeMap<TextureHandle, TextureRecord>,
    totals: BackendTotals,
    last_frame: Option<BackendFrameDiagnostics>,
}

impl RendererBackend {
    /// Create a WebGL2 context, compile/link shaders, and allocate the vertex buffer.
    ///
    /// # Errors
    ///
    /// Returns an error if WebGL2 is unavailable, browser context creation
    /// throws, shader compilation/linking fails, or required resources and
    /// shader inputs are unavailable.
    pub fn new(canvas: &HtmlCanvasElement) -> Result<Self, BackendError> {
        let context = canvas
            .get_context("webgl2")
            .map_err(|error| BackendError::from_js("create WebGL2 context", &error))?
            .ok_or(BackendError::WebGl2Unavailable)?;
        let gl = context
            .dyn_into::<WebGl2RenderingContext>()
            .map_err(|error| BackendError::from_js("cast WebGL2 context", &error))?;

        let vertex = compile_shader(
            &gl,
            ShaderStage::Vertex,
            WebGl2RenderingContext::VERTEX_SHADER,
            VERTEX_SHADER,
        )?;
        let fragment = match compile_shader(
            &gl,
            ShaderStage::Fragment,
            WebGl2RenderingContext::FRAGMENT_SHADER,
            FRAGMENT_SHADER,
        ) {
            Ok(fragment) => fragment,
            Err(error) => {
                gl.delete_shader(Some(&vertex));
                return Err(error);
            }
        };
        let program_result = link_program(&gl, &vertex, &fragment);
        gl.delete_shader(Some(&vertex));
        gl.delete_shader(Some(&fragment));
        let program = program_result?;

        let inputs = match shader_inputs(&gl, &program) {
            Ok(inputs) => inputs,
            Err(error) => {
                gl.delete_program(Some(&program));
                return Err(error);
            }
        };
        let Some(vertex_buffer) = gl.create_buffer() else {
            gl.delete_program(Some(&program));
            return Err(BackendError::ResourceUnavailable("vertex buffer"));
        };

        gl.use_program(Some(&program));
        gl.uniform1i(Some(&inputs.texture), 0);
        gl.disable(WebGl2RenderingContext::DEPTH_TEST);
        gl.disable(WebGl2RenderingContext::CULL_FACE);
        gl.line_width(1.0);
        configure_vertex_attributes(&gl, &vertex_buffer, &inputs);

        Ok(Self {
            gl,
            program,
            vertex_buffer,
            inputs,
            textures: BTreeMap::new(),
            totals: BackendTotals::default(),
            last_frame: None,
        })
    }

    /// Upload or atomically replace one stable renderer texture handle.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid dimensions/data length, texture allocation
    /// failure, a JavaScript exception during upload, or a reported WebGL error.
    pub fn upload_texture(
        &mut self,
        handle: TextureHandle,
        decoded: &DecodedTexture,
    ) -> Result<(), BackendError> {
        let record = self.prepare_texture(decoded)?;
        self.commit_texture(handle, record);
        Ok(())
    }

    /// Prepare every texture before replacing any resident handle.
    ///
    /// This keeps the currently presented command set valid if allocation or
    /// upload of any replacement fails.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate handles or any error documented by
    /// [`Self::upload_texture`].
    pub fn upload_textures_atomically<'a>(
        &mut self,
        textures: impl IntoIterator<Item = (TextureHandle, &'a DecodedTexture)>,
    ) -> Result<(), BackendError> {
        let textures = textures.into_iter().collect::<Vec<_>>();
        let mut handles = BTreeSet::new();
        for (handle, _) in &textures {
            if !handles.insert(*handle) {
                return Err(BackendError::DuplicateTextureHandle(*handle));
            }
        }

        let mut prepared = Vec::with_capacity(textures.len());
        for (handle, decoded) in textures {
            match self.prepare_texture(decoded) {
                Ok(record) => prepared.push((handle, record)),
                Err(error) => {
                    for (_, record) in prepared {
                        self.gl.delete_texture(Some(&record.texture));
                    }
                    return Err(error);
                }
            }
        }
        for (handle, record) in prepared {
            self.commit_texture(handle, record);
        }
        Ok(())
    }

    fn prepare_texture(&mut self, decoded: &DecodedTexture) -> Result<TextureRecord, BackendError> {
        let width = decoded.width();
        let height = decoded.height();
        let width_i32 = i32::try_from(width)
            .map_err(|_| BackendError::InvalidTextureDimensions { width, height })?;
        let height_i32 = i32::try_from(height)
            .map_err(|_| BackendError::InvalidTextureDimensions { width, height })?;
        if width == 0 || height == 0 {
            return Err(BackendError::InvalidTextureDimensions { width, height });
        }
        let expected = rgba_byte_len(width, height)
            .ok_or(BackendError::InvalidTextureDimensions { width, height })?;
        if decoded.rgba().len() != expected {
            return Err(BackendError::InvalidTextureLength {
                expected,
                actual: decoded.rgba().len(),
            });
        }
        let texture = self
            .gl
            .create_texture()
            .ok_or(BackendError::ResourceUnavailable("texture"))?;

        // Clear stale errors so a failure is attributable to this upload.
        let _ = drain_gl_errors(&self.gl);
        self.gl.active_texture(WebGl2RenderingContext::TEXTURE0);
        self.gl
            .bind_texture(WebGl2RenderingContext::TEXTURE_2D, Some(&texture));
        self.gl
            .pixel_storei(WebGl2RenderingContext::UNPACK_ALIGNMENT, 1);
        self.gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_MIN_FILTER,
            gl_i32(WebGl2RenderingContext::NEAREST),
        );
        self.gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_MAG_FILTER,
            gl_i32(WebGl2RenderingContext::NEAREST),
        );
        self.gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_WRAP_S,
            gl_i32(WebGl2RenderingContext::CLAMP_TO_EDGE),
        );
        self.gl.tex_parameteri(
            WebGl2RenderingContext::TEXTURE_2D,
            WebGl2RenderingContext::TEXTURE_WRAP_T,
            gl_i32(WebGl2RenderingContext::CLAMP_TO_EDGE),
        );
        let upload = self
            .gl
            .tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_u8_array(
                WebGl2RenderingContext::TEXTURE_2D,
                0,
                gl_i32(WebGl2RenderingContext::RGBA8),
                width_i32,
                height_i32,
                0,
                WebGl2RenderingContext::RGBA,
                WebGl2RenderingContext::UNSIGNED_BYTE,
                Some(decoded.rgba()),
            );
        self.gl
            .bind_texture(WebGl2RenderingContext::TEXTURE_2D, None);
        if let Err(error) = upload {
            self.gl.delete_texture(Some(&texture));
            return Err(BackendError::from_js("upload texture", &error));
        }
        let errors = drain_gl_errors(&self.gl);
        if !errors.is_empty() {
            self.gl.delete_texture(Some(&texture));
            add_count(&mut self.totals.gl_errors, errors.len());
            return Err(BackendError::WebGlErrors {
                operation: "upload texture",
                codes: errors,
            });
        }

        Ok(TextureRecord {
            texture,
            width,
            height,
            bytes: expected,
            last_used_frame: self.totals.frames,
        })
    }

    fn commit_texture(&mut self, handle: TextureHandle, record: TextureRecord) {
        let expected = record.bytes;
        let replacement = self.textures.insert(handle, record);
        increment(&mut self.totals.texture_uploads);
        self.totals.resident_texture_bytes = self
            .totals
            .resident_texture_bytes
            .saturating_add(u64::try_from(expected).unwrap_or(u64::MAX));
        if let Some(old) = replacement {
            self.gl.delete_texture(Some(&old.texture));
            increment(&mut self.totals.texture_replacements);
            self.totals.resident_texture_bytes = self
                .totals
                .resident_texture_bytes
                .saturating_sub(u64::try_from(old.bytes).unwrap_or(u64::MAX));
        }
    }

    /// Upload a decoded texture directly from a cache lease.
    ///
    /// # Errors
    ///
    /// Propagates upload errors from [`Self::upload_texture`].
    pub fn upload_cached_texture(&mut self, cached: &CachedTexture) -> Result<(), BackendError> {
        self.upload_texture(cached.handle, &cached.pixels)
    }

    /// Remove one GPU texture. Returns whether a handle was present.
    pub fn remove_texture(&mut self, handle: TextureHandle) -> bool {
        let Some(record) = self.textures.remove(&handle) else {
            return false;
        };
        self.gl.delete_texture(Some(&record.texture));
        increment(&mut self.totals.texture_removals);
        self.totals.resident_texture_bytes = self
            .totals
            .resident_texture_bytes
            .saturating_sub(u64::try_from(record.bytes).unwrap_or(u64::MAX));
        true
    }

    /// Remove every texture not included in `handles`.
    pub fn retain_textures(&mut self, handles: impl IntoIterator<Item = TextureHandle>) {
        let retained: BTreeSet<_> = handles.into_iter().collect();
        let removed: Vec<_> = self
            .textures
            .keys()
            .filter(|handle| !retained.contains(handle))
            .copied()
            .collect();
        for handle in removed {
            let _ = self.remove_texture(handle);
        }
    }

    pub fn clear_textures(&mut self) {
        let handles: Vec<_> = self.textures.keys().copied().collect();
        for handle in handles {
            let _ = self.remove_texture(handle);
        }
    }

    #[must_use]
    pub fn has_texture(&self, handle: TextureHandle) -> bool {
        self.textures.contains_key(&handle)
    }

    #[must_use]
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    #[must_use]
    pub fn texture_dimensions(&self, handle: TextureHandle) -> Option<[u32; 2]> {
        self.textures
            .get(&handle)
            .map(|record| [record.width, record.height])
    }

    /// Upload and draw a complete renderer frame in ordering-table order.
    ///
    /// Missing texture handles skip only their affected batches and are
    /// reported in the returned diagnostics; untextured batches still render.
    ///
    /// # Errors
    ///
    /// Returns an error if frame batches/vertices violate renderer invariants,
    /// a viewport is invalid, or a vertex/draw range cannot fit WebGL's signed
    /// argument widths.
    pub fn render(
        &mut self,
        frame: &GeneratedFrame,
        options: RenderOptions,
    ) -> Result<BackendFrameDiagnostics, BackendError> {
        validate_frame(frame)?;
        let viewport = options.viewport.unwrap_or(PixelViewport {
            x: 0,
            y: 0,
            width: self.gl.drawing_buffer_width(),
            height: self.gl.drawing_buffer_height(),
        });
        viewport.validate()?;
        if let Some(scissor) = options.scissor {
            scissor.validate()?;
        }

        let flattened = flatten_vertices(&frame.vertices);
        let uploaded_bytes = flattened
            .len()
            .checked_mul(BYTES_PER_FLOAT)
            .ok_or(BackendError::VertexDataTooLarge)?;
        let vertices = Float32Array::from(flattened.as_slice());

        let frame_index = self.totals.frames.saturating_add(1);
        let mut diagnostics = BackendFrameDiagnostics {
            frame_index,
            renderer: frame.diagnostics.clone(),
            submitted_batches: u64::try_from(frame.batches.len()).unwrap_or(u64::MAX),
            vertices_uploaded: u64::try_from(frame.vertices.len()).unwrap_or(u64::MAX),
            vertex_upload_bytes: u64::try_from(uploaded_bytes).unwrap_or(u64::MAX),
            preexisting_gl_errors: drain_gl_errors(&self.gl),
            ..BackendFrameDiagnostics::default()
        };

        self.gl
            .viewport(viewport.x, viewport.y, viewport.width, viewport.height);
        if let Some(scissor) = options.scissor {
            self.gl.enable(WebGl2RenderingContext::SCISSOR_TEST);
            self.gl
                .scissor(scissor.x, scissor.y, scissor.width, scissor.height);
        } else {
            self.gl.disable(WebGl2RenderingContext::SCISSOR_TEST);
        }
        if let Some(color) = options.clear_color {
            self.gl.clear_color(color[0], color[1], color[2], color[3]);
            self.gl.clear(WebGl2RenderingContext::COLOR_BUFFER_BIT);
        }

        self.gl.use_program(Some(&self.program));
        configure_vertex_attributes(&self.gl, &self.vertex_buffer, &self.inputs);
        self.gl.buffer_data_with_array_buffer_view(
            WebGl2RenderingContext::ARRAY_BUFFER,
            &vertices,
            WebGl2RenderingContext::DYNAMIC_DRAW,
        );
        self.gl.active_texture(WebGl2RenderingContext::TEXTURE0);
        self.gl.uniform1i(Some(&self.inputs.texture), 0);

        let mut missing = BTreeSet::new();
        for batch in &frame.batches {
            let textured = batch.pipeline.texture.is_some();
            let texture = if let Some(handle) = batch.pipeline.texture {
                let Some(record) = self.textures.get_mut(&handle) else {
                    missing.insert(handle);
                    increment(&mut diagnostics.skipped_missing_texture_batches);
                    continue;
                };
                record.last_used_frame = frame_index;
                Some(record.texture.clone())
            } else {
                None
            };

            self.gl
                .bind_texture(WebGl2RenderingContext::TEXTURE_2D, texture.as_ref());
            self.gl
                .uniform1i(Some(&self.inputs.textured), i32::from(textured));
            self.gl.uniform1f(
                Some(&self.inputs.alpha_scale),
                untextured_alpha_scale(textured, batch.pipeline.blend),
            );

            let triangle_count = batch.triangles.end - batch.triangles.start;
            increment(&mut diagnostics.drawn_batches);
            add_count(&mut diagnostics.unique_triangles_drawn, triangle_count);
            for pass in render_passes(textured, batch.pipeline.blend) {
                apply_pass(&self.gl, &self.inputs.alpha_test, pass);
                let calls = draw_batch(&self.gl, batch)?;
                diagnostics.draw_calls = diagnostics.draw_calls.saturating_add(calls);
                add_count(&mut diagnostics.triangle_passes, triangle_count);
            }
        }

        if options.black_overlay_alpha != 0 {
            let alpha = f32::from(options.black_overlay_alpha) / 255.0;
            let overlay_vertices = fullscreen_black_vertices();
            let overlay_flattened = flatten_vertices(&overlay_vertices);
            let overlay_bytes = overlay_flattened
                .len()
                .checked_mul(BYTES_PER_FLOAT)
                .ok_or(BackendError::VertexDataTooLarge)?;
            let overlay_buffer = Float32Array::from(overlay_flattened.as_slice());

            self.gl.buffer_data_with_array_buffer_view(
                WebGl2RenderingContext::ARRAY_BUFFER,
                &overlay_buffer,
                WebGl2RenderingContext::DYNAMIC_DRAW,
            );
            self.gl
                .bind_texture(WebGl2RenderingContext::TEXTURE_2D, None);
            self.gl.uniform1i(Some(&self.inputs.textured), 0);
            self.gl.uniform1f(Some(&self.inputs.alpha_scale), alpha);
            apply_pass(
                &self.gl,
                &self.inputs.alpha_test,
                RenderPass {
                    blend_enabled: true,
                    equation: BlendEquation::Add,
                    source_factor: BlendFactor::SourceAlpha,
                    destination_factor: BlendFactor::OneMinusSourceAlpha,
                    alpha_test: AlphaTest::Disabled,
                },
            );
            self.gl.draw_arrays(WebGl2RenderingContext::TRIANGLES, 0, 6);

            diagnostics.black_overlay_drawn = true;
            diagnostics.draw_calls = diagnostics.draw_calls.saturating_add(1);
            diagnostics.vertices_uploaded = diagnostics.vertices_uploaded.saturating_add(6);
            diagnostics.vertex_upload_bytes = diagnostics
                .vertex_upload_bytes
                .saturating_add(u64::try_from(overlay_bytes).unwrap_or(u64::MAX));
        }
        diagnostics.missing_textures = missing.into_iter().collect();

        self.gl
            .bind_texture(WebGl2RenderingContext::TEXTURE_2D, None);
        self.gl.disable(WebGl2RenderingContext::BLEND);
        self.gl.disable(WebGl2RenderingContext::SCISSOR_TEST);
        self.gl.uniform1i(Some(&self.inputs.alpha_test), 0);
        diagnostics.gl_errors = drain_gl_errors(&self.gl);

        increment(&mut self.totals.frames);
        self.totals.batches_drawn = self
            .totals
            .batches_drawn
            .saturating_add(diagnostics.drawn_batches);
        self.totals.batches_skipped_missing_texture = self
            .totals
            .batches_skipped_missing_texture
            .saturating_add(diagnostics.skipped_missing_texture_batches);
        self.totals.unique_triangles_drawn = self
            .totals
            .unique_triangles_drawn
            .saturating_add(diagnostics.unique_triangles_drawn);
        self.totals.triangle_passes = self
            .totals
            .triangle_passes
            .saturating_add(diagnostics.triangle_passes);
        self.totals.draw_calls = self
            .totals
            .draw_calls
            .saturating_add(diagnostics.draw_calls);
        self.totals.vertices_uploaded = self
            .totals
            .vertices_uploaded
            .saturating_add(diagnostics.vertices_uploaded);
        self.totals.vertex_upload_bytes = self
            .totals
            .vertex_upload_bytes
            .saturating_add(diagnostics.vertex_upload_bytes);
        add_count(
            &mut self.totals.gl_errors,
            diagnostics.preexisting_gl_errors.len(),
        );
        add_count(&mut self.totals.gl_errors, diagnostics.gl_errors.len());
        self.last_frame = Some(diagnostics.clone());
        Ok(diagnostics)
    }

    #[must_use]
    pub const fn totals(&self) -> &BackendTotals {
        &self.totals
    }

    #[must_use]
    pub const fn last_frame(&self) -> Option<&BackendFrameDiagnostics> {
        self.last_frame.as_ref()
    }

    /// Return and clear the next raw WebGL error flag.
    #[must_use]
    pub fn next_gl_error(&self) -> u32 {
        self.gl.get_error()
    }
}

impl Drop for RendererBackend {
    fn drop(&mut self) {
        for record in self.textures.values() {
            self.gl.delete_texture(Some(&record.texture));
        }
        self.gl.delete_buffer(Some(&self.vertex_buffer));
        self.gl.delete_program(Some(&self.program));
    }
}

fn shader_inputs(
    gl: &WebGl2RenderingContext,
    program: &WebGlProgram,
) -> Result<ShaderInputs, BackendError> {
    Ok(ShaderInputs {
        position: required_attribute(gl, program, "a_position")?,
        color: required_attribute(gl, program, "a_color")?,
        uv: required_attribute(gl, program, "a_uv")?,
        texture: required_uniform(gl, program, "u_texture")?,
        textured: required_uniform(gl, program, "u_textured")?,
        alpha_test: required_uniform(gl, program, "u_alpha_test")?,
        alpha_scale: required_uniform(gl, program, "u_alpha_scale")?,
    })
}

fn required_attribute(
    gl: &WebGl2RenderingContext,
    program: &WebGlProgram,
    name: &'static str,
) -> Result<u32, BackendError> {
    let location = gl.get_attrib_location(program, name);
    u32::try_from(location).map_err(|_| BackendError::MissingShaderInput(name))
}

fn required_uniform(
    gl: &WebGl2RenderingContext,
    program: &WebGlProgram,
    name: &'static str,
) -> Result<WebGlUniformLocation, BackendError> {
    gl.get_uniform_location(program, name)
        .ok_or(BackendError::MissingShaderInput(name))
}

fn configure_vertex_attributes(
    gl: &WebGl2RenderingContext,
    buffer: &WebGlBuffer,
    inputs: &ShaderInputs,
) {
    gl.bind_buffer(WebGl2RenderingContext::ARRAY_BUFFER, Some(buffer));
    gl.enable_vertex_attrib_array(inputs.position);
    gl.vertex_attrib_pointer_with_i32(
        inputs.position,
        3,
        WebGl2RenderingContext::FLOAT,
        false,
        VERTEX_STRIDE_BYTES,
        POSITION_OFFSET_BYTES,
    );
    gl.enable_vertex_attrib_array(inputs.color);
    gl.vertex_attrib_pointer_with_i32(
        inputs.color,
        4,
        WebGl2RenderingContext::FLOAT,
        false,
        VERTEX_STRIDE_BYTES,
        COLOR_OFFSET_BYTES,
    );
    gl.enable_vertex_attrib_array(inputs.uv);
    gl.vertex_attrib_pointer_with_i32(
        inputs.uv,
        2,
        WebGl2RenderingContext::FLOAT,
        false,
        VERTEX_STRIDE_BYTES,
        UV_OFFSET_BYTES,
    );
}

fn compile_shader(
    gl: &WebGl2RenderingContext,
    stage: ShaderStage,
    kind: u32,
    source: &str,
) -> Result<WebGlShader, BackendError> {
    let shader = gl
        .create_shader(kind)
        .ok_or(BackendError::ResourceUnavailable("shader"))?;
    gl.shader_source(&shader, source);
    gl.compile_shader(&shader);
    if gl
        .get_shader_parameter(&shader, WebGl2RenderingContext::COMPILE_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(shader)
    } else {
        let log = gl
            .get_shader_info_log(&shader)
            .unwrap_or_else(|| "unknown shader compile error".to_owned());
        gl.delete_shader(Some(&shader));
        Err(BackendError::ShaderCompile { stage, log })
    }
}

fn link_program(
    gl: &WebGl2RenderingContext,
    vertex: &WebGlShader,
    fragment: &WebGlShader,
) -> Result<WebGlProgram, BackendError> {
    let program = gl
        .create_program()
        .ok_or(BackendError::ResourceUnavailable("shader program"))?;
    gl.attach_shader(&program, vertex);
    gl.attach_shader(&program, fragment);
    gl.link_program(&program);
    if gl
        .get_program_parameter(&program, WebGl2RenderingContext::LINK_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(program)
    } else {
        let log = gl
            .get_program_info_log(&program)
            .unwrap_or_else(|| "unknown program link error".to_owned());
        gl.delete_program(Some(&program));
        Err(BackendError::ProgramLink { log })
    }
}

fn apply_pass(gl: &WebGl2RenderingContext, alpha_test: &WebGlUniformLocation, pass: RenderPass) {
    if pass.blend_enabled {
        gl.enable(WebGl2RenderingContext::BLEND);
    } else {
        gl.disable(WebGl2RenderingContext::BLEND);
    }
    gl.blend_equation(gl_blend_equation(pass.equation));
    gl.blend_func(
        gl_blend_factor(pass.source_factor),
        gl_blend_factor(pass.destination_factor),
    );
    gl.uniform1i(Some(alpha_test), alpha_test_code(pass.alpha_test));
}

fn draw_batch(gl: &WebGl2RenderingContext, batch: &DrawBatch) -> Result<u64, BackendError> {
    match batch.pipeline.style {
        PrimitiveStyle::Fill => {
            let (first, count) = webgl_vertex_range(&batch.triangles)?;
            gl.draw_arrays(WebGl2RenderingContext::TRIANGLES, first, count);
            Ok(1)
        }
        PrimitiveStyle::Wireframe => {
            for triangle in batch.triangles.clone() {
                let first = triangle
                    .checked_mul(3)
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or(BackendError::VertexDataTooLarge)?;
                gl.draw_arrays(WebGl2RenderingContext::LINE_LOOP, first, 3);
            }
            Ok(u64::try_from(batch.triangles.end - batch.triangles.start).unwrap_or(u64::MAX))
        }
    }
}

fn webgl_vertex_range(triangles: &Range<usize>) -> Result<(i32, i32), BackendError> {
    let first = triangles
        .start
        .checked_mul(3)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or(BackendError::VertexDataTooLarge)?;
    let count = (triangles.end - triangles.start)
        .checked_mul(3)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or(BackendError::VertexDataTooLarge)?;
    Ok((first, count))
}

fn validate_frame(frame: &GeneratedFrame) -> Result<(), BackendError> {
    let expected_vertices = frame
        .triangles
        .len()
        .checked_mul(3)
        .ok_or(BackendError::VertexDataTooLarge)?;
    if frame.vertices.len() != expected_vertices {
        return Err(BackendError::FrameInvariant(format!(
            "{} triangles require {expected_vertices} vertices, found {}",
            frame.triangles.len(),
            frame.vertices.len()
        )));
    }
    validate_batch_ranges(
        frame.triangles.len(),
        frame.batches.iter().map(|batch| &batch.triangles),
    )
}

fn validate_batch_ranges<'a>(
    triangle_count: usize,
    ranges: impl IntoIterator<Item = &'a Range<usize>>,
) -> Result<(), BackendError> {
    let mut cursor = 0;
    for range in ranges {
        if range.start != cursor || range.start >= range.end || range.end > triangle_count {
            return Err(BackendError::FrameInvariant(format!(
                "batch range {}..{} is invalid after triangle {cursor} of {triangle_count}",
                range.start, range.end
            )));
        }
        cursor = range.end;
    }
    if cursor != triangle_count {
        return Err(BackendError::FrameInvariant(format!(
            "batches cover {cursor} of {triangle_count} triangles"
        )));
    }
    Ok(())
}

fn flatten_vertices(vertices: &[GpuVertex]) -> Vec<f32> {
    let mut flattened = Vec::with_capacity(vertices.len().saturating_mul(FLOATS_PER_VERTEX));
    for vertex in vertices {
        flattened.extend(vertex.position);
        flattened.extend(vertex.color);
        flattened.extend(vertex.uv);
    }
    flattened
}

fn fullscreen_black_vertices() -> [GpuVertex; 6] {
    let vertex = |x, y| GpuVertex {
        position: [x, y, -1.0],
        color: [0.0, 0.0, 0.0, 1.0],
        uv: [0.0, 0.0],
    };
    [
        vertex(-1.0, 1.0),
        vertex(1.0, 1.0),
        vertex(1.0, -1.0),
        vertex(-1.0, -1.0),
        vertex(-1.0, 1.0),
        vertex(1.0, -1.0),
    ]
}

fn untextured_alpha_scale(textured: bool, blend: BlendMode) -> f32 {
    if textured {
        return 1.0;
    }
    match blend {
        BlendMode::Average => 127.0 / 255.0,
        BlendMode::Additive => 0.0,
        BlendMode::Subtractive | BlendMode::Opaque => 1.0,
    }
}

const fn alpha_test_code(test: AlphaTest) -> i32 {
    match test {
        AlphaTest::Disabled => 0,
        AlphaTest::GreaterThanThreeQuarters => 1,
        AlphaTest::LessThanThreeQuarters => 2,
    }
}

const fn gl_blend_equation(equation: BlendEquation) -> u32 {
    match equation {
        BlendEquation::Add => WebGl2RenderingContext::FUNC_ADD,
        BlendEquation::ReverseSubtract => WebGl2RenderingContext::FUNC_REVERSE_SUBTRACT,
    }
}

const fn gl_blend_factor(factor: BlendFactor) -> u32 {
    match factor {
        BlendFactor::Zero => WebGl2RenderingContext::ZERO,
        BlendFactor::One => WebGl2RenderingContext::ONE,
        BlendFactor::SourceAlpha => WebGl2RenderingContext::SRC_ALPHA,
        BlendFactor::OneMinusSourceAlpha => WebGl2RenderingContext::ONE_MINUS_SRC_ALPHA,
    }
}

fn rgba_byte_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

fn gl_i32(value: u32) -> i32 {
    i32::try_from(value).unwrap_or_default()
}

fn drain_gl_errors(gl: &WebGl2RenderingContext) -> Vec<u32> {
    let mut errors = Vec::new();
    while errors.len() < MAX_REPORTED_GL_ERRORS {
        let error = gl.get_error();
        if error == WebGl2RenderingContext::NO_ERROR {
            break;
        }
        errors.push(error);
    }
    errors
}

fn increment(value: &mut u64) {
    *value = value.saturating_add(1);
}

fn add_count(value: &mut u64, count: usize) {
    *value = value.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShaderStage {
    Vertex,
    Fragment,
}

impl fmt::Display for ShaderStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vertex => formatter.write_str("vertex"),
            Self::Fragment => formatter.write_str("fragment"),
        }
    }
}

/// Backend construction, upload, or frame-validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    WebGl2Unavailable,
    JavaScript {
        operation: &'static str,
        message: String,
    },
    ShaderCompile {
        stage: ShaderStage,
        log: String,
    },
    ProgramLink {
        log: String,
    },
    ResourceUnavailable(&'static str),
    MissingShaderInput(&'static str),
    InvalidViewport(PixelViewport),
    InvalidTextureDimensions {
        width: u32,
        height: u32,
    },
    InvalidTextureLength {
        expected: usize,
        actual: usize,
    },
    DuplicateTextureHandle(TextureHandle),
    VertexDataTooLarge,
    FrameInvariant(String),
    WebGlErrors {
        operation: &'static str,
        codes: Vec<u32>,
    },
}

impl BackendError {
    fn from_js(operation: &'static str, value: &JsValue) -> Self {
        Self::JavaScript {
            operation,
            message: value.as_string().unwrap_or_else(|| format!("{value:?}")),
        }
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WebGl2Unavailable => formatter.write_str("WebGL2 is unavailable"),
            Self::JavaScript { operation, message } => {
                write!(formatter, "{operation} failed: {message}")
            }
            Self::ShaderCompile { stage, log } => {
                write!(formatter, "{stage} shader compilation failed: {log}")
            }
            Self::ProgramLink { log } => write!(formatter, "shader program link failed: {log}"),
            Self::ResourceUnavailable(resource) => {
                write!(formatter, "WebGL2 could not allocate {resource}")
            }
            Self::MissingShaderInput(name) => write!(formatter, "shader input {name} is missing"),
            Self::InvalidViewport(viewport) => write!(formatter, "invalid viewport {viewport:?}"),
            Self::InvalidTextureDimensions { width, height } => {
                write!(formatter, "invalid texture dimensions {width}x{height}")
            }
            Self::InvalidTextureLength { expected, actual } => write!(
                formatter,
                "texture contains {actual} RGBA bytes; expected {expected}"
            ),
            Self::DuplicateTextureHandle(handle) => {
                write!(
                    formatter,
                    "texture batch contains duplicate handle {handle:?}"
                )
            }
            Self::VertexDataTooLarge => {
                formatter.write_str("frame vertex data exceeds WebGL2 limits")
            }
            Self::FrameInvariant(message) => write!(formatter, "invalid renderer frame: {message}"),
            Self::WebGlErrors { operation, codes } => {
                write!(formatter, "{operation} reported WebGL errors {codes:?}")
            }
        }
    }
}

impl std::error::Error for BackendError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn vertex(seed: f32) -> GpuVertex {
        GpuVertex {
            position: [seed, seed + 1.0, seed + 2.0],
            color: [seed + 3.0, seed + 4.0, seed + 5.0, seed + 6.0],
            uv: [seed + 7.0, seed + 8.0],
        }
    }

    #[test]
    fn interleaving_is_stable_and_complete() {
        let flattened = flatten_vertices(&[vertex(0.0), vertex(10.0)]);
        assert_eq!(flattened.len(), FLOATS_PER_VERTEX * 2);
        assert_eq!(
            flattened[..FLOATS_PER_VERTEX],
            [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]
        );
        assert_eq!(
            flattened[FLOATS_PER_VERTEX..],
            [10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0]
        );
    }

    #[test]
    fn black_overlay_is_two_full_viewport_triangles_and_opt_in() {
        assert_eq!(RenderOptions::default().black_overlay_alpha, 0);
        let vertices = fullscreen_black_vertices();
        assert_eq!(
            vertices.map(|vertex| vertex.position),
            [
                [-1.0, 1.0, -1.0],
                [1.0, 1.0, -1.0],
                [1.0, -1.0, -1.0],
                [-1.0, -1.0, -1.0],
                [-1.0, 1.0, -1.0],
                [1.0, -1.0, -1.0],
            ]
        );
        assert!(
            vertices
                .iter()
                .all(|vertex| vertex.color.map(f32::to_bits) == [0, 0, 0, 1.0_f32.to_bits()])
        );
    }

    #[test]
    fn contiguous_batch_layout_is_required() {
        let valid = [0..2, 2..5];
        assert!(validate_batch_ranges(5, valid.iter()).is_ok());
        let gap = [0..2, 3..5];
        assert!(matches!(
            validate_batch_ranges(5, gap.iter()),
            Err(BackendError::FrameInvariant(_))
        ));
        let overlap = [0..3, 2..5];
        assert!(matches!(
            validate_batch_ranges(5, overlap.iter()),
            Err(BackendError::FrameInvariant(_))
        ));
        let empty = 0..0;
        assert!(matches!(
            validate_batch_ranges(0, core::iter::once(&empty)),
            Err(BackendError::FrameInvariant(_))
        ));
        assert!(validate_batch_ranges(0, core::iter::empty()).is_ok());
    }

    #[test]
    fn alpha_and_pass_state_match_psx_modes() {
        assert!(
            (untextured_alpha_scale(false, BlendMode::Average) - 127.0 / 255.0).abs()
                <= f32::EPSILON
        );
        assert!(untextured_alpha_scale(false, BlendMode::Additive).abs() <= f32::EPSILON);
        assert!((untextured_alpha_scale(false, BlendMode::Opaque) - 1.0).abs() <= f32::EPSILON);
        assert!((untextured_alpha_scale(true, BlendMode::Average) - 1.0).abs() <= f32::EPSILON);

        let passes = render_passes(true, BlendMode::Subtractive);
        assert_eq!(passes.len(), 2);
        assert_eq!(alpha_test_code(passes[0].alpha_test), 1);
        assert_eq!(alpha_test_code(passes[1].alpha_test), 2);
        assert_eq!(
            gl_blend_equation(passes[1].equation),
            WebGl2RenderingContext::FUNC_REVERSE_SUBTRACT
        );
    }

    #[test]
    fn byte_lengths_and_vertex_ranges_are_checked() {
        assert_eq!(rgba_byte_len(1024, 128), Some(1024 * 128 * 4));
        assert_eq!(webgl_vertex_range(&(2..5)), Ok((6, 9)));
        assert!(rgba_byte_len(u32::MAX, u32::MAX).is_none());
    }
}
