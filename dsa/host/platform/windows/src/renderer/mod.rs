//! The graphics renderer in a process of its own: `dsa-gfx.exe`, which Task Manager shows as
//! "Graphics renderer for Android" ("Android용 그래픽 렌더러" on a Korean desktop, and in the
//! display language in each of DSA's interface languages).
//!
//! The window manager (`dsa-wm.exe`) used to load gfxstream itself, so a crash in the
//! renderer or in the GPU driver under it ended the window manager and every app window with
//! it. Now the window manager starts `dsa-gfx.exe` beside itself ([`client`]), and the renderer
//! process ([`process`]) loads `gfxstream_backend.dll`, runs on the render GPU the window
//! manager asks for, takes the guest's graphics streams on the VM's Hyper-V sockets, and
//! copies each task's frames on the GPU into the Direct3D 11 textures the window manager
//! makes for the app windows.
//!
//! A texture crosses as an NT handle: the window manager duplicates its handle into the
//! renderer's process (it started the process, so it holds a handle to it) and sends the
//! value. The renderer opens it once and copies every later frame of the display into it,
//! waiting on the GPU for each copy before it says the frame is there; nothing crosses the
//! CPU. When the renderer ends, the window manager and the app windows stay with their last
//! frame, and the window manager starts the renderer again (see
//! [`dsa_host_core::renderer::supervise`]); the guest's streams drop with the old process and
//! the guest connects again to the new one. When the window manager ends without stopping the
//! renderer, the renderer waits for the next window manager, which takes it over
//! ([`adoption_pipe`]).
//!
//! This file turns the render GPU request and the renderer's report into the messages and
//! back, which every build tests.

use dsa_host_core::graphics_state::GraphicsState;
use dsa_protocol::renderer::types::{
    FramePath, GpuMatch, GpuRule, Start, Started, VulkanDriverKind, VulkanDriverRule,
};

use crate::render_gpu::{GpuRequest, Luid, RendererGpu};
use crate::vulkan_driver::{DriverOutcome, VulkanDriverSetting};

#[cfg(windows)]
pub mod client;
#[cfg(windows)]
pub mod process;

/// The renderer's program file, installed beside the window manager.
pub const RENDERER_EXE: &str = "dsa-gfx.exe";

/// The pipe a renderer whose window manager ended without stopping it waits on, for the next
/// window manager in this session to take it over. One per session: one window manager runs
/// in each.
#[cfg(windows)]
pub fn adoption_pipe() -> String {
    format!("dsa-gfx-adopt-{}", crate::compositor::current_session())
}

/// The `Start` that asks for `request`, with the guest's streams on the vsock ports
/// `ports` (the command stream's, then the process pipe's).
pub fn start_for(request: &GpuRequest, ports: (u32, u32)) -> Start {
    let (rule, luid, name) = match request {
        GpuRequest::Ranking => (GpuRule::Ranking, Vec::new(), String::new()),
        GpuRequest::Luid(Luid(bytes)) => (GpuRule::Luid, bytes.to_vec(), String::new()),
        GpuRequest::Integrated => (GpuRule::Integrated, Vec::new(), String::new()),
        GpuRequest::Discrete => (GpuRule::Discrete, Vec::new(), String::new()),
        GpuRequest::Named(name) => (GpuRule::Name, Vec::new(), name.clone()),
    };
    Start {
        gpu_rule: rule as i32,
        gpu_luid: luid,
        gpu_name: name,
        display_width: 0,
        display_height: 0,
        command_port: ports.0,
        process_pipe_port: ports.1,
        command_socket: String::new(),
        process_pipe_socket: String::new(),
        frames_on_gpu: true,
        vulkan_driver: VulkanDriverRule::Auto as i32,
    }
}

