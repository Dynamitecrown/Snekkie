//! How the window gets drawn.
//!
//! Plain OpenGL isn't available everywhere Snekkie runs: Remote Desktop
//! sessions and VMs without a GPU driver (a Hyper-V jump box, say) only
//! offer OpenGL 1.1. So drawing goes through wgpu, which uses DirectX 12 on
//! Windows and Vulkan or OpenGL ES elsewhere, and this picks the adapter:
//! a real GPU if there is one, otherwise the platform's software renderer
//! (WARP on Windows, llvmpipe/lavapipe on Linux), rather than failing.

use std::sync::Arc;

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use eframe::wgpu;

/// Lower is better.
fn device_rank(kind: wgpu::DeviceType) -> u8 {
    match kind {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    }
}

/// Lower is better. DirectX 12 is the most dependable on Windows; some
/// laptop Vulkan drivers are not.
fn backend_rank(backend: wgpu::Backend) -> u8 {
    match backend {
        wgpu::Backend::Dx12 | wgpu::Backend::Metal => 0,
        wgpu::Backend::Vulkan => 1,
        wgpu::Backend::Gl => 2,
        _ => 3,
    }
}

/// Adapters that can draw to the window, best first.
fn ranked(adapters: &[wgpu::Adapter], surface: Option<&wgpu::Surface<'_>>) -> Vec<wgpu::Adapter> {
    let mut usable: Vec<wgpu::Adapter> =
        adapters.iter().filter(|a| surface.is_none_or(|s| a.is_surface_supported(s))).cloned().collect();
    usable.sort_by_key(|a| {
        let info = a.get_info();
        (device_rank(info.device_type), backend_rank(info.backend))
    });
    usable
}

type DeviceDescriptor = Arc<dyn Fn(&wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> + Send + Sync>;

/// The best adapter that actually hands out a device. A driver can list an
/// adapter it then can't use (seen with DirectX 12 under some VMs and
/// Wine), so each candidate is tried in turn rather than trusting the
/// first.
fn pick_adapter(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
    descriptor: &DeviceDescriptor,
) -> Result<wgpu::Adapter, String> {
    let candidates = ranked(adapters, surface);
    let mut failures = Vec::new();
    for adapter in candidates {
        let info = adapter.get_info();
        match pollster::block_on(adapter.request_device(&descriptor(&adapter))) {
            Ok(_) => return Ok(adapter),
            Err(e) => failures.push(format!("{} ({:?}): {e}", info.name, info.backend)),
        }
    }
    if failures.is_empty() {
        let found: Vec<String> =
            adapters.iter().map(|a| format!("{} ({:?})", a.get_info().name, a.get_info().backend)).collect();
        Err(format!("No graphics adapter can draw to this window. Found: {}", found.join(", ")))
    } else {
        Err(format!("No graphics adapter could start. Tried: {}", failures.join("; ")))
    }
}

pub fn wgpu_options() -> WgpuConfiguration {
    let setup = WgpuSetupCreateNew::without_display_handle();
    let descriptor = setup.device_descriptor.clone();
    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(WgpuSetupCreateNew {
            native_adapter_selector: Some(Arc::new(move |adapters, surface| {
                pick_adapter(adapters, surface, &descriptor)
            })),
            ..setup
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_gpus_beat_software_and_dx12_beats_gl() {
        use wgpu::{Backend, DeviceType};
        let mut options = [
            (DeviceType::Cpu, Backend::Dx12),
            (DeviceType::IntegratedGpu, Backend::Gl),
            (DeviceType::IntegratedGpu, Backend::Dx12),
            (DeviceType::VirtualGpu, Backend::Vulkan),
        ];
        options.sort_by_key(|(d, b)| (device_rank(*d), backend_rank(*b)));
        assert_eq!(options[0], (DeviceType::IntegratedGpu, Backend::Dx12));
        assert_eq!(options[3], (DeviceType::Cpu, Backend::Dx12));
    }
}
