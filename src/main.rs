#![windows_subsystem = "windows"]

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use nalgebra::Vector2;

use wgpu::rwh::{HasWindowHandle, RawWindowHandle};
use wgpu::{
    Color, CommandEncoderDescriptor, CompositeAlphaMode, Instance, LoadOp, Operations, PresentMode,
    RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions, StoreOp, Surface,
    SurfaceConfiguration, TextureFormat, TextureUsages, TextureViewDescriptor,
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Fullscreen, Window, WindowAttributes, WindowId},
};

use macintosh_wallpaper::{
    animation::Animation,
    pipelines::{Gpu, Renderer},
};

struct Application {
    gpu: Gpu,
    state: Option<State>,
}

struct State {
    window: Arc<Window>,
    surface: Surface<'static>,
    renderer: Renderer,

    cursor_start: Option<PhysicalPosition<f64>>,
    preview: bool,
}

fn main() -> Result<()> {
    let instance = Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions::default()))
        .context("No adapter found")?;
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default(), None))?;

    let mut app = Application {
        gpu: Gpu {
            instance,
            adapter,
            device,
            queue,

            texture_format: TextureFormat::Bgra8UnormSrgb,
        },
        state: None,
    };

    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut app)?;

    Ok(())
}

impl ApplicationHandler for Application {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let config = include_bytes!("../animation/animation.bin");
        let animation = Animation::load(config).unwrap().runtime_from_args();
        let rt = &animation.runtime;
        let preview = rt.preview.is_some();
        let desktop = rt.desktop;

        #[cfg(windows)]
        if desktop && !ensure_single_instance() {
            event_loop.exit();
            return;
        }

        #[cfg(windows)]
        if rt.configure {
            message_box("This screensaver has no configuration options.");
            event_loop.exit();
            return;
        }

        #[allow(unused_mut)]
        let mut attrs = WindowAttributes::default()
            .with_fullscreen((rt.full_screen && !desktop).then_some(Fullscreen::Borderless(None)))
            .with_title("Macintosh Wallpaper")
            .with_decorations(!desktop)
            .with_visible(false);

        #[cfg(windows)]
        if let Some(hwnd) = rt.preview {
            use ::{wgpu::rwh::Win32WindowHandle, winit::dpi::PhysicalSize};
            use std::num::NonZeroIsize;

            let parent = Win32WindowHandle::new(NonZeroIsize::new(hwnd).unwrap());
            unsafe {
                attrs = attrs
                    .with_inner_size(PhysicalSize::new(152, 112))
                    .with_parent_window(Some(parent.into()))
                    .with_decorations(false)
            }
        }

        let window = Arc::new(event_loop.create_window(attrs).unwrap());

        #[cfg(windows)]
        if desktop {
            let hwnd = get_hwnd(&window);

            match find_workerw() {
                Some(workerw) => {
                    if !embed_window(hwnd, workerw) {
                    message_box("Failed to embed window into WorkerW.");
                    event_loop.exit();
                    return;
                    }        
                }        
                None => {
                    message_box("Could not find WorkerW window.");
                    event_loop.exit();
                    return;
                 }
            }
        }
        window.set_cursor_visible(!rt.full_screen);
        window.set_visible(true);

        let surface = self.gpu.instance.create_surface(window.clone()).unwrap();
        let renderer = Renderer::new(&self.gpu, animation);
        self.state = Some(State {
            surface,
            window,
            renderer,

            cursor_start: None,
            preview,
        });
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = &mut self.state else { return };
        if window_id != state.window.id() {
            return;
        }

        let full_screen = state.window.fullscreen().is_some();
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_size) => self.resize_surface(),
            WindowEvent::CursorMoved { position, .. } if !state.preview && full_screen => {
                match state.cursor_start {
                    Some(start) if start != position => {
                        state.window.set_visible(false);
                        event_loop.exit()
                    }
                    None => state.cursor_start = Some(position),
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                let output = state.surface.get_current_texture().unwrap();

                let mut encoder =
                    (self.gpu.device).create_command_encoder(&CommandEncoderDescriptor::default());
                let view = (output.texture).create_view(&TextureViewDescriptor::default());

                {
                    let mut render_pass = encoder.begin_render_pass(&RenderPassDescriptor {
                        label: None,
                        color_attachments: &[Some(RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: Operations {
                                load: LoadOp::Clear(Color::BLACK),
                                store: StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });

                    let size = state.window.inner_size();
                    let size = Vector2::new(size.width, size.height);
                    state.renderer.render(&self.gpu, size, &mut render_pass);
                }

                self.gpu.queue.submit([encoder.finish()]);

                output.present();
                state.window.request_redraw();
            }
            _ => {}
        }
    }
}

impl Application {
    fn resize_surface(&mut self) {
        let state = self.state.as_mut().unwrap();
        let size = state.window.inner_size();
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format: self.gpu.texture_format,
            width: size.width,
            height: size.height,
            present_mode: PresentMode::AutoVsync,
            alpha_mode: CompositeAlphaMode::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        state.surface.configure(&self.gpu.device, &config);
    }
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "C" {
    fn ShellMessageBoxA(
        app: isize,
        hwnd: isize,
        caption: *const i8,
        title: *const i8,
        u_type: u32,
    ) -> i32;
}

#[cfg(windows)]
fn message_box(message: impl Into<Vec<u8>>) {
    use std::ffi::CString;

    unsafe {
        let title = CString::new("Macintosh Screensaver").unwrap();
        let caption = CString::new(message.into()).unwrap();
        ShellMessageBoxA(0, 0, caption.as_ptr(), title.as_ptr(), 0);
    }
}

#[cfg(windows)]
fn get_hwnd(window: &Window) -> isize {
    match window.window_handle().unwrap().as_raw() {
        RawWindowHandle::Win32(handle) => handle.hwnd.get(),
        _ => panic!("Not a Windows window"),
    }
}

#[cfg(windows)]
static INSTANCE_MUTEX: OnceLock<isize> = OnceLock::new();

#[cfg(windows)]
fn ensure_single_instance() -> bool {
    const ERROR_ALREADY_EXISTS: u32 = 183;

    let name: Vec<u16> = "Global\\MacintoshWallpaperInstance"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let handle = CreateMutexW(0, 1, name.as_ptr());

        if handle == 0 {
            return false;
        }

        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            return false;
        }

        let _ = INSTANCE_MUTEX.set(handle);
    }

    true
}

