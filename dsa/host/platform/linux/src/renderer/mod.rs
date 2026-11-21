//! The graphics renderer in a process of its own on Linux: `dsa-gfx`.
//!
//! The compositor (`dsa-compositor`) used to load gfxstream itself, so a crash in the renderer
//! or in the GPU driver under it ended every task window. Now the compositor starts `dsa-gfx`
//! from beside itself ([`client`]), and the renderer process ([`process`]) loads
//! `libgfxstream_backend.so`, takes the guest's graphics streams on vsock (or the Unix sockets
//! a virtual machine monitor proxies them to), and puts each task's frames into dmabufs it
//! exports. Each dmabuf crosses to the compositor once, over the Unix socket between the two
//! (`SCM_RIGHTS`), and every later frame names it by its slot; nothing crosses the CPU. Where
//! the driver or the desktop cannot take dmabufs, a frame is read back into a memfd that
//! crosses the same way.
//!
//! When the renderer ends, the compositor and its windows stay with their last frame, and the
//! compositor starts the renderer again (see [`dsa_host_core::renderer::supervise`]); the
//! guest's streams drop with the old process and the guest connects again.
//!
//! This file holds what both sides share.

use dsa_host_core::render_gpu::RenderGpu;
use dsa_protocol::renderer::types::{GpuRule, Start};

use crate::compositor::gfxstream::{GpuChoice, COMMAND_PORT, PROCESS_PIPE_PORT};

pub mod client;
pub mod process;

/// The backend's GPU rules (`DSA_GFX_GPU_*`).
const GPU_INTEGRATED: u32 = 2;
const GPU_DISCRETE: u32 = 3;
const GPU_NAME: u32 = 4;

/// The `Start` for the user's render GPU setting (`display`, `integrated`, `discrete` or part
/// of a device's name). A Linux desktop says no adapter for its primary monitor, so `display`
/// leaves the choice to gfxstream's own ranking.
pub fn start_for(render_gpu: &str, frames_on_gpu: bool) -> Start {
    // No display source says which GPU drives the primary monitor here, so the renderer ranks
    // the GPUs itself for `display` and for a blank setting.
    let (rule, name) = match RenderGpu::parse(render_gpu).unwrap_or_default() {
        RenderGpu::Display => (GpuRule::Ranking, String::new()),
        RenderGpu::Integrated => (GpuRule::Integrated, String::new()),
        RenderGpu::Discrete => (GpuRule::Discrete, String::new()),
        RenderGpu::Named(name) => (GpuRule::Name, name),
    };
    Start {
        gpu_rule: rule as i32,
        gpu_name: name,
        command_port: COMMAND_PORT,
        process_pipe_port: PROCESS_PIPE_PORT,
        frames_on_gpu,
        ..Start::default()
    }
}

/// The GPU `start` asks gfxstream for; `None` leaves gfxstream's own ranking.
pub fn gpu_choice(start: &Start) -> Option<GpuChoice> {
    let (rule, name) = match GpuRule::try_from(start.gpu_rule).ok()? {
        GpuRule::Integrated => (GPU_INTEGRATED, String::new()),
        GpuRule::Discrete => (GPU_DISCRETE, String::new()),
        GpuRule::Name if !start.gpu_name.is_empty() => (GPU_NAME, start.gpu_name.clone()),
        _ => return None,
    };
    Some(GpuChoice { rule, name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr3_the_render_gpu_setting_reaches_the_renderer_process() {
        assert_eq!(gpu_choice(&start_for("display", true)), None);
        assert_eq!(gpu_choice(&start_for("", true)), None);
        assert_eq!(
            gpu_choice(&start_for("Discrete", true)),
            Some(GpuChoice {
                rule: GPU_DISCRETE,
                name: String::new()
            })
        );
        assert_eq!(
            gpu_choice(&start_for("integrated", false)),
            Some(GpuChoice {
                rule: GPU_INTEGRATED,
                name: String::new()
            })
        );
        assert_eq!(
            gpu_choice(&start_for(" Radeon ", true)),
            Some(GpuChoice {
                rule: GPU_NAME,
                name: "Radeon".into()
            })
        );
        let start = start_for("display", false);
        assert!(!start.frames_on_gpu);
        assert_eq!(
            (start.command_port, start.process_pipe_port),
            (COMMAND_PORT, PROCESS_PIPE_PORT)
        );
    }
}
