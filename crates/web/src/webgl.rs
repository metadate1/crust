use js_sys::Float32Array;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use web_sys::{HtmlCanvasElement, WebGl2RenderingContext, WebGlBuffer, WebGlProgram, WebGlShader};

const VERTEX_SHADER: &str = r#"#version 300 es
precision highp float;
in vec2 a_position;
in vec3 a_color;
out vec3 v_color;
void main() {
  v_color = a_color;
  gl_Position = vec4(a_position, 0.0, 1.0);
}
"#;

const FRAGMENT_SHADER: &str = r#"#version 300 es
precision highp float;
in vec3 v_color;
out vec4 out_color;
void main() {
  out_color = vec4(v_color, 1.0);
}
"#;

#[derive(Clone, Copy, Debug)]
pub struct VisualState {
    pub time: f32,
    pub seed: u32,
    pub player_x: f32,
    pub player_y: f32,
    pub active: bool,
}

#[derive(Debug)]
pub struct GlStage {
    gl: WebGl2RenderingContext,
    program: WebGlProgram,
    buffer: WebGlBuffer,
    position_location: u32,
    color_location: u32,
}

impl GlStage {
    pub fn new(canvas: &HtmlCanvasElement) -> Result<Self, JsValue> {
        let gl = canvas
            .get_context("webgl2")?
            .ok_or_else(|| JsValue::from_str("WebGL2 is unavailable"))?
            .dyn_into::<WebGl2RenderingContext>()?;
        let vertex = compile_shader(&gl, WebGl2RenderingContext::VERTEX_SHADER, VERTEX_SHADER)?;
        let fragment = compile_shader(
            &gl,
            WebGl2RenderingContext::FRAGMENT_SHADER,
            FRAGMENT_SHADER,
        )?;
        let program = link_program(&gl, &vertex, &fragment)?;
        let buffer = gl
            .create_buffer()
            .ok_or_else(|| JsValue::from_str("WebGL2 could not allocate a vertex buffer"))?;
        let position_location = gl.get_attrib_location(&program, "a_position");
        let color_location = gl.get_attrib_location(&program, "a_color");
        if position_location < 0 || color_location < 0 {
            return Err(JsValue::from_str("WebGL2 shader attributes are missing"));
        }
        gl.use_program(Some(&program));
        gl.bind_buffer(WebGl2RenderingContext::ARRAY_BUFFER, Some(&buffer));
        gl.enable_vertex_attrib_array(position_location as u32);
        gl.vertex_attrib_pointer_with_i32(
            position_location as u32,
            2,
            WebGl2RenderingContext::FLOAT,
            false,
            5 * size_of::<f32>() as i32,
            0,
        );
        gl.enable_vertex_attrib_array(color_location as u32);
        gl.vertex_attrib_pointer_with_i32(
            color_location as u32,
            3,
            WebGl2RenderingContext::FLOAT,
            false,
            5 * size_of::<f32>() as i32,
            2 * size_of::<f32>() as i32,
        );
        Ok(Self {
            gl,
            program,
            buffer,
            position_location: position_location as u32,
            color_location: color_location as u32,
        })
    }

    pub fn render(&self, state: VisualState) {
        let gl = &self.gl;
        gl.viewport(0, 0, gl.drawing_buffer_width(), gl.drawing_buffer_height());
        gl.use_program(Some(&self.program));
        gl.bind_buffer(WebGl2RenderingContext::ARRAY_BUFFER, Some(&self.buffer));
        gl.enable_vertex_attrib_array(self.position_location);
        gl.enable_vertex_attrib_array(self.color_location);
        let vertices = scene_vertices(state);
        let array = Float32Array::from(vertices.as_slice());
        gl.buffer_data_with_array_buffer_view(
            WebGl2RenderingContext::ARRAY_BUFFER,
            &array,
            WebGl2RenderingContext::DYNAMIC_DRAW,
        );
        gl.draw_arrays(
            WebGl2RenderingContext::TRIANGLES,
            0,
            i32::try_from(vertices.len() / 5).unwrap_or(i32::MAX),
        );
    }