#[cfg(windows)]
struct WorkerSearch {
    shell_class: Vec<u16>,
    worker_class: Vec<u16>,
    found: isize,
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn SetParent(hwnd: isize, new_parent: isize) -> isize;

    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;

    fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;

    fn GetClientRect(hwnd: isize, rect: *mut Rect) -> i32;
    
    fn SetWindowPos(
        hwnd: isize,
        insert_after: isize,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    fn FindWindowW(class_name: *const u16, window_name: *const u16) -> isize;

    fn FindWindowExW(
        parent: isize,
        child_after: isize,
        class_name: *const u16,
        window_name: *const u16,
    ) -> isize;

    fn SendMessageTimeoutW(
        hwnd: isize,
        msg: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        timeout: u32,
        result: *mut usize,
    ) -> isize;

    fn EnumWindows(
        callback: Option<unsafe extern "system" fn(isize, isize) -> i32>,
        lparam: isize,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateMutexW(
        attributes: isize,
        initial_owner: i32,
        name: *const u16,
    ) -> isize;

    fn GetLastError() -> u32;

    fn CloseHandle(handle: isize) -> i32;
}

#[cfg(windows)]
unsafe extern "system" fn enum_windows_proc(hwnd: isize, lparam: isize) -> i32 {
    let data = unsafe { &mut *(lparam as *mut WorkerSearch) };

    let shell = unsafe {
        FindWindowExW(
            hwnd,
            0,
            data.shell_class.as_ptr(),
            std::ptr::null(),
        )
    };

    if shell != 0 {
        let worker = unsafe {
            FindWindowExW(
                0,
                hwnd,
                data.worker_class.as_ptr(),
                std::ptr::null(),
            )
        };

        if worker != 0 {
            data.found = worker;
            return 0;
        }
    }

    1
}

#[cfg(windows)]
fn find_workerw() -> Option<isize> {
    let progman_class = "Progman\0".encode_utf16().collect::<Vec<_>>();

    let progman = unsafe {
        FindWindowW(progman_class.as_ptr(), std::ptr::null())
    };

    if progman == 0 {
        return None;
    }

    let mut result = 0usize;

    unsafe {
        SendMessageTimeoutW(
            progman,
            0x052C,
            0,
            0,
            0,
            1000,
            &mut result,
        );
    }

    let mut data = WorkerSearch {
        shell_class: "SHELLDLL_DefView\0".encode_utf16().collect(),
        worker_class: "WorkerW\0".encode_utf16().collect(),
        found: 0,
    };

    unsafe {
        EnumWindows(
            Some(enum_windows_proc),
            &mut data as *mut WorkerSearch as isize,
        );
    }

    if data.found != 0 {
        Some(data.found)
    } else {
        None
    }
}

#[cfg(windows)]
#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
fn embed_window(hwnd: isize, workerw: isize) -> bool {
    const GWL_STYLE: i32 = -16;

    const WS_CHILD: isize = 0x40000000;
    const WS_POPUP: isize = 0x80000000u32 as isize;
    const WS_VISIBLE: isize = 0x10000000;
    const WS_CLIPSIBLINGS: isize = 0x04000000;

    const SWP_NOZORDER: u32 = 0x0004;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_FRAMECHANGED: u32 = 0x0020;
    const SWP_SHOWWINDOW: u32 = 0x0040;

    let mut rect = Rect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    unsafe {
        if GetClientRect(workerw, &mut rect) == 0 {
            return false;
        }

        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;

        if width <= 0 || height <= 0 {
            return false;
        }

        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);

        let new_style =
            (style & !WS_POPUP) | WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS;

        SetWindowLongPtrW(hwnd, GWL_STYLE, new_style);

        SetWindowPos(
            hwnd,
            0,
            0,
            0,
            0,
            0,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );

        SetParent(hwnd, workerw);

        SetWindowPos(
            hwnd,
            0,
            0,
            0,
            width,
            height,
            SWP_NOZORDER
                | SWP_NOACTIVATE
                | SWP_FRAMECHANGED
                | SWP_SHOWWINDOW,
        ) != 0
    }
}
