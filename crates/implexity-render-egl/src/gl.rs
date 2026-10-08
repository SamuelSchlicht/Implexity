// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::ffi::{CStr, CString, c_char, c_void};

use crate::shaders::ShaderSources;
use crate::worker::{Readback, RenderPlan, Step, WorkerError};

type Ptr = *mut c_void;

fn runtime(m: impl Into<String>) -> WorkerError {
    WorkerError::Runtime(m.into())
}

#[cfg(unix)]
mod sys {
    use std::ffi::{c_char, c_int, c_void};

    pub(super) const RTLD_NOW: c_int = 2;

    #[cfg_attr(all(target_os = "linux", target_env = "gnu"), link(name = "dl"))]
    unsafe extern "C" {
        pub(super) fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        pub(super) fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::{c_char, c_void};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub(super) fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        pub(super) fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }
}

struct Library(Ptr);

impl Library {
    fn candidates() -> &'static [&'static CStr] {
        if cfg!(windows) {
            &[c"libEGL.dll"]
        } else if cfg!(target_os = "macos") {
            &[c"libEGL.dylib"]
        } else {
            &[c"libEGL.so.1", c"libEGL.so"]
        }
    }

    fn open() -> Result<Self, WorkerError> {
        for name in Self::candidates() {
            #[cfg(unix)]
            let handle = unsafe { sys::dlopen(name.as_ptr(), sys::RTLD_NOW) };
            #[cfg(windows)]
            let handle = unsafe { sys::LoadLibraryA(name.as_ptr()) };
            if !handle.is_null() {
                return Ok(Self(handle));
            }
        }
        Err(runtime("the EGL library is not installed (libEGL)"))
    }

    fn symbol(&self, name: &CStr) -> *const c_void {
        #[cfg(unix)]
        
        let p = unsafe { sys::dlsym(self.0, name.as_ptr()) };
        #[cfg(windows)]
        
        let p = unsafe { sys::GetProcAddress(self.0, name.as_ptr()) };
        p.cast_const()
    }
}

macro_rules! entry_points {
    ($table:ident { $($field:ident = $symbol:literal : fn($($arg:ty),*) $(-> $ret:ty)?;)* }) => {
        #[allow(non_snake_case)]
        struct $table {
            $(
                $field: unsafe extern "system" fn($($arg),*) $(-> $ret)?,
            )*
        }

        impl $table {
            fn load(resolve: &dyn Fn(&CStr) -> *const c_void) -> Result<Self, WorkerError> {
                Ok(Self {
                    $($field: {
                        let p = resolve($symbol);
                        if p.is_null() {
                            return Err(runtime(format!(
                                "missing graphics entry {}", $symbol.to_string_lossy())));
                        }
                        
                        unsafe { std::mem::transmute::<*const c_void, unsafe extern "system" fn($($arg),*) $(-> $ret)?>(p) }
                    },)*
                })
            }
        }
    };
}

entry_points!(Egl {
    get_display = c"eglGetDisplay": fn(Ptr) -> Ptr;
    initialize = c"eglInitialize": fn(Ptr, *mut i32, *mut i32) -> u32;
    bind_api = c"eglBindAPI": fn(u32) -> u32;
    choose_config = c"eglChooseConfig": fn(Ptr, *const i32, *mut Ptr, i32, *mut i32) -> u32;
    create_context = c"eglCreateContext": fn(Ptr, Ptr, Ptr, *const i32) -> Ptr;
    create_pbuffer_surface = c"eglCreatePbufferSurface": fn(Ptr, Ptr, *const i32) -> Ptr;
    make_current = c"eglMakeCurrent": fn(Ptr, Ptr, Ptr, Ptr) -> u32;
    destroy_surface = c"eglDestroySurface": fn(Ptr, Ptr) -> u32;
    destroy_context = c"eglDestroyContext": fn(Ptr, Ptr) -> u32;
    terminate = c"eglTerminate": fn(Ptr) -> u32;
    get_proc_address = c"eglGetProcAddress": fn(*const c_char) -> *const c_void;
});

entry_points!(GetString { get_string = c"glGetString": fn(u32) -> *const u8; });

