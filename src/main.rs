//! Pinta-rs: a Rust port of Pinta (https://github.com/PintaProject/Pinta)
//! with a tiled, GPU-rendered canvas for very large images.

mod app;
mod blend;
mod composite;
mod document;
mod effects;
mod flood;
mod gpu;
mod io;
mod ops;
mod raster;
mod selection;
mod script;
mod selftest;
mod surface;
mod text;
mod tools;

use eframe::egui;
use egui_wgpu::wgpu;
use std::sync::Arc;

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("pinta_rs=info,wgpu=warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--selftest") {
        selftest::run(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20000));
        return Ok(());
    }
    let files: Vec<std::path::PathBuf> = args.into_iter().map(Into::into).collect();

    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::METAL | wgpu::Backends::DX12 | wgpu::Backends::GL);
    setup.power_preference = wgpu::PowerPreference::HighPerformance;
    setup.device_descriptor = Arc::new(|adapter| {
        let a = adapter.limits();
        let base = if adapter.get_info().backend == wgpu::Backend::Gl { wgpu::Limits::downlevel_webgl2_defaults() } else { wgpu::Limits::default() };
        wgpu::DeviceDescriptor {
            label: Some("pinta-rs device"),
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: a.max_texture_dimension_2d.min(16384),
                // the canvas tile cache is a texture array; more layers = more cached tiles
                max_texture_array_layers: a.max_texture_array_layers.min(2048),
                ..base
            },
            ..Default::default()
        }
    });

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 860.0]).with_min_inner_size([640.0, 400.0]).with_title("Pinta-rs").with_app_id("pinta-rs").with_drag_and_drop(true),
        wgpu_options: egui_wgpu::WgpuConfiguration { wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup), ..Default::default() },
        ..Default::default()
    };
    eframe::run_native(
        "Pinta-rs",
        options,
        Box::new(|cc| {
            if let Some(rs) = cc.wgpu_render_state.as_ref() {
                let srgb = rs.target_format.is_srgb();
                cc.egui_ctx.data_mut(|d| d.insert_temp(egui::Id::new("srgb_target"), srgb));
            }
            Ok(Box::new(app::App::new(cc, files)))
        }),
    )
}