/// [`start_for`], with the user's Vulkan driver setting.
pub fn start_with(request: &GpuRequest, vulkan: VulkanDriverSetting, ports: (u32, u32)) -> Start {
    Start {
        vulkan_driver: match vulkan {
            VulkanDriverSetting::Auto => VulkanDriverRule::Auto,
            VulkanDriverSetting::Native => VulkanDriverRule::Native,
            VulkanDriverSetting::Dozen => VulkanDriverRule::Dozen,
        } as i32,
        ..start_for(request, ports)
    }
}

/// The Vulkan driver setting `start` asks for; a value this build does not know is `auto`.
pub fn vulkan_driver_of(start: &Start) -> VulkanDriverSetting {
    match VulkanDriverRule::try_from(start.vulkan_driver) {
        Ok(VulkanDriverRule::Native) => VulkanDriverSetting::Native,
        Ok(VulkanDriverRule::Dozen) => VulkanDriverSetting::Dozen,
        _ => VulkanDriverSetting::Auto,
    }
}

/// The Vulkan fields of `Started` for what the renderer runs on.
pub fn report_vulkan_driver(started: &mut Started, outcome: DriverOutcome, reason: &str) {
    started.vulkan_driver = match outcome {
        DriverOutcome::Native => VulkanDriverKind::Native,
        DriverOutcome::Dozen => VulkanDriverKind::Dozen,
        DriverOutcome::DozenOnCpu => VulkanDriverKind::DozenCpu,
        DriverOutcome::None => VulkanDriverKind::None,
    } as i32;
    started.vulkan_driver_reason = reason.to_string();
}

/// What the renderer said it runs on, as the service is told of it. A renderer that does not
/// say leaves it unknown, which never keeps Android from starting.
pub fn graphics_state_of(started: &Started) -> GraphicsState {
    let reason = started.vulkan_driver_reason.clone();
    match VulkanDriverKind::try_from(started.vulkan_driver) {
        Ok(VulkanDriverKind::Native) => GraphicsState::Native(reason),
        Ok(VulkanDriverKind::Dozen) => GraphicsState::Layered(reason),
        Ok(VulkanDriverKind::DozenCpu) => GraphicsState::LayeredOnCpu(reason),
        Ok(VulkanDriverKind::None) => GraphicsState::Unavailable(reason),
        _ => GraphicsState::Unknown,
    }
}

/// The render GPU `start` asks for. A request that does not hold together asks for nothing,
/// and gfxstream ranks the GPUs itself.
pub fn request_of(start: &Start) -> GpuRequest {
    match GpuRule::try_from(start.gpu_rule).unwrap_or(GpuRule::Ranking) {
        GpuRule::Ranking => GpuRequest::Ranking,
        GpuRule::Luid => <[u8; 8]>::try_from(start.gpu_luid.as_slice())
            .map(|bytes| GpuRequest::Luid(Luid(bytes)))
            .unwrap_or(GpuRequest::Ranking),
        GpuRule::Integrated => GpuRequest::Integrated,
        GpuRule::Discrete => GpuRequest::Discrete,
        GpuRule::Name if !start.gpu_name.is_empty() => GpuRequest::Named(start.gpu_name.clone()),
        GpuRule::Name => GpuRequest::Ranking,
    }
}

/// The GPU fields of `Started` for `gpu`.
pub fn report_gpu(started: &mut Started, gpu: Option<&RendererGpu>) {
    let Some(gpu) = gpu else {
        started.gpu_known = false;
        return;
    };
    started.gpu_known = true;
    started.gpu_name = gpu.name.clone();
    started.gpu_luid = gpu.luid.map(|luid| luid.0.to_vec()).unwrap_or_default();
    started.gpu_match = match gpu.matched {
        Some(true) => GpuMatch::Matched,
        Some(false) => GpuMatch::Ranked,
        None => GpuMatch::Unknown,
    } as i32;
}

/// The GPU the renderer says it runs on.
pub fn started_gpu(started: &Started) -> Option<RendererGpu> {
    started.gpu_known.then(|| RendererGpu {
        name: started.gpu_name.clone(),
        luid: <[u8; 8]>::try_from(started.gpu_luid.as_slice())
            .ok()
            .map(Luid),
        matched: match GpuMatch::try_from(started.gpu_match) {
            Ok(GpuMatch::Matched) => Some(true),
            Ok(GpuMatch::Ranked) => Some(false),
            _ => None,
        },
    })
}