entry_points!(Gl {
    create_program = c"glCreateProgram": fn() -> u32;
    create_shader = c"glCreateShader": fn(u32) -> u32;
    shader_source = c"glShaderSource": fn(u32, i32, *const *const c_char, *const i32);
    compile_shader = c"glCompileShader": fn(u32);
    get_shader_iv = c"glGetShaderiv": fn(u32, u32, *mut i32);
    get_shader_info_log = c"glGetShaderInfoLog": fn(u32, i32, *mut i32, *mut c_char);
    attach_shader = c"glAttachShader": fn(u32, u32);
    delete_shader = c"glDeleteShader": fn(u32);
    link_program = c"glLinkProgram": fn(u32);
    get_program_iv = c"glGetProgramiv": fn(u32, u32, *mut i32);
    use_program = c"glUseProgram": fn(u32);
    get_uniform_location = c"glGetUniformLocation": fn(u32, *const c_char) -> i32;
    uniform1f = c"glUniform1f": fn(i32, f32);
    uniform2f = c"glUniform2f": fn(i32, f32, f32);
    uniform3f = c"glUniform3f": fn(i32, f32, f32, f32);
    uniform1i = c"glUniform1i": fn(i32, i32);
    uniform_matrix3fv = c"glUniformMatrix3fv": fn(i32, i32, u8, *const f32);
    gen_textures = c"glGenTextures": fn(i32, *mut u32);
    active_texture = c"glActiveTexture": fn(u32);
    bind_texture = c"glBindTexture": fn(u32, u32);
    tex_parameter_i = c"glTexParameteri": fn(u32, u32, i32);
    pixel_store_i = c"glPixelStorei": fn(u32, i32);
    tex_image_3d = c"glTexImage3D": fn(u32, i32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
    tex_image_2d = c"glTexImage2D": fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
    gen_framebuffers = c"glGenFramebuffers": fn(i32, *mut u32);
    bind_framebuffer = c"glBindFramebuffer": fn(u32, u32);
    framebuffer_texture_2d = c"glFramebufferTexture2D": fn(u32, u32, u32, u32, i32);
    draw_buffers = c"glDrawBuffers": fn(i32, *const u32);
    check_framebuffer_status = c"glCheckFramebufferStatus": fn(u32) -> u32;
    gen_vertex_arrays = c"glGenVertexArrays": fn(i32, *mut u32);
    bind_vertex_array = c"glBindVertexArray": fn(u32);
    gen_buffers = c"glGenBuffers": fn(i32, *mut u32);
    bind_buffer = c"glBindBuffer": fn(u32, u32);
    buffer_data = c"glBufferData": fn(u32, isize, *const c_void, u32);
    get_attrib_location = c"glGetAttribLocation": fn(u32, *const c_char) -> i32;
    enable_vertex_attrib_array = c"glEnableVertexAttribArray": fn(u32);
    vertex_attrib_pointer = c"glVertexAttribPointer": fn(u32, i32, u32, u8, i32, *const c_void);
    viewport = c"glViewport": fn(i32, i32, i32, i32);
    draw_arrays = c"glDrawArrays": fn(u32, i32, i32);
    finish = c"glFinish": fn();
    get_error = c"glGetError": fn() -> u32;
    read_buffer = c"glReadBuffer": fn(u32);
    read_pixels = c"glReadPixels": fn(i32, i32, i32, i32, u32, u32, *mut c_void);
});

const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_NONE: i32 = 0x3038;
const GL_TEXTURE_3D: u32 = 0x806F;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_COLOR_ATTACHMENT1: u32 = 0x8CE1;
const GL_R32F: i32 = 0x822E;
const GL_RED: u32 = 0x1903;
const GL_RGBA: u32 = 0x1908;
const GL_FLOAT: u32 = 0x1406;
const GL_UNSIGNED_BYTE: u32 = 0x1401;

fn int(n: usize) -> Result<i32, WorkerError> {
    i32::try_from(n).map_err(|_| runtime("graphics size out of range"))
}

struct EglSession {
    egl: Egl,
    lib: Library,
    display: Ptr,
    context: Ptr,
    surface: Ptr,
}

impl EglSession {
    fn open(width: usize, height: usize) -> Result<Self, WorkerError> {
        let lib = Library::open()?;
        let egl = Egl::load(&|name| lib.symbol(name))?;
        let display = unsafe { (egl.get_display)(std::ptr::null_mut()) };
        let mut ctx =
            Self { egl, lib, display, context: std::ptr::null_mut(), surface: std::ptr::null_mut() };
        if ctx.display.is_null() {
            return Err(runtime("EGL initialization failed"));
        }
        let (mut major, mut minor) = (0i32, 0i32);
        if unsafe { (ctx.egl.initialize)(ctx.display, &raw mut major, &raw mut minor) } == 0 {
            return Err(runtime("EGL initialization failed"));
        }
        if unsafe { (ctx.egl.bind_api)(EGL_OPENGL_ES_API) } == 0 {
            return Err(runtime("OpenGL ES unavailable"));
        }
        let attrs: [i32; 13] =
            [0x3033, 1, 0x3040, 0x40, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 8, EGL_NONE];
        let mut config: Ptr = std::ptr::null_mut();
        let mut n = 0i32;
        let (choose, display) = (ctx.egl.choose_config, ctx.display);
        
        let chosen = unsafe { choose(display, attrs.as_ptr(), &raw mut config, 1, &raw mut n) };
        if chosen == 0 || n != 1 {
            return Err(runtime("no ES3 pbuffer configuration"));
        }
        let context_attrs: [i32; 3] = [0x3098, 3, EGL_NONE];
        
        ctx.context = unsafe {
            (ctx.egl.create_context)(ctx.display, config, std::ptr::null_mut(), context_attrs.as_ptr())
        };
        let surface_attrs: [i32; 5] = [0x3057, int(width)?, 0x3056, int(height)?, EGL_NONE];
        let create_surface = ctx.egl.create_pbuffer_surface;
        ctx.surface = unsafe { create_surface(display, config, surface_attrs.as_ptr()) };
        if ctx.context.is_null() || ctx.surface.is_null() {
            return Err(runtime("ES3 pbuffer context unavailable"));
        }
        if unsafe { (ctx.egl.make_current)(ctx.display, ctx.surface, ctx.surface, ctx.context) } == 0 {
            return Err(runtime("ES3 pbuffer context unavailable"));
        }
        Ok(ctx)
    }

    fn resolve(&self, name: &CStr) -> *const c_void {
        let p = unsafe { (self.egl.get_proc_address)(name.as_ptr()) };
        if p.is_null() { self.lib.symbol(name) } else { p }
    }
}

impl Drop for EglSession {
    fn drop(&mut self) {
        if self.display.is_null() {
            return;
        }
        let none = std::ptr::null_mut();
        unsafe { (self.egl.make_current)(self.display, none, none, none) };
        if !self.surface.is_null() {
            unsafe { (self.egl.destroy_surface)(self.display, self.surface) };
        }
        if !self.context.is_null() {
            unsafe { (self.egl.destroy_context)(self.display, self.context) };
        }
        unsafe { (self.egl.terminate)(self.display) };
    }
}

fn gl_string(get: &GetString, name: u32) -> Option<String> {
    let p = unsafe { (get.get_string)(name) };
    if p.is_null() {
        return None;
    }
    
    let s = unsafe { CStr::from_ptr(p.cast::<c_char>()) };
    Some(s.to_string_lossy().into_owned())
}

impl Gl {
    
    fn generate(f: unsafe extern "system" fn(i32, *mut u32)) -> u32 {
        let mut id = 0u32;
        unsafe { f(1, &raw mut id) };
        id
    }

    fn compile(&self, program: u32, kind: u32, text: &str) -> Result<(), WorkerError> {
        let source = CString::new(text).map_err(|_| runtime("native shader source contains NUL"))?;
        let shader = unsafe { (self.create_shader)(kind) };
        let strings = [source.as_ptr()];
        
        unsafe { (self.shader_source)(shader, 1, strings.as_ptr(), std::ptr::null()) };
        unsafe { (self.compile_shader)(shader) };
        let mut ok = 0i32;
        unsafe { (self.get_shader_iv)(shader, 0x8B81, &raw mut ok) };
        if ok == 0 {
            let mut buf = vec![0u8; 8192];
            
            unsafe {
                (self.get_shader_info_log)(
                    shader,
                    8192,
                    std::ptr::null_mut(),
                    buf.as_mut_ptr().cast::<c_char>(),
                );
            };
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            return Err(runtime(format!(
                "native shader compile failed: {}",
                String::from_utf8_lossy(&buf[..end])
            )));
        }
        unsafe { (self.attach_shader)(program, shader) };
        unsafe { (self.delete_shader)(shader) };
        Ok(())
    }

    fn location(&self, program: u32, name: &str) -> Result<i32, WorkerError> {
        let c = CString::new(name).map_err(|_| runtime("uniform name contains NUL"))?;
        Ok(unsafe { (self.get_uniform_location)(program, c.as_ptr()) })
    }

    fn texture_parameters(&self, target: u32, params: &[(u32, i32)]) {
        for (key, v) in params {
            unsafe { (self.tex_parameter_i)(target, *key, *v) };
        }
    }

    fn tex3(
        &self,
        program: u32,
        unit: u32,
        name: &str,
        shape: [usize; 3],
        values: &[f32],
    ) -> Result<u32, WorkerError> {

        if shape.iter().try_fold(1usize, |acc, &n| acc.checked_mul(n)) != Some(values.len()) {
            return Err(runtime("texture size disagrees with its shape"));
        }
        let id = Self::generate(self.gen_textures);
        unsafe { (self.active_texture)(0x84C0 + unit) };
        unsafe { (self.bind_texture)(GL_TEXTURE_3D, id) };
        self.texture_parameters(
            GL_TEXTURE_3D,
            &[(0x2801, 0x2601), (0x2800, 0x2601), (0x2802, 0x812F), (0x2803, 0x812F), (0x8072, 0x812F)],
        );
        unsafe { (self.pixel_store_i)(0x0CF5, 1) };
        let (w, h, d) = (int(shape[2])?, int(shape[1])?, int(shape[0])?);
        
        unsafe {
            (self.tex_image_3d)(
                GL_TEXTURE_3D,
                0,
                GL_R32F,
                w,
                h,
                d,
                0,
                GL_RED,
                GL_FLOAT,
                values.as_ptr().cast::<c_void>(),
            );
        };
        let loc = self.location(program, name)?;
        unsafe { (self.uniform1i)(loc, int(unit as usize)?) };
        Ok(id)
    }

    fn uniform(&self, program: u32, step: &Step) -> Result<(), WorkerError> {
        match step {
            Step::Float(name, v) => {
                let loc = self.location(program, name)?;
                match v.as_slice() {
                    [a] => unsafe { (self.uniform1f)(loc, *a) },
                    [a, b] => unsafe { (self.uniform2f)(loc, *a, *b) },
                    [a, b, c] => unsafe { (self.uniform3f)(loc, *a, *b, *c) },
                    _ => return Err(runtime(format!("uniform {name} has {} components", v.len()))),
                }
            }
            Step::Int(name, v) => {
                let loc = self.location(program, name)?;
                unsafe { (self.uniform1i)(loc, *v) };
            }
            Step::Mat3(name, m) => {
                let loc = self.location(program, name)?;
                
                unsafe { (self.uniform_matrix3fv)(loc, 1, 0, m.as_ptr()) };
            }
            Step::Texture { .. } => {}
        }
        Ok(())
    }

    fn error(&self) -> u32 {
        unsafe { (self.get_error)() }
    }
}



#[allow(clippy::too_many_lines)]
pub(crate) fn execute(plan: &RenderPlan, sources: &ShaderSources) -> Result<Readback, WorkerError> {
    let [width, height] = plan.raster;

    let texels = width.checked_mul(height).filter(|&n| {
        n > 0
            && n <= crate::worker::MAX_EGL_RASTER_PIXELS
            && width.max(height) <= crate::worker::MAX_EGL_RASTER_SIDE_PX
    });
    let Some(texels) = texels else {
        return Err(runtime("invalid offscreen raster size"));
    };
    let ctx = EglSession::open(width, height)?;
    let resolve = |name: &CStr| ctx.resolve(name);
    let get = GetString::load(&resolve)?;
    let extensions = gl_string(&get, 0x1F03).unwrap_or_default();
    if !extensions.contains("GL_OES_texture_float_linear")
        || !extensions.contains("GL_EXT_color_buffer_float")
    {
        return Err(runtime("linear floating-point textures and float framebuffer support are required"));
    }
    let gl_renderer = gl_string(&get, 0x1F01).ok_or_else(|| runtime("GL_RENDERER is unavailable"))?;
    let gl_version = gl_string(&get, 0x1F02).ok_or_else(|| runtime("GL_VERSION is unavailable"))?;
    let gl = Gl::load(&resolve)?;
    let program = unsafe { (gl.create_program)() };
    gl.compile(program, 0x8B31, &sources.vertex)?;
    gl.compile(program, 0x8B30, &sources.fragment)?;
    unsafe { (gl.link_program)(program) };
    let mut ok = 0i32;
    unsafe { (gl.get_program_iv)(program, 0x8B82, &raw mut ok) };
    if ok == 0 {
        return Err(runtime("native shader link failed"));
    }
    unsafe { (gl.use_program)(program) };
    for step in &plan.steps {
        if let Step::Texture { unit, name, shape, values } = step {
            gl.tex3(program, *unit, name, *shape, values)?;
        } else {
            gl.uniform(program, step)?;
        }
    }
    let (w, h) = (int(width)?, int(height)?);
    let fbo = Gl::generate(gl.gen_framebuffers);
    unsafe { (gl.bind_framebuffer)(GL_FRAMEBUFFER, fbo) };
    for (i, (internal, format, kind)) in
        [(0x8058i32, GL_RGBA, GL_UNSIGNED_BYTE), (GL_R32F, GL_RED, GL_FLOAT)].into_iter().enumerate()
    {
        let id = Gl::generate(gl.gen_textures);
        unsafe { (gl.bind_texture)(GL_TEXTURE_2D, id) };
        
        unsafe { (gl.tex_image_2d)(GL_TEXTURE_2D, 0, internal, w, h, 0, format, kind, std::ptr::null()) };
        gl.texture_parameters(GL_TEXTURE_2D, &[(0x2801, 0x2600), (0x2800, 0x2600)]);
        let attachment = GL_COLOR_ATTACHMENT0 + u32::try_from(i).unwrap_or(0);
        unsafe { (gl.framebuffer_texture_2d)(GL_FRAMEBUFFER, attachment, GL_TEXTURE_2D, id, 0) };
    }
    let buffers = [GL_COLOR_ATTACHMENT0, GL_COLOR_ATTACHMENT1];
    unsafe { (gl.draw_buffers)(2, buffers.as_ptr()) };
    if unsafe { (gl.check_framebuffer_status)(GL_FRAMEBUFFER) } != 0x8CD5 {
        return Err(runtime("native offscreen framebuffer incomplete"));
    }
    let vao = Gl::generate(gl.gen_vertex_arrays);
    unsafe { (gl.bind_vertex_array)(vao) };
    let vertices: [f32; 6] = [-1.0, -1.0, 3.0, -1.0, -1.0, 3.0];
    let vbo = Gl::generate(gl.gen_buffers);
    unsafe { (gl.bind_buffer)(0x8892, vbo) };
    
    unsafe { (gl.buffer_data)(0x8892, 24, vertices.as_ptr().cast::<c_void>(), 0x88E4) };
    let attribute = unsafe { (gl.get_attrib_location)(program, c"a_pos".as_ptr()) };
    let Ok(attribute) = u32::try_from(attribute) else {
        return Err(runtime("native vertex attribute unavailable"));
    };
    unsafe { (gl.enable_vertex_attrib_array)(attribute) };
    
    unsafe { (gl.vertex_attrib_pointer)(attribute, 2, GL_FLOAT, 0, 0, std::ptr::null()) };
    unsafe { (gl.viewport)(0, 0, w, h) };
    unsafe { (gl.draw_arrays)(0x0004, 0, 3) };
    unsafe { (gl.finish)() };
    let error = gl.error();
    if error != 0 {
        return Err(runtime(format!("native GL operation failed: {error:#x}")));
    }
    let mut pixels = vec![0u8; texels * 4];
    unsafe { (gl.read_buffer)(GL_COLOR_ATTACHMENT0) };
    
    unsafe { (gl.read_pixels)(0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, pixels.as_mut_ptr().cast::<c_void>()) };
    let mut steps = vec![0f32; texels];
    unsafe { (gl.read_buffer)(GL_COLOR_ATTACHMENT1) };
    
    unsafe { (gl.read_pixels)(0, 0, w, h, GL_RED, GL_FLOAT, steps.as_mut_ptr().cast::<c_void>()) };
    if gl.error() != 0 || steps.iter().any(|x| !x.is_finite()) {
        return Err(runtime("native pixel read failed"));
    }
    drop(ctx);
    Ok(Readback { gl_renderer, gl_version, pixels, steps })
}
