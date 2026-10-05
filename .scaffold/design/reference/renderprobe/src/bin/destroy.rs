use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
fn main() {
    let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
    d.flags = wgpu::InstanceFlags::empty();
    d.backends = wgpu::Backends::from_env().filter(|b| !b.is_empty()).unwrap_or(wgpu::Backends::VULKAN);
    let inst = wgpu::Instance::new(d);
    for adapter in pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all())) {
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let lost = Arc::new(AtomicBool::new(false));
        let l2 = lost.clone();
        device.set_device_lost_callback(move |r, m| { println!("  lost callback: {r:?} {m:?}"); l2.store(true, Ordering::SeqCst); });
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| println!("  uncaptured: {}", e.to_string().lines().next().unwrap_or(""))));
        device.destroy();
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        queue.submit([]);
        let _ = device.poll(wgpu::PollType::Poll);
        println!("{} ({:?}): lost flag after destroy = {}", adapter.get_info().name, adapter.get_info().backend, lost.load(Ordering::SeqCst));
        // a new device on the same adapter works?
        let r = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()));
        println!("  new device after destroy: {}", r.is_ok());
    }
}
