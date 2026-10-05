//! Does FIFO present block on a minimized Wayland window, and does frame-callback gating keep
//! the update loop alive? usage: hidden naive|gated   (WGPU_BACKEND=vulkan|gl, WGPU_POWER_PREF)
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

static UPDATES: AtomicU64 = AtomicU64::new(0);
static PRESENTS: AtomicU64 = AtomicU64::new(0);

struct Gpu { surface: wgpu::Surface<'static>, device: wgpu::Device, queue: wgpu::Queue, config: wgpu::SurfaceConfiguration }
struct App { gated: bool, window: Option<Arc<Window>>, gpu: Option<Gpu>, start: Instant, minimized: bool,
             frame_ready: bool, redraw_pending: bool, mark: Option<(u64, u64)>, display: winit::event_loop::OwnedDisplayHandle, label: String }

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() { return; }
        let window = Arc::new(el.create_window(Window::default_attributes().with_title("boxel hidden-window probe").with_inner_size(winit::dpi::LogicalSize::new(480, 270))).unwrap());
        let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(self.display.clone()));
        desc.backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::VULKAN);
        desc.flags = wgpu::InstanceFlags::empty();
        let instance = wgpu::Instance::new(desc);
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance),
            compatible_surface: Some(&surface), force_fallback_adapter: false, ..Default::default() })).unwrap();
        self.label = format!("{} {:?}", adapter.get_info().name, adapter.get_info().backend);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let size = window.inner_size();
        let mut config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)).unwrap();
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        self.gpu = Some(Gpu { surface, device, queue, config });
        self.window = Some(window);
        self.redraw_pending = true; // winit delivers the first RedrawRequested on its own
    }
    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, e: WindowEvent) {
        match e {
            WindowEvent::RedrawRequested => { self.frame_ready = true; self.redraw_pending = false; }
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) if s.width > 0 && s.height > 0 => if let Some(g) = &mut self.gpu {
                g.config.width = s.width; g.config.height = s.height; g.surface.configure(&g.device, &g.config);
            },
            _ => {}
        }
    }
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let t = self.start.elapsed();
        UPDATES.fetch_add(1, Ordering::Relaxed);
        let (Some(window), Some(g)) = (&self.window, &self.gpu) else { return };
        if t > Duration::from_millis(1500) && !self.minimized {
            println!("RESULT {} visible 0..1.5 s: updates={} presents={}", if self.gated { "gated" } else { "naive" }, UPDATES.load(Ordering::Relaxed), PRESENTS.load(Ordering::Relaxed));
            window.set_minimized(true); self.minimized = true;
        }
        if t > Duration::from_millis(2500) && self.mark.is_none() {
            self.mark = Some((UPDATES.load(Ordering::Relaxed), PRESENTS.load(Ordering::Relaxed)));
        }
        if t > Duration::from_millis(4500) {
            let (u0, p0) = self.mark.unwrap();
            println!("RESULT {} {}: while minimized (2 s): updates={} presents={}", self.label,
                     if self.gated { "gated" } else { "naive" },
                     UPDATES.load(Ordering::Relaxed) - u0, PRESENTS.load(Ordering::Relaxed) - p0);
            el.exit();
            return;
        }
        let present = if self.gated { self.frame_ready } else { true };
        if present {
            if let wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) = g.surface.get_current_texture() {
                let view = frame.texture.create_view(&Default::default());
                let mut enc = g.device.create_command_encoder(&Default::default());
                drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLUE), store: wgpu::StoreOp::Store } })],
                    depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None }));
                g.queue.submit([enc.finish()]);
                window.pre_present_notify();
                g.queue.present(frame);
                PRESENTS.fetch_add(1, Ordering::Relaxed);
            }
            self.frame_ready = false;
        }
        if self.gated && !self.frame_ready && !self.redraw_pending { window.request_redraw(); self.redraw_pending = true; }
        el.set_control_flow(ControlFlow::Poll);
    }
}

fn main() {
    let gated = std::env::args().nth(1).as_deref() == Some("gated");
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(8));
        println!("RESULT WATCHDOG: main thread stuck after 8 s (updates={} presents={}): present blocked", UPDATES.load(Ordering::Relaxed), PRESENTS.load(Ordering::Relaxed));
        std::process::exit(3);
    });
    let el = EventLoop::new().unwrap();
    let display = el.owned_display_handle();
    let mut app = App { gated, window: None, gpu: None, start: Instant::now(), minimized: false, frame_ready: false,
                        redraw_pending: false, mark: None, display, label: String::new() };
    el.run_app(&mut app).unwrap();
}