/// The adapter the renderer's frames are on, which the window manager's textures must be
/// made on.
pub fn started_adapter(started: &Started) -> Option<[u8; 8]> {
    <[u8; 8]>::try_from(started.adapter_luid.as_slice()).ok()
}

/// How the renderer's frames reach the windows, for the log.
pub fn describe_frame_path(started: &Started) -> &'static str {
    match FramePath::try_from(started.frame_path) {
        Ok(FramePath::Gpu) => {
            "the graphics renderer copies the frames into the windows' textures on the GPU"
        }
        Ok(FramePath::ReadBack) => "the graphics renderer reads the frames back to memory",
        _ => {
            "the graphics renderer cannot present frames on this computer; task windows stay \
             empty"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr3_the_render_gpu_request_reaches_the_renderer_process_unchanged() {
        let luid = Luid([1, 2, 3, 4, 5, 6, 7, 8]);
        for request in [
            GpuRequest::Ranking,
            GpuRequest::Luid(luid),
            GpuRequest::Integrated,
            GpuRequest::Discrete,
            GpuRequest::Named("RTX".into()),
        ] {
            let start = start_for(&request, (5566, 5567));
            assert_eq!(request_of(&start), request);
            assert_eq!((start.command_port, start.process_pipe_port), (5566, 5567));
        }
        // A name rule without a name, or a LUID of the wrong length, asks for nothing.
        let mut start = start_for(&GpuRequest::Named(String::new()), (1, 2));
        assert_eq!(request_of(&start), GpuRequest::Ranking);
        start.gpu_rule = GpuRule::Luid as i32;
        start.gpu_luid = vec![1, 2, 3];
        assert_eq!(request_of(&start), GpuRequest::Ranking);
    }

    #[test]
    fn pr3_the_vulkan_driver_setting_and_outcome_cross_the_processes() {
        for setting in [
            VulkanDriverSetting::Auto,
            VulkanDriverSetting::Native,
            VulkanDriverSetting::Dozen,
        ] {
            let start = start_with(&GpuRequest::Ranking, setting, (5566, 5567));
            assert_eq!(vulkan_driver_of(&start), setting);
            assert_eq!(start.command_port, 5566);
        }
        // An older window manager asks for nothing: auto.
        assert_eq!(
            vulkan_driver_of(&start_for(&GpuRequest::Ranking, (1, 2))),
            VulkanDriverSetting::Auto
        );
        let mut started = Started::default();
        assert_eq!(graphics_state_of(&started), GraphicsState::Unknown);
        for (outcome, state) in [
            (DriverOutcome::Native, GraphicsState::Native("why".into())),
            (DriverOutcome::Dozen, GraphicsState::Layered("why".into())),
            (
                DriverOutcome::DozenOnCpu,
                GraphicsState::LayeredOnCpu("why".into()),
            ),
            (
                DriverOutcome::None,
                GraphicsState::Unavailable("why".into()),
            ),
        ] {
            report_vulkan_driver(&mut started, outcome, "why");
            assert_eq!(graphics_state_of(&started), state);
        }
    }

    #[test]
    fn pr3_the_renderers_gpu_reaches_the_window_managers_log() {
        let gpu = RendererGpu {
            name: "Intel(R) Arc(TM) Graphics".into(),
            luid: Some(Luid([8, 7, 6, 5, 4, 3, 2, 1])),
            matched: Some(false),
        };
        let mut started = Started::default();
        report_gpu(&mut started, Some(&gpu));
        assert_eq!(started_gpu(&started), Some(gpu));
        report_gpu(&mut started, None);
        assert_eq!(started_gpu(&started), None);
        started.adapter_luid = vec![1, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(started_adapter(&started), Some([1, 0, 0, 0, 0, 0, 0, 0]));
    }
}