    #[must_use]
    pub fn error(&self) -> u32 {
        self.gl.get_error()
    }
}

fn compile_shader(
    gl: &WebGl2RenderingContext,
    kind: u32,
    source: &str,
) -> Result<WebGlShader, JsValue> {
    let shader = gl
        .create_shader(kind)
        .ok_or_else(|| JsValue::from_str("WebGL2 could not create a shader"))?;
    gl.shader_source(&shader, source);
    gl.compile_shader(&shader);
    if gl
        .get_shader_parameter(&shader, WebGl2RenderingContext::COMPILE_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        Ok(shader)
    } else {
        Err(JsValue::from_str(
            &gl.get_shader_info_log(&shader)
                .unwrap_or_else(|| "unknown shader compile error".to_owned()),
        ))
    }
}

fn link_program(
    gl: &WebGl2RenderingContext,
    vertex: &WebGlShader,
    fragment: &WebGlShader,
) -> Result<WebGlProgram, JsValue> {
    let program = gl
        .create_program()
        .ok_or_else(|| JsValue::from_str("WebGL2 could not create a program"))?;
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
        Err(JsValue::from_str(
            &gl.get_program_info_log(&program)
                .unwrap_or_else(|| "unknown program link error".to_owned()),
        ))
    }
}

fn scene_vertices(state: VisualState) -> Vec<f32> {
    let hue = ((state.seed.rotate_left(7) & 0xff) as f32) / 255.0;
    let pulse = (state.time * 0.45).sin() * 0.025;
    let sky = [0.025 + hue * 0.035, 0.07 + pulse, 0.085 + hue * 0.045];
    let horizon = [0.07 + hue * 0.08, 0.13 + pulse, 0.14];
    let ground = [0.025, 0.045, 0.04 + hue * 0.04];
    let amber = [0.95, 0.48 + hue * 0.18, 0.13];
    let cyan = [0.17, 0.72 + pulse, 0.63];
    let mut output = Vec::with_capacity(5 * 45);

    quad(&mut output, -1.0, -1.0, 1.0, 1.0, sky, horizon);
    // Low-poly distant silhouette, intentionally original and data-independent.
    triangle(
        &mut output,
        [-1.0, -0.32],
        [-0.56, 0.18],
        [-0.16, -0.32],
        ground,
    );
    triangle(
        &mut output,
        [-0.34, -0.32],
        [0.08, 0.31],
        [0.52, -0.32],
        ground,
    );
    triangle(
        &mut output,
        [0.32, -0.32],
        [0.78, 0.12],
        [1.0, -0.32],
        ground,
    );
    quad(&mut output, -1.0, -1.0, 1.0, -0.31, ground, ground);

    if state.active {
        let x = state.player_x.clamp(-0.88, 0.88);
        let y = state.player_y.clamp(-0.78, 0.56);
        let bob = (state.time * 5.0).sin() * 0.012;
        triangle(
            &mut output,
            [x - 0.055, y - 0.08 + bob],
            [x, y + 0.07 + bob],
            [x + 0.055, y - 0.08 + bob],
            amber,
        );
        quad(&mut output, x - 0.09, -0.83, x + 0.09, -0.80, cyan, cyan);
    }
    output
}

fn push_vertex(output: &mut Vec<f32>, point: [f32; 2], color: [f32; 3]) {
    output.extend([point[0], point[1], color[0], color[1], color[2]]);
}

fn triangle(output: &mut Vec<f32>, a: [f32; 2], b: [f32; 2], c: [f32; 2], color: [f32; 3]) {
    push_vertex(output, a, color);
    push_vertex(output, b, color);
    push_vertex(output, c, color);
}

fn quad(
    output: &mut Vec<f32>,
    left: f32,
    bottom: f32,
    right: f32,
    top: f32,
    bottom_color: [f32; 3],
    top_color: [f32; 3],
) {
    push_vertex(output, [left, bottom], bottom_color);
    push_vertex(output, [right, bottom], bottom_color);
    push_vertex(output, [left, top], top_color);
    push_vertex(output, [left, top], top_color);
    push_vertex(output, [right, bottom], bottom_color);
    push_vertex(output, [right, top], top_color);
}
