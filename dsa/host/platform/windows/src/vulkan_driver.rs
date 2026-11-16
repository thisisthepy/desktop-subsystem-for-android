//! Which Vulkan implementation the graphics renderer runs on.
//!
//! The renderer (gfxstream in `dsa-gfx.exe`) runs the guest's Vulkan calls on the host's
//! Vulkan. Most GPUs have a native Windows Vulkan driver. Some have none: Intel's Haswell and
//! Broadwell, older Snapdragons, GPU partitions of virtual machines, computers with no GPU at
//! all. For those the renderer uses Dozen, Mesa's Vulkan on Direct3D 12, from Microsoft's
//! "OpenCL, OpenGL, and Vulkan Compatibility Pack" (the package `Microsoft.D3DMappingLayers`,
//! from the Microsoft Store or winget). DSA ships neither Dozen nor the DXIL validator
//! (`dxil.dll`) Dozen needs, which is partly closed source; the pack carries both and Microsoft
//! updates it.
//!
//! The rules ([`plan`]):
//!
//! - `auto` (the default): a native device when there is one, and Dozen only when there is
//!   none. Where one GPU shows both (the Vulkan loader lists the pack's Dozen beside the
//!   native driver), the native one is used, and Dozen is hidden from the renderer.
//! - `native`: never Dozen.
//! - `dozen`: only the pack's Dozen, to test and compare it on a GPU that has a native driver.
//!
//! A GPU without a Direct3D 12 driver (Intel disabled Direct3D 12 on Haswell in driver
//! 15.40.44.5107) leaves Dozen only Windows' software Direct3D 12 device, WARP: the CPU draws
//! everything. DSA then works, and the user is told once that games will be slow.
//!
//! The choice reaches only the renderer's process, through the Vulkan loader's environment
//! variables ([`DriverPlan::env`]); the computer's Vulkan setup is never changed. When nothing
//! is usable, the plan says what to install, and the window manager tells the user and the
//! service, which then does not start Android.

use std::path::{Path, PathBuf};

use crate::render_gpu::{GpuRequest, Luid};

/// `VK_DRIVER_ID_MESA_DOZEN`.
pub const DOZEN_DRIVER_ID: u32 = 23;

/// What Dozen calls each of its devices: "Microsoft Direct3D12 (<the adapter's name>)".
pub const DOZEN_NAME_PREFIX: &str = "Microsoft Direct3D12 (";

/// The package family of Microsoft's OpenCL, OpenGL, and Vulkan Compatibility Pack.
pub const PACK_FAMILY: &str = "Microsoft.D3DMappingLayers_8wekyb3d8bbwe";

/// Where the user gets the pack: the Microsoft Store page, and winget.
pub const PACK_STORE_LINK: &str = "ms-windows-store://pdp/?ProductId=9NQPSL29BFFF";
pub const PACK_WEB_LINK: &str = "https://apps.microsoft.com/detail/9NQPSL29BFFF";
pub const PACK_WINGET_ID: &str = "Microsoft.OpenCLGLVulkanCompatibilityPack";

/// `VkPhysicalDeviceType`.
pub const DEVICE_TYPE_INTEGRATED: u32 = 1;
pub const DEVICE_TYPE_DISCRETE: u32 = 2;
pub const DEVICE_TYPE_CPU: u32 = 4;

/// The gfxstream feature that allocates guest memory from host memory it imports
/// (`VK_EXT_external_memory_host`), off where the device cannot import it.
pub const NO_HOST_MEMORY_IMPORT: &str = "VulkanAllocateHostMemory:disabled";

/// The user's `vulkan_driver` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VulkanDriverSetting {
    #[default]
    Auto,
    Native,
    Dozen,
}

impl VulkanDriverSetting {
    /// Reads the setting as the service keeps it. Anything else reads as `auto`, the default.
    pub fn parse(value: &str) -> VulkanDriverSetting {
        match value.trim().to_ascii_lowercase().as_str() {
            "native" => VulkanDriverSetting::Native,
            "dozen" => VulkanDriverSetting::Dozen,
            _ => VulkanDriverSetting::Auto,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            VulkanDriverSetting::Auto => "auto",
            VulkanDriverSetting::Native => "native",
            VulkanDriverSetting::Dozen => "dozen",
        }
    }
}

/// One Vulkan physical device the loader lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VulkanDevice {
    /// `VkPhysicalDeviceProperties::deviceName`.
    pub name: String,
    /// `VkPhysicalDeviceIDProperties::deviceLUID`, when valid.
    pub luid: Option<Luid>,
    /// `VkPhysicalDeviceDriverProperties::driverID`, when the device reports it.
    pub driver_id: Option<u32>,
    /// `VkPhysicalDeviceLayeredDriverPropertiesMSFT::underlyingAPI` is Direct3D 12.
    pub layered_on_d3d12: bool,
    /// `VkPhysicalDeviceType`.
    pub device_type: u32,
    /// Whether the device has `VK_EXT_external_memory_host`.
    pub external_memory_host: bool,
}

impl VulkanDevice {
    /// Whether it is Dozen.
    pub fn is_dozen(&self) -> bool {
        self.driver_id == Some(DOZEN_DRIVER_ID) || self.name.starts_with(DOZEN_NAME_PREFIX)
    }

    /// Whether the CPU draws for it.
    pub fn on_cpu(&self) -> bool {
        self.device_type == DEVICE_TYPE_CPU
    }

    /// Whether it is a native driver of a GPU: not Dozen, not another driver layered on
    /// Direct3D 12, and not a CPU implementation (which cannot take the task windows'
    /// Direct3D 11 textures).
    pub fn is_native(&self) -> bool {
        !self.is_dozen() && !self.layered_on_d3d12 && !self.on_cpu()
    }

    fn describe(&self) -> String {
        match self.luid {
            Some(luid) => format!("{} (LUID {luid})", self.name),
            None => self.name.clone(),
        }
    }
}

/// The pack's Dozen driver manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackIcd {
    /// The ICD JSON that names Dozen's library.
    pub manifest: PathBuf,
}

impl PackIcd {
    /// The manifest's file name, which the loader's driver filters match.
    pub fn file_name(&self) -> String {
        // Split on both separators: the manifest path is a Windows path, whichever system
        // reads it (a host that is not Windows only runs the tests).
        let path = self.manifest.to_string_lossy();
        path.rsplit(['\\', '/'])
            .next()
            .unwrap_or_default()
            .to_string()
    }
}

/// What the renderer found before it chose.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Found {
    /// The devices the loader lists: the system's drivers, and the pack's Dozen when the pack
    /// is installed (whether the loader found it itself or it was added for the look).
    pub devices: Vec<VulkanDevice>,
    /// Whether the loader lists Dozen with the system's drivers alone, without being told of
    /// the pack: then Dozen must be hidden to keep it from the renderer.
    pub system_lists_dozen: bool,
    /// The pack's Dozen manifest, when the pack is installed.
    pub pack: Option<PackIcd>,
    /// A Vulkan loader found outside the system's search (in the pack), when the system has
    /// none.
    pub loader: Option<PathBuf>,
    /// Why the devices could not be listed at all (no Vulkan loader, or it failed).
    pub failure: Option<String>,
    /// The drivers the renderer's own environment already names (`VK_DRIVER_FILES` or
    /// `VK_ICD_FILENAMES`, as a test running the renderer on a software Vulkan sets them):
    /// they are taken as they are, whatever the setting says, unless the process runs
    /// elevated ([`Found::elevated`]), when the loader ignores them.
    pub driver_override: Option<String>,
    /// Whether the process runs elevated (at high integrity or above, as an administrator's
    /// elevated program or SYSTEM does). The Khronos Vulkan loader then ignores the variables
    /// that name driver and layer files (`VK_DRIVER_FILES`, `VK_ICD_FILENAMES`,
    /// `VK_ADD_DRIVER_FILES` and the layer paths), so that an elevated program does not load
    /// a library a normal user's environment points at. It lists only the drivers registered
    /// with Windows: `HKLM\SOFTWARE\Khronos\Vulkan\Drivers`, the display drivers' own entries,
    /// and the Compatibility Pack's package registration.
    pub elevated: bool,
    /// Whether every driver manifest [`Found::driver_override`] names is also an enabled
    /// driver under `HKLM\SOFTWARE\Khronos\Vulkan\Drivers`. An elevated process's loader lists
    /// those drivers itself, so naming them still holds, as on a test machine that runs
    /// elevated and registers its software Vulkan there.
    pub override_registered: bool,
}

/// What an elevated process is told: why the environment does not reach the loader.
fn elevated_note(drivers: &str) -> String {
    format!(
        "this process runs elevated (as an administrator), so the Vulkan loader ignores \
         VK_DRIVER_FILES and VK_ICD_FILENAMES ({drivers}) and lists only the drivers \
         registered with Windows"
    )
}

/// Which Vulkan the renderer runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverOutcome {
    Native,
    Dozen,
    DozenOnCpu,
    /// Nothing usable: the renderer renders nothing.
    None,
}

/// What the renderer does: the environment it starts gfxstream with, the request that replaces
/// the render GPU setting's when one is needed, the gfxstream features it turns off, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverPlan {
    pub outcome: DriverOutcome,
    /// The device the renderer is expected to run on.
    pub device: Option<VulkanDevice>,
    /// Variables set in the renderer's process before gfxstream loads the Vulkan loader.
    pub env: Vec<(String, String)>,
    /// A request that replaces the render GPU setting's, when the devices need one.
    pub request: Option<GpuRequest>,
    /// gfxstream renderer features (`Name:disabled`).
    pub features: Vec<String>,
    /// Why, in a sentence for the log and the user.
    pub reason: String,
}

/// What the user is told to do when nothing is usable.
pub fn install_guidance() -> String {
    format!(
        "install Microsoft's \"OpenCL, OpenGL, and Vulkan Compatibility Pack\" from the \
         Microsoft Store ({PACK_WEB_LINK}) or with `winget install {PACK_WINGET_ID}`, then \
         sign out and in again, or update the GPU driver to one with Vulkan; `dsa gpus` shows \
         what DSA finds"
    )
}

/// The device `request` would pick among `devices`, as gfxstream's rules pick it: the one the
/// request names when there is one, else gfxstream's ranking (a discrete GPU, then an
/// integrated one, then anything else, the CPU last).
fn expected(devices: &[&VulkanDevice], request: &GpuRequest) -> Option<VulkanDevice> {
    let named = match request {
        GpuRequest::Luid(luid) => devices.iter().find(|d| d.luid == Some(*luid)),
        GpuRequest::Integrated => devices
            .iter()
            .find(|d| d.device_type == DEVICE_TYPE_INTEGRATED),
        GpuRequest::Discrete => devices
            .iter()
            .find(|d| d.device_type == DEVICE_TYPE_DISCRETE),
        GpuRequest::Named(name) => {
            let name = name.to_lowercase();
            devices
                .iter()
                .find(|d| d.name.to_lowercase().contains(&name))
        }
        GpuRequest::Ranking => None,
    };
    let rank = |d: &VulkanDevice| match d.device_type {
        DEVICE_TYPE_DISCRETE => 3,
        DEVICE_TYPE_INTEGRATED => 2,
        DEVICE_TYPE_CPU => 0,
        _ => 1,
    };
    // Of devices that rank the same, the first the loader lists.
    named
        .or_else(|| devices.iter().rev().max_by_key(|d| rank(d)))
        .map(|d| (*d).clone())
}

/// The native device a request that cannot be trusted to skip Dozen is pinned to: the
/// native one it would pick, by its LUID. `None` when the request already names a LUID a
/// native device has (the loader orders a native device before Dozen on the same GPU) or no
/// native device has a LUID.
pub fn native_pin(request: &GpuRequest, devices: &[VulkanDevice]) -> Option<GpuRequest> {
    let natives: Vec<&VulkanDevice> = devices.iter().filter(|d| d.is_native()).collect();
    if let GpuRequest::Luid(luid) = request {
        if natives.iter().any(|d| d.luid == Some(*luid)) {
            return None;
        }
    }
    expected(&natives, request)
        .and_then(|device| device.luid)
        .map(GpuRequest::Luid)
}

/// Chooses the Vulkan implementation for `setting` from what was `found`, for the render GPU
/// `request`.
pub fn plan(setting: VulkanDriverSetting, found: &Found, request: &GpuRequest) -> DriverPlan {
    let mut plan = choose(setting, found, request);
    if found.elevated && !found.override_registered {
        if let Some(drivers) = &found.driver_override {
            plan.reason = format!("{}; {}", elevated_note(drivers), plan.reason);
        }
    }
    plan
}

fn choose(setting: VulkanDriverSetting, found: &Found, request: &GpuRequest) -> DriverPlan {
    let natives: Vec<&VulkanDevice> = found.devices.iter().filter(|d| d.is_native()).collect();
    let dozens: Vec<&VulkanDevice> = found.devices.iter().filter(|d| d.is_dozen()).collect();
    let mut env = Vec::new();
    if let Some(loader) = &found.loader {
        env.push((
            "ANDROID_EMU_VK_LOADER_PATH".to_string(),
            loader.display().to_string(),
        ));
    }
    let none = |reason: String| DriverPlan {
        outcome: DriverOutcome::None,
        device: None,
        env: Vec::new(),
        request: None,
        features: Vec::new(),
        reason,
    };
    let listed = || {
        if found.devices.is_empty() {
            "the Vulkan loader lists no device".to_string()
        } else {
            format!(
                "the Vulkan loader lists {}",
                found
                    .devices
                    .iter()
                    .map(VulkanDevice::describe)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    };

    // An elevated process's loader ignores the drivers the environment names: what it lists
    // is judged like any other list below, unless those drivers are registered with Windows,
    // when the loader lists them all the same.
    if let (Some(drivers), false) = (
        &found.driver_override,
        found.elevated && !found.override_registered,
    ) {
        let all: Vec<&VulkanDevice> = found.devices.iter().collect();
        let Some(device) = expected(&all, request) else {
            return none(format!(
                "the environment names the Vulkan drivers ({drivers}), and {}",
                match &found.failure {
                    Some(failure) => failure.clone(),
                    None => listed(),
                }
            ));
        };
        return DriverPlan {
            outcome: match (device.is_dozen(), device.on_cpu()) {
                (true, true) => DriverOutcome::DozenOnCpu,
                (true, false) => DriverOutcome::Dozen,
                _ => DriverOutcome::Native,
            },
            reason: format!(
                "the environment names the Vulkan drivers ({drivers}), so the Vulkan driver \
                 setting {} is not applied: rendering on {}",
                setting.name(),
                device.describe()
            ),
            features: if device.is_dozen() && !device.external_memory_host {
                vec![NO_HOST_MEMORY_IMPORT.to_string()]
            } else {
                Vec::new()
            },
            device: Some(device),
            env,
            request: None,
        };
    }

    let use_native = match setting {
        VulkanDriverSetting::Native => true,
        VulkanDriverSetting::Auto => !natives.is_empty(),
        VulkanDriverSetting::Dozen => false,
    };
    if use_native {
        let Some(device) = expected(&natives, request) else {
            let why = match &found.failure {
                Some(failure) => failure.clone(),
                None => listed(),
            };
            return none(format!(
                "the Vulkan driver setting is native, and no GPU has a native Vulkan driver \
                 ({why}); set it to auto to use Dozen, or update the GPU driver"
            ));
        };
        let mut request_override = None;
        if found.system_lists_dozen {
            // The loader lists the pack's Dozen beside the native drivers: hidden, so that no
            // rule of the render GPU setting picks it. The filter names the manifest's file.
            let filter = found
                .pack
                .as_ref()
                .map(PackIcd::file_name)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "*dzn*".to_string());
            env.push(("VK_LOADER_DRIVERS_DISABLE".to_string(), filter));
            // A loader older than that filter still lists Dozen; pinning the native device's
            // LUID keeps the renderer off it, since the loader orders a native device first.
            request_override = native_pin(request, &found.devices);
        }
        let others = if dozens.is_empty() {
            String::new()
        } else {
            "; Dozen is hidden from the renderer, since the native driver comes first".into()
        };
        return DriverPlan {
            outcome: DriverOutcome::Native,
            reason: format!(
                "Vulkan driver setting {}: rendering on {} through its native Vulkan driver{others}",
                setting.name(),
                device.describe()
            ),
            device: Some(device),
            env,
            request: request_override,
            features: Vec::new(),
        };
    }

    // Dozen: forced, or no native device.
    let Some(device) = expected(&dozens, request) else {
        let what = match setting {
            VulkanDriverSetting::Dozen => "the Vulkan driver setting is dozen, but",
            _ => "no GPU has a Vulkan driver DSA can use:",
        };
        if let (true, None, Some(pack)) = (found.elevated, &found.failure, &found.pack) {
            // The pack's Dozen reaches the loader only through VK_ADD_DRIVER_FILES or
            // VK_DRIVER_FILES when the loader does not find it itself, and an elevated
            // process's loader ignores both. Installing the pack again would not help.
            return none(format!(
                "{what} {}; the Compatibility Pack is installed ({}), but this process runs \
                 elevated (as an administrator), so the Vulkan loader ignores \
                 VK_ADD_DRIVER_FILES and VK_DRIVER_FILES, and Dozen cannot be added through \
                 the environment; the loader does not find the pack's Dozen by itself (it is \
                 registered neither under HKLM\\SOFTWARE\\Khronos\\Vulkan\\Drivers nor through \
                 the pack's own package registration); run DSA without administrator rights",
                listed(),
                pack.manifest.display()
            ));
        }
        let why = match (&found.failure, &found.pack) {
            (Some(failure), _) => failure.clone(),
            (None, None) => format!(
                "{}, and the Compatibility Pack that carries Dozen (Vulkan on Direct3D 12) is \
                 not installed",
                listed()
            ),
            (None, Some(pack)) => format!(
                "{}; the Compatibility Pack is installed ({}), but its Dozen found no \
                 Direct3D 12 device",
                listed(),
                pack.manifest.display()
            ),
        };
        return none(format!("{what} {why}; {}", install_guidance()));
    };
    let mut request_override = None;
    match &found.pack {
        Some(pack) if !found.elevated => {
            // Only the pack's Dozen: every other driver is out of the renderer's sight.
            let manifest = pack.manifest.display().to_string();
            env.push(("VK_DRIVER_FILES".to_string(), manifest.clone()));
            env.push(("VK_ICD_FILENAMES".to_string(), manifest));
        }
        _ if !natives.is_empty() => {
            // A Dozen the system lists beside native drivers, without the pack or in an
            // elevated process (whose loader ignores VK_DRIVER_FILES, so the other drivers
            // cannot be hidden): only its name tells it apart.
            request_override = Some(GpuRequest::Named(match device.luid {
                Some(_) => device.name.clone(),
                None => DOZEN_NAME_PREFIX.to_string(),
            }));
        }
        _ => {}
    }
    let features = if dozens.iter().any(|d| !d.external_memory_host) {
        vec![NO_HOST_MEMORY_IMPORT.to_string()]
    } else {
        Vec::new()
    };
    let (outcome, why) = if device.on_cpu() {
        (
            DriverOutcome::DozenOnCpu,
            "no GPU has a Direct3D 12 driver, so Dozen runs on Windows' software Direct3D 12 \
             device and the CPU draws everything; games will be slow",
        )
    } else if setting == VulkanDriverSetting::Dozen {
        (
            DriverOutcome::Dozen,
            "the Vulkan driver setting asks for Dozen",
        )
    } else {
        (DriverOutcome::Dozen, "no GPU has a native Vulkan driver")
    };
    DriverPlan {
        outcome,
        reason: format!(
            "Vulkan driver setting {}: rendering on {} through Dozen (Vulkan on Direct3D 12): \
             {why}",
            setting.name(),
            device.describe()
        ),
        device: Some(device),
        env,
        request: request_override,
        features,
    }
}

/// What the renderer runs on once gfxstream has chosen its device (`gpu_name`, `gpu_luid`):
/// the plan's outcome, corrected by the device it actually took.
pub fn outcome_for(
    plan: &DriverPlan,
    found: &Found,
    gpu_name: Option<&str>,
    gpu_luid: Option<Luid>,
) -> DriverOutcome {
    if plan.outcome == DriverOutcome::None {
        return DriverOutcome::None;
    }
    let Some(name) = gpu_name else {
        return plan.outcome;
    };
    let taken = found
        .devices
        .iter()
        .find(|d| d.name == name && (gpu_luid.is_none() || d.luid == gpu_luid))
        .or_else(|| found.devices.iter().find(|d| d.name == name));
    match taken {
        Some(device) if device.is_dozen() && device.on_cpu() => DriverOutcome::DozenOnCpu,
        Some(device) if device.is_dozen() => DriverOutcome::Dozen,
        Some(_) => DriverOutcome::Native,
        None => plan.outcome,
    }
}

/// Finds the pack's Dozen manifest among `files` (each a JSON file's path and text): the
/// driver manifest whose library is Dozen's (`vulkan_dzn.dll`, or any library with `dzn` in
/// its name).
pub fn dozen_manifest_among(files: &[(PathBuf, String)]) -> Option<PackIcd> {
    files.iter().find_map(|(path, text)| {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let library = value.get("ICD")?.get("library_path")?.as_str()?;
        let file = Path::new(&library.replace('\\', "/"))
            .file_name()?
            .to_string_lossy()
            .to_lowercase();
        file.contains("dzn").then(|| PackIcd {
            manifest: path.clone(),
        })
    })
}

/// The folders of the pack to look in, under its install folder: the folder itself and the
/// one for this computer's architecture.
pub fn pack_folders(root: &Path) -> Vec<PathBuf> {
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    vec![
        root.to_path_buf(),
        root.join(arch),
        root.join("vulkan"),
        root.join(arch).join("vulkan"),
    ]
}

/// The pack's Dozen manifest under `root`, its install folder.
pub fn find_pack_manifest(root: &Path) -> Option<PackIcd> {
    let mut files = Vec::new();
    for dir in pack_folders(root) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_json = path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
            if !is_json {
                continue;
            }
            // A driver manifest is small; anything large is not one.
            if entry.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > 64 * 1024 {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                files.push((path, text));
            }
        }
    }
    dozen_manifest_among(&files)
}

/// A Vulkan loader in the pack, for a computer whose system has none.
pub fn pack_loader(root: &Path) -> Option<PathBuf> {
    pack_folders(root)
        .into_iter()
        .map(|dir| dir.join("vulkan-1.dll"))
        .find(|path| path.is_file())
}

#[cfg(windows)]
pub use probe::{find_compatibility_pack, look, look_with, runs_elevated};

#[cfg(windows)]
mod probe {
    //! Listing the Vulkan devices through the Vulkan loader, and finding the pack.

    use std::ffi::{c_char, c_void};
    use std::path::{Path, PathBuf};

    use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    use super::{find_pack_manifest, pack_loader, Found, PackIcd, VulkanDevice, PACK_FAMILY};
    use crate::render_gpu::Luid;

    type VkInstance = *mut c_void;
    type VkPhysicalDevice = *mut c_void;
    type VkResult = i32;
    type PfnVoid = Option<unsafe extern "system" fn()>;
    type GetInstanceProcAddr = unsafe extern "system" fn(VkInstance, *const c_char) -> PfnVoid;
    type EnumerateInstanceVersion = unsafe extern "system" fn(*mut u32) -> VkResult;
    type CreateInstance = unsafe extern "system" fn(
        *const InstanceCreateInfo,
        *const c_void,
        *mut VkInstance,
    ) -> VkResult;
    type DestroyInstance = unsafe extern "system" fn(VkInstance, *const c_void);
    type EnumeratePhysicalDevices =
        unsafe extern "system" fn(VkInstance, *mut u32, *mut VkPhysicalDevice) -> VkResult;
    type GetPhysicalDeviceProperties = unsafe extern "system" fn(VkPhysicalDevice, *mut c_void);
    type GetPhysicalDeviceProperties2 = unsafe extern "system" fn(VkPhysicalDevice, *mut c_void);
    type EnumerateDeviceExtensionProperties = unsafe extern "system" fn(
        VkPhysicalDevice,
        *const c_char,
        *mut u32,
        *mut ExtensionProperties,
    ) -> VkResult;

    const STRUCTURE_TYPE_APPLICATION_INFO: i32 = 0;
    const STRUCTURE_TYPE_INSTANCE_CREATE_INFO: i32 = 1;
    const STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2: i32 = 1_000_059_001;
    const STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES: i32 = 1_000_071_004;
    const STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES: i32 = 1_000_196_000;
    const STRUCTURE_TYPE_PHYSICAL_DEVICE_LAYERED_DRIVER_PROPERTIES_MSFT: i32 = 1_000_530_000;
    const LAYERED_DRIVER_UNDERLYING_API_D3D12_MSFT: i32 = 1;
    const fn make_version(major: u32, minor: u32) -> u32 {
        (major << 22) | (minor << 12)
    }

    #[repr(C)]
    struct ApplicationInfo {
        s_type: i32,
        p_next: *const c_void,
        application_name: *const c_char,
        application_version: u32,
        engine_name: *const c_char,
        engine_version: u32,
        api_version: u32,
    }

    #[repr(C)]
    struct InstanceCreateInfo {
        s_type: i32,
        p_next: *const c_void,
        flags: u32,
        application_info: *const ApplicationInfo,
        enabled_layer_count: u32,
        enabled_layer_names: *const *const c_char,
        enabled_extension_count: u32,
        enabled_extension_names: *const *const c_char,
    }

    #[repr(C)]
    struct ExtensionProperties {
        name: [c_char; 256],
        spec_version: u32,
    }

    #[repr(C)]
    struct IdProperties {
        s_type: i32,
        p_next: *mut c_void,
        device_uuid: [u8; 16],
        driver_uuid: [u8; 16],
        device_luid: [u8; 8],
        device_node_mask: u32,
        device_luid_valid: u32,
    }

    #[repr(C)]
    struct DriverProperties {
        s_type: i32,
        p_next: *mut c_void,
        driver_id: i32,
        driver_name: [c_char; 256],
        driver_info: [c_char; 256],
        conformance_version: [u8; 4],
    }

    #[repr(C)]
    struct LayeredDriverProperties {
        s_type: i32,
        p_next: *mut c_void,
        underlying_api: i32,
    }

    /// `VkPhysicalDeviceProperties2` with room to spare: only its head is read (the sType,
    /// pNext, then `VkPhysicalDeviceProperties`'s apiVersion, driverVersion, vendorID,
    /// deviceID, deviceType and deviceName), so the limits that follow need no description.
    #[repr(C, align(8))]
    struct Properties2 {
        bytes: [u8; 4096],
    }

    /// Where `VkPhysicalDeviceProperties` starts in `VkPhysicalDeviceProperties2`.
    const PROPERTIES_AT: usize = 16;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn c_text(bytes: &[c_char]) -> String {
        let bytes: Vec<u8> = bytes
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    struct Library(HMODULE);

    impl Drop for Library {
        fn drop(&mut self) {
            // SAFETY: a module this probe loaded, freed once.
            unsafe { FreeLibrary(self.0) };
        }
    }

    /// Lists the devices the Vulkan loader at `loader` (`vulkan-1.dll` on the system's search
    /// when `None`) sees, with the process environment as it is.
    fn list(loader: Option<&Path>) -> Result<Vec<VulkanDevice>, String> {
        let name = loader
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "vulkan-1.dll".to_string());
        let path = wide(&name);
        // SAFETY: a NUL-terminated UTF-16 path.
        let module = unsafe { LoadLibraryW(path.as_ptr()) };
        if module.is_null() {
            return Err(format!(
                "no Vulkan loader ({name}) could be loaded: {}",
                std::io::Error::last_os_error()
            ));
        }
        let library = Library(module);
        // SAFETY: the loader's exported entry point, with its declared signature.
        let gipa: GetInstanceProcAddr = unsafe {
            match GetProcAddress(library.0, c"vkGetInstanceProcAddr".as_ptr().cast()) {
                Some(f) => std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    GetInstanceProcAddr,
                >(f),
                None => return Err(format!("{name} has no vkGetInstanceProcAddr")),
            }
        };
        macro_rules! proc {
            ($instance:expr, $name:literal, $ty:ty) => {{
                // SAFETY: a Vulkan command looked up by its name, cast to its signature.
                unsafe {
                    gipa($instance, $name.as_ptr())
                        .map(|f| std::mem::transmute::<unsafe extern "system" fn(), $ty>(f))
                }
            }};
        }
        let mut api = make_version(1, 0);
        if let Some(version) = proc!(
            std::ptr::null_mut(),
            c"vkEnumerateInstanceVersion",
            EnumerateInstanceVersion
        ) {
            let mut found = 0u32;
            // SAFETY: writes one u32.
            if unsafe { version(&mut found) } == 0 && found >= make_version(1, 1) {
                api = make_version(1, 1);
            }
        }
        let create = proc!(std::ptr::null_mut(), c"vkCreateInstance", CreateInstance)
            .ok_or_else(|| format!("{name} has no vkCreateInstance"))?;
        let app = ApplicationInfo {
            s_type: STRUCTURE_TYPE_APPLICATION_INFO,
            p_next: std::ptr::null(),
            application_name: c"dsa-gfx".as_ptr(),
            application_version: 0,
            engine_name: c"dsa-gfx".as_ptr(),
            engine_version: 0,
            api_version: api,
        };
        let info = InstanceCreateInfo {
            s_type: STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            p_next: std::ptr::null(),
            flags: 0,
            application_info: &app,
            enabled_layer_count: 0,
            enabled_layer_names: std::ptr::null(),
            enabled_extension_count: 0,
            enabled_extension_names: std::ptr::null(),
        };
        let mut instance: VkInstance = std::ptr::null_mut();
        // SAFETY: valid create info, and a handle to write.
        let result = unsafe { create(&info, std::ptr::null(), &mut instance) };
        if result != 0 || instance.is_null() {
            // VK_ERROR_INCOMPATIBLE_DRIVER (-9): the loader found no driver at all.
            return if result == -9 {
                Ok(Vec::new())
            } else {
                Err(format!(
                    "the Vulkan loader could not start (VkResult {result})"
                ))
            };
        }
        let destroy = proc!(instance, c"vkDestroyInstance", DestroyInstance);
        let devices = (|| {
            let enumerate = proc!(
                instance,
                c"vkEnumeratePhysicalDevices",
                EnumeratePhysicalDevices
            )
            .ok_or("the Vulkan loader has no vkEnumeratePhysicalDevices")?;
            let properties = proc!(
                instance,
                c"vkGetPhysicalDeviceProperties",
                GetPhysicalDeviceProperties
            )
            .ok_or("the Vulkan loader has no vkGetPhysicalDeviceProperties")?;
            let properties2 = if api >= make_version(1, 1) {
                proc!(
                    instance,
                    c"vkGetPhysicalDeviceProperties2",
                    GetPhysicalDeviceProperties2
                )
            } else {
                None
            };
            let extensions = proc!(
                instance,
                c"vkEnumerateDeviceExtensionProperties",
                EnumerateDeviceExtensionProperties
            );
            let mut count = 0u32;
            // SAFETY: asks for the count only.
            unsafe { enumerate(instance, &mut count, std::ptr::null_mut()) };
            let mut handles = vec![std::ptr::null_mut(); count as usize];
            // SAFETY: room for `count` handles.
            unsafe { enumerate(instance, &mut count, handles.as_mut_ptr()) };
            handles.truncate(count as usize);
            let mut devices = Vec::new();
            for handle in handles {
                let names = extensions
                    .map(|extensions| {
                        let mut count = 0u32;
                        // SAFETY: the count, then that many records.
                        unsafe {
                            extensions(handle, std::ptr::null(), &mut count, std::ptr::null_mut())
                        };
                        let mut list: Vec<ExtensionProperties> = (0..count)
                            .map(|_| ExtensionProperties {
                                name: [0; 256],
                                spec_version: 0,
                            })
                            .collect();
                        // SAFETY: room for `count` records.
                        unsafe {
                            extensions(handle, std::ptr::null(), &mut count, list.as_mut_ptr())
                        };
                        list.truncate(count as usize);
                        list.iter().map(|e| c_text(&e.name)).collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let has = |wanted: &str| names.iter().any(|name| name == wanted);
                let mut head = Properties2 { bytes: [0; 4096] };
                // SAFETY: room for VkPhysicalDeviceProperties, whose head is read.
                unsafe { properties(handle, head.bytes[PROPERTIES_AT..].as_mut_ptr().cast()) };
                let at = |offset: usize| {
                    u32::from_le_bytes(
                        head.bytes[PROPERTIES_AT + offset..PROPERTIES_AT + offset + 4]
                            .try_into()
                            .unwrap_or([0; 4]),
                    )
                };
                let device_api = at(0);
                let device_type = at(16);
                let name_bytes: Vec<c_char> = head.bytes[PROPERTIES_AT + 20..PROPERTIES_AT + 276]
                    .iter()
                    .map(|&b| b as c_char)
                    .collect();
                let mut device = VulkanDevice {
                    name: c_text(&name_bytes),
                    device_type,
                    external_memory_host: has("VK_EXT_external_memory_host"),
                    ..VulkanDevice::default()
                };
                if let (Some(properties2), true) = (properties2, device_api >= make_version(1, 1)) {
                    let mut layered = LayeredDriverProperties {
                        s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_LAYERED_DRIVER_PROPERTIES_MSFT,
                        p_next: std::ptr::null_mut(),
                        underlying_api: 0,
                    };
                    let mut driver = DriverProperties {
                        s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES,
                        p_next: std::ptr::null_mut(),
                        driver_id: 0,
                        driver_name: [0; 256],
                        driver_info: [0; 256],
                        conformance_version: [0; 4],
                    };
                    let mut id = IdProperties {
                        s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES,
                        p_next: std::ptr::null_mut(),
                        device_uuid: [0; 16],
                        driver_uuid: [0; 16],
                        device_luid: [0; 8],
                        device_node_mask: 0,
                        device_luid_valid: 0,
                    };
                    // Only structures the device knows go in the chain.
                    let wants_driver =
                        device_api >= make_version(1, 2) || has("VK_KHR_driver_properties");
                    let wants_layered = has("VK_MSFT_layered_driver");
                    let mut next: *mut c_void = std::ptr::null_mut();
                    if wants_layered {
                        layered.p_next = next;
                        next = (&mut layered as *mut LayeredDriverProperties).cast();
                    }
                    if wants_driver {
                        driver.p_next = next;
                        next = (&mut driver as *mut DriverProperties).cast();
                    }
                    id.p_next = next;
                    let mut head2 = Properties2 { bytes: [0; 4096] };
                    head2.bytes[..4].copy_from_slice(
                        &STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2.to_le_bytes(),
                    );
                    let chain = (&mut id as *mut IdProperties).cast::<c_void>() as usize;
                    head2.bytes[8..16].copy_from_slice(&(chain as u64).to_le_bytes());
                    // SAFETY: a VkPhysicalDeviceProperties2 with a chain of structures that
                    // live until the call returns.
                    unsafe { properties2(handle, head2.bytes.as_mut_ptr().cast()) };
                    if id.device_luid_valid != 0 {
                        device.luid = Some(Luid(id.device_luid));
                    }
                    if wants_driver && driver.driver_id != 0 {
                        device.driver_id = Some(driver.driver_id as u32);
                    }
                    device.layered_on_d3d12 = wants_layered
                        && layered.underlying_api == LAYERED_DRIVER_UNDERLYING_API_D3D12_MSFT;
                }
                devices.push(device);
            }
            Ok::<_, String>(devices)
        })();
        if let Some(destroy) = destroy {
            // SAFETY: the instance this probe made, destroyed once.
            unsafe { destroy(instance, std::ptr::null()) };
        }
        devices
    }

    /// Runs `f` with `name` set to `value` in this process, then puts it back.
    fn with_variable<T>(name: &str, value: &str, f: impl FnOnce() -> T) -> T {
        let before = std::env::var_os(name);
        std::env::set_var(name, value);
        let result = f();
        match before {
            Some(before) => std::env::set_var(name, before),
            None => std::env::remove_var(name),
        }
        result
    }

    /// Whether every manifest in `drivers` (separated by `;`) is an enabled driver under
    /// `HKLM\SOFTWARE\Khronos\Vulkan\Drivers`: a DWORD named by the manifest's path, 0 when
    /// enabled. The loader reads that key even when the process runs elevated.
    fn registered_with_windows(drivers: &str) -> bool {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD,
        };
        fn wide(text: &str) -> Vec<u16> {
            text.encode_utf16().chain(std::iter::once(0)).collect()
        }
        let key = wide(r"SOFTWARE\Khronos\Vulkan\Drivers");
        let paths: Vec<&str> = drivers
            .split(';')
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .collect();
        !paths.is_empty()
            && paths.iter().all(|path| {
                let name = wide(path);
                let mut value = 1u32;
                let mut size = 4u32;
                // SAFETY: NUL-terminated names and a DWORD of the size given.
                let code = unsafe {
                    RegGetValueW(
                        HKEY_LOCAL_MACHINE,
                        key.as_ptr(),
                        name.as_ptr(),
                        RRF_RT_REG_DWORD,
                        std::ptr::null_mut(),
                        (&mut value as *mut u32).cast(),
                        &mut size,
                    )
                };
                code == ERROR_SUCCESS && value == 0
            })
    }

    /// Whether this process runs elevated as the Vulkan loader judges it: its token's
    /// integrity level is high (an administrator's elevated program) or above (SYSTEM). The
    /// Khronos loader checks exactly this (`TokenIntegrityLevel` against
    /// `SECURITY_MANDATORY_HIGH_RID`) before it reads a variable that names files to load,
    /// and ignores the variable when it holds. False when the token cannot be read, as the
    /// loader also takes it.
    pub fn runs_elevated() -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{
            GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenIntegrityLevel,
            TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        /// `SECURITY_MANDATORY_HIGH_RID`.
        const HIGH_INTEGRITY: u32 = 0x3000;
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: this process's own token, opened for query and closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return false;
        }
        // A TOKEN_MANDATORY_LABEL and the SID it points at: a SID is at most 68 bytes.
        let mut buffer = [0u64; 16];
        let mut needed = 0u32;
        // SAFETY: a buffer of the size given, aligned for the label's pointer.
        let read = unsafe {
            GetTokenInformation(
                token,
                TokenIntegrityLevel,
                buffer.as_mut_ptr().cast(),
                std::mem::size_of_val(&buffer) as u32,
                &mut needed,
            )
        };
        let level = (read != 0).then(|| {
            // SAFETY: Windows wrote a TOKEN_MANDATORY_LABEL whose SID lies in the buffer.
            unsafe {
                let label = &*(buffer.as_ptr() as *const TOKEN_MANDATORY_LABEL);
                let sid = label.Label.Sid;
                let count = *GetSidSubAuthorityCount(sid);
                if count == 0 {
                    0
                } else {
                    *GetSidSubAuthority(sid, u32::from(count) - 1)
                }
            }
        });
        // SAFETY: the token opened above, closed once.
        unsafe { CloseHandle(token) };
        level.is_some_and(|level| level >= HIGH_INTEGRITY)
    }

    /// The install folder of the Compatibility Pack for this user, when it is installed.
    fn pack_root() -> Option<PathBuf> {
        type GetPackagesByPackageFamily = unsafe extern "system" fn(
            *const u16,
            *mut u32,
            *mut *mut u16,
            *mut u32,
            *mut u16,
        ) -> i32;
        type GetPackagePathByFullName =
            unsafe extern "system" fn(*const u16, *mut u32, *mut u16) -> i32;
        const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
        let kernel = wide("kernel32.dll");
        // SAFETY: kernel32 is in every process; the functions exist from Windows 8 and are
        // looked up so an older system only finds no pack.
        unsafe {
            let module = LoadLibraryW(kernel.as_ptr());
            if module.is_null() {
                return None;
            }
            let library = Library(module);
            let by_family: GetPackagesByPackageFamily = std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                GetPackagesByPackageFamily,
            >(GetProcAddress(
                library.0,
                c"GetPackagesByPackageFamily".as_ptr().cast(),
            )?);
            let path_of: GetPackagePathByFullName = std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                GetPackagePathByFullName,
            >(GetProcAddress(
                library.0,
                c"GetPackagePathByFullName".as_ptr().cast(),
            )?);
            let family = wide(PACK_FAMILY);
            let (mut count, mut length) = (0u32, 0u32);
            let first = by_family(
                family.as_ptr(),
                &mut count,
                std::ptr::null_mut(),
                &mut length,
                std::ptr::null_mut(),
            );
            if first != ERROR_INSUFFICIENT_BUFFER || count == 0 || length == 0 {
                return None;
            }
            let mut names = vec![std::ptr::null_mut::<u16>(); count as usize];
            let mut buffer = vec![0u16; length as usize];
            if by_family(
                family.as_ptr(),
                &mut count,
                names.as_mut_ptr(),
                &mut length,
                buffer.as_mut_ptr(),
            ) != 0
            {
                return None;
            }
            // The newest comes first, as the Vulkan loader takes it.
            let name = *names.first()?;
            let mut path_length = 0u32;
            if path_of(name, &mut path_length, std::ptr::null_mut()) != ERROR_INSUFFICIENT_BUFFER {
                return None;
            }
            let mut path = vec![0u16; path_length as usize];
            if path_of(name, &mut path_length, path.as_mut_ptr()) != 0 {
                return None;
            }
            let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
            Some(PathBuf::from(String::from_utf16_lossy(&path[..end])))
        }
    }

    /// The Compatibility Pack's Dozen manifest and the folder it is installed in, when the
    /// pack is installed for this user.
    pub fn find_compatibility_pack() -> Option<(PathBuf, Option<PackIcd>)> {
        let root = pack_root()?;
        let manifest = find_pack_manifest(&root);
        Some((root, manifest))
    }

    /// Looks at what this process's Vulkan loader lists, with the pack's Dozen added when the
    /// pack is installed and the loader did not find it itself.
    pub fn look() -> Found {
        look_with(find_compatibility_pack())
    }

    /// [`look`] with the pack as found (`None` when it is not installed).
    pub fn look_with(pack: Option<(PathBuf, Option<PackIcd>)>) -> Found {
        let (root, manifest) = match pack {
            Some((root, manifest)) => (Some(root), manifest),
            None => (None, None),
        };
        let mut found = Found {
            pack: manifest.clone(),
            driver_override: ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"]
                .iter()
                .find_map(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty())),
            elevated: runs_elevated(),
            ..Found::default()
        };
        found.override_registered = found
            .driver_override
            .as_deref()
            .is_some_and(registered_with_windows);
        // Drivers the environment names already are all the loader lists; nothing is added.
        // An elevated process's loader ignores VK_ADD_DRIVER_FILES too, so the pack's Dozen
        // cannot be added for the look either: the loader lists only what it finds itself.
        let manifest = if found.driver_override.is_some() || found.elevated {
            None
        } else {
            manifest
        };
        let mut loader: Option<PathBuf> = None;
        let mut system = list(None);
        if system.is_err() {
            if let Some(in_pack) = root.as_deref().and_then(pack_loader) {
                system = list(Some(&in_pack));
                loader = Some(in_pack);
            }
        }
        match system {
            Ok(devices) => {
                found.system_lists_dozen = devices.iter().any(VulkanDevice::is_dozen);
                found.devices = devices;
            }
            Err(why) => {
                found.failure = Some(why);
                return found;
            }
        }
        if !found.system_lists_dozen {
            if let Some(manifest) = &manifest {
                let path = manifest.manifest.display().to_string();
                if let Ok(with_pack) =
                    with_variable("VK_ADD_DRIVER_FILES", &path, || list(loader.as_deref()))
                {
                    found
                        .devices
                        .extend(with_pack.into_iter().filter(VulkanDevice::is_dozen));
                }
            }
        }
        found.loader = loader;
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARC: Luid = Luid([1, 0, 0, 0, 0, 0, 0, 0]);
    const RTX: Luid = Luid([2, 0, 0, 0, 0, 0, 0, 0]);
    const HD5500: Luid = Luid([3, 0, 0, 0, 0, 0, 0, 0]);
    const WARP: Luid = Luid([9, 0, 0, 0, 0, 0, 0, 0]);

    fn native(name: &str, luid: Luid, device_type: u32) -> VulkanDevice {
        VulkanDevice {
            name: name.into(),
            luid: Some(luid),
            driver_id: Some(1),
            device_type,
            external_memory_host: true,
            ..VulkanDevice::default()
        }
    }

    fn dozen(adapter: &str, luid: Luid, device_type: u32) -> VulkanDevice {
        VulkanDevice {
            name: format!("{DOZEN_NAME_PREFIX}{adapter})"),
            luid: Some(luid),
            driver_id: Some(DOZEN_DRIVER_ID),
            layered_on_d3d12: true,
            device_type,
            external_memory_host: false,
        }
    }

    fn pack() -> PackIcd {
        PackIcd {
            manifest: PathBuf::from(r"C:\Program Files\WindowsApps\Pack\dzn_icd.x86_64.json"),
        }
    }

    fn env_of<'a>(plan: &'a DriverPlan, name: &str) -> Option<&'a str> {
        plan.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn pr3_a_native_device_is_used_and_nothing_changes_the_loader() {
        let found = Found {
            devices: vec![native(
                "Intel(R) Arc(TM) Graphics",
                ARC,
                DEVICE_TYPE_INTEGRATED,
            )],
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Auto, &found, &GpuRequest::Luid(ARC));
        assert_eq!(plan.outcome, DriverOutcome::Native);
        assert!(plan.env.is_empty(), "{:?}", plan.env);
        assert_eq!(plan.request, None);
        assert!(plan.features.is_empty());
        assert!(
            plan.reason.contains("native Vulkan driver"),
            "{}",
            plan.reason
        );
    }

    #[test]
    fn pr3_native_comes_first_when_the_same_gpu_shows_dozen_too() {
        let found = Found {
            devices: vec![
                native("Intel(R) Arc(TM) Graphics", ARC, DEVICE_TYPE_INTEGRATED),
                dozen("Intel(R) Arc(TM) Graphics", ARC, DEVICE_TYPE_INTEGRATED),
            ],
            system_lists_dozen: true,
            pack: Some(pack()),
            ..Found::default()
        };
        for request in [
            GpuRequest::Luid(ARC),
            GpuRequest::Integrated,
            GpuRequest::Ranking,
        ] {
            let plan = plan(VulkanDriverSetting::Auto, &found, &request);
            assert_eq!(plan.outcome, DriverOutcome::Native);
            assert_eq!(
                plan.device.as_ref().unwrap().name,
                "Intel(R) Arc(TM) Graphics"
            );
            assert_eq!(
                env_of(&plan, "VK_LOADER_DRIVERS_DISABLE"),
                Some("dzn_icd.x86_64.json"),
                "Dozen is hidden from the renderer"
            );
            assert_eq!(env_of(&plan, "VK_DRIVER_FILES"), None);
            // A request that does not name the native LUID is pinned to it, for a loader too
            // old for the filter.
            match request {
                GpuRequest::Luid(_) => assert_eq!(plan.request, None),
                _ => assert_eq!(plan.request, Some(GpuRequest::Luid(ARC))),
            }
            assert!(plan.reason.contains("Dozen is hidden"), "{}", plan.reason);
        }
    }

    #[test]
    fn pr3_dozen_is_used_only_when_no_gpu_has_a_native_driver() {
        let found = Found {
            devices: vec![
                dozen("Intel(R) HD Graphics 5500", HD5500, DEVICE_TYPE_INTEGRATED),
                dozen("Microsoft Basic Render Driver", WARP, DEVICE_TYPE_CPU),
            ],
            pack: Some(pack()),
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Auto, &found, &GpuRequest::Luid(HD5500));
        assert_eq!(plan.outcome, DriverOutcome::Dozen);
        assert!(plan
            .device
            .as_ref()
            .unwrap()
            .name
            .contains("HD Graphics 5500"));
        let manifest = pack().manifest.display().to_string();
        assert_eq!(env_of(&plan, "VK_DRIVER_FILES"), Some(manifest.as_str()));
        assert_eq!(env_of(&plan, "VK_ICD_FILENAMES"), Some(manifest.as_str()));
        assert_eq!(plan.features, vec![NO_HOST_MEMORY_IMPORT.to_string()]);
        assert!(
            plan.reason.contains("no GPU has a native Vulkan driver"),
            "{}",
            plan.reason
        );
        // The hardware device comes before the software one, whatever the request.
        let ranked = super::plan(VulkanDriverSetting::Auto, &found, &GpuRequest::Ranking);
        assert!(!ranked.device.unwrap().on_cpu());
    }

    #[test]
    fn pr3_dozen_on_warp_alone_says_the_cpu_draws() {
        // Haswell: Intel disabled Direct3D 12, so only Windows' software device is left.
        let found = Found {
            devices: vec![dozen(
                "Microsoft Basic Render Driver",
                WARP,
                DEVICE_TYPE_CPU,
            )],
            pack: Some(pack()),
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Auto, &found, &GpuRequest::Luid(HD5500));
        assert_eq!(plan.outcome, DriverOutcome::DozenOnCpu);
        assert!(
            plan.reason.contains("CPU draws everything"),
            "{}",
            plan.reason
        );
        assert!(
            plan.reason.contains("games will be slow"),
            "{}",
            plan.reason
        );
        assert_eq!(
            outcome_for(&plan, &found, Some(&found.devices[0].name), Some(WARP)),
            DriverOutcome::DozenOnCpu
        );
    }

    #[test]
    fn pr3_nothing_usable_says_how_to_install_the_pack() {
        for found in [
            Found::default(),
            Found {
                devices: vec![VulkanDevice {
                    name: "llvmpipe".into(),
                    device_type: DEVICE_TYPE_CPU,
                    ..VulkanDevice::default()
                }],
                ..Found::default()
            },
            Found {
                failure: Some("no Vulkan loader (vulkan-1.dll) could be loaded".into()),
                ..Found::default()
            },
        ] {
            let plan = plan(VulkanDriverSetting::Auto, &found, &GpuRequest::Ranking);
            assert_eq!(plan.outcome, DriverOutcome::None);
            assert!(plan.env.is_empty());
            assert!(plan.reason.contains(PACK_WINGET_ID), "{}", plan.reason);
            assert!(plan.reason.contains(PACK_WEB_LINK), "{}", plan.reason);
            assert!(plan.reason.contains("dsa gpus"), "{}", plan.reason);
        }
        // Installed, but Dozen found no Direct3D 12 device at all.
        let empty_pack = Found {
            pack: Some(pack()),
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Auto, &empty_pack, &GpuRequest::Ranking);
        assert_eq!(plan.outcome, DriverOutcome::None);
        assert!(
            plan.reason.contains("found no Direct3D 12 device"),
            "{}",
            plan.reason
        );
    }

    #[test]
    fn pr3_native_never_uses_dozen() {
        let only_dozen = Found {
            devices: vec![dozen(
                "Intel(R) HD Graphics 5500",
                HD5500,
                DEVICE_TYPE_INTEGRATED,
            )],
            pack: Some(pack()),
            ..Found::default()
        };
        let plan = plan(
            VulkanDriverSetting::Native,
            &only_dozen,
            &GpuRequest::Ranking,
        );
        assert_eq!(plan.outcome, DriverOutcome::None);
        assert!(plan.reason.contains("setting is native"), "{}", plan.reason);
    }

    #[test]
    fn pr3_dozen_forced_takes_only_the_packs_dozen() {
        let both = Found {
            devices: vec![
                native(
                    "NVIDIA GeForce RTX 4050 Laptop GPU",
                    RTX,
                    DEVICE_TYPE_DISCRETE,
                ),
                native("Intel(R) Arc(TM) Graphics", ARC, DEVICE_TYPE_INTEGRATED),
                dozen("Intel(R) Arc(TM) Graphics", ARC, DEVICE_TYPE_INTEGRATED),
                dozen(
                    "NVIDIA GeForce RTX 4050 Laptop GPU",
                    RTX,
                    DEVICE_TYPE_DISCRETE,
                ),
            ],
            pack: Some(pack()),
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Dozen, &both, &GpuRequest::Luid(ARC));
        assert_eq!(plan.outcome, DriverOutcome::Dozen);
        let device = plan.device.as_ref().unwrap();
        assert!(device.is_dozen() && device.luid == Some(ARC), "{device:?}");
        assert!(env_of(&plan, "VK_DRIVER_FILES").is_some());
        assert!(plan.reason.contains("asks for Dozen"), "{}", plan.reason);
        // Without the pack there is no Dozen to force.
        let natives_only = Found {
            devices: both.devices[..2].to_vec(),
            ..Found::default()
        };
        let refused = super::plan(
            VulkanDriverSetting::Dozen,
            &natives_only,
            &GpuRequest::Luid(ARC),
        );
        assert_eq!(refused.outcome, DriverOutcome::None);
        assert!(
            refused.reason.contains("setting is dozen"),
            "{}",
            refused.reason
        );
        assert!(
            refused.reason.contains("not installed"),
            "{}",
            refused.reason
        );
    }

    #[test]
    fn pr3_drivers_the_environment_names_are_taken_as_they_are() {
        // The graphics test runs the renderer on lavapipe, a CPU Vulkan, by naming it.
        let lavapipe = Found {
            devices: vec![VulkanDevice {
                name: "llvmpipe (LLVM 19.1.0, 256 bits)".into(),
                device_type: DEVICE_TYPE_CPU,
                ..VulkanDevice::default()
            }],
            driver_override: Some(r"C:\lavapipe\lvp_icd.json".into()),
            ..Found::default()
        };
        for setting in [VulkanDriverSetting::Auto, VulkanDriverSetting::Native] {
            let plan = plan(setting, &lavapipe, &GpuRequest::Ranking);
            assert_eq!(plan.outcome, DriverOutcome::Native);
            assert!(plan.env.is_empty());
            assert!(plan.reason.contains("environment names"), "{}", plan.reason);
        }
        let warp = Found {
            devices: vec![dozen(
                "Microsoft Basic Render Driver",
                WARP,
                DEVICE_TYPE_CPU,
            )],
            driver_override: Some(r"C:\mesa\dzn_icd.x86_64.json".into()),
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Auto, &warp, &GpuRequest::Ranking);
        assert_eq!(plan.outcome, DriverOutcome::DozenOnCpu);
        assert_eq!(plan.features, vec![NO_HOST_MEMORY_IMPORT.to_string()]);
        assert!(!plan.reason.contains("elevated"), "{}", plan.reason);
    }

    #[test]
    fn pr3_an_elevated_process_ignores_the_drivers_the_environment_names() {
        // An elevated terminal with lavapipe named: the loader ignores the variable and lists
        // the registered native driver, so the setting applies to what it lists.
        let found = Found {
            devices: vec![native(
                "Intel(R) Arc(TM) Graphics",
                ARC,
                DEVICE_TYPE_INTEGRATED,
            )],
            driver_override: Some(r"C:\lavapipe\lvp_icd.json".into()),
            elevated: true,
            ..Found::default()
        };
        for setting in [VulkanDriverSetting::Auto, VulkanDriverSetting::Native] {
            let plan = plan(setting, &found, &GpuRequest::Ranking);
            assert_eq!(plan.outcome, DriverOutcome::Native);
            assert_eq!(
                plan.device.as_ref().unwrap().name,
                "Intel(R) Arc(TM) Graphics"
            );
            assert!(
                !plan
                    .reason
                    .contains("the environment names the Vulkan drivers"),
                "{}",
                plan.reason
            );
            assert!(
                plan.reason.contains("runs elevated")
                    && plan.reason.contains("ignores VK_DRIVER_FILES")
                    && plan.reason.contains(r"C:\lavapipe\lvp_icd.json"),
                "{}",
                plan.reason
            );
            assert!(
                plan.reason.contains("native Vulkan driver"),
                "{}",
                plan.reason
            );
        }
        // The setting is no longer bypassed: dozen, with no Dozen listed, is refused.
        let refused = plan(VulkanDriverSetting::Dozen, &found, &GpuRequest::Ranking);
        assert_eq!(refused.outcome, DriverOutcome::None);
        assert!(
            refused.reason.contains("runs elevated"),
            "{}",
            refused.reason
        );
    }

    #[test]
    fn pr3_an_elevated_process_takes_named_drivers_that_are_registered_with_windows() {
        // The graphics test's elevated runner registers lavapipe under HKLM and names it: the
        // loader lists it, so it is taken as named, with no elevation note.
        let found = Found {
            devices: vec![VulkanDevice {
                name: "llvmpipe (LLVM 19.1.0, 256 bits)".into(),
                device_type: DEVICE_TYPE_CPU,
                ..VulkanDevice::default()
            }],
            driver_override: Some(r"C:\lavapipe\lvp_icd.json".into()),
            elevated: true,
            override_registered: true,
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Auto, &found, &GpuRequest::Ranking);
        assert_eq!(plan.outcome, DriverOutcome::Native);
        assert!(plan.reason.contains("environment names"), "{}", plan.reason);
        assert!(!plan.reason.contains("elevated"), "{}", plan.reason);
    }

    #[test]
    fn pr3_an_elevated_process_cannot_add_the_packs_dozen_through_the_environment() {
        // No native driver, the pack installed but not registered where the loader looks: the
        // loader lists nothing, and VK_ADD_DRIVER_FILES cannot change that when elevated.
        let elevated = Found {
            pack: Some(pack()),
            elevated: true,
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Auto, &elevated, &GpuRequest::Ranking);
        assert_eq!(plan.outcome, DriverOutcome::None);
        assert!(plan.env.is_empty(), "{:?}", plan.env);
        assert!(
            plan.reason
                .contains("Dozen cannot be added through the environment")
                && plan.reason.contains("runs elevated")
                && plan
                    .reason
                    .contains(r"HKLM\SOFTWARE\Khronos\Vulkan\Drivers")
                && plan.reason.contains("without administrator rights"),
            "{}",
            plan.reason
        );
        assert!(
            !plan.reason.contains("found no Direct3D 12 device"),
            "{}",
            plan.reason
        );
        // Not elevated, the same look means Dozen was added and found nothing to run on.
        let unelevated = Found {
            elevated: false,
            ..elevated
        };
        let plan = super::plan(VulkanDriverSetting::Auto, &unelevated, &GpuRequest::Ranking);
        assert!(
            plan.reason.contains("found no Direct3D 12 device"),
            "{}",
            plan.reason
        );
        assert!(!plan.reason.contains("elevated"), "{}", plan.reason);
    }

    #[test]
    fn pr3_an_elevated_process_picks_a_registered_dozen_by_its_name() {
        // The loader finds the pack's Dozen itself beside the native driver. VK_DRIVER_FILES
        // would be ignored, so forcing Dozen names its device instead of hiding the others.
        let found = Found {
            devices: vec![
                native("Intel(R) Arc(TM) Graphics", ARC, DEVICE_TYPE_INTEGRATED),
                dozen("Intel(R) Arc(TM) Graphics", ARC, DEVICE_TYPE_INTEGRATED),
            ],
            system_lists_dozen: true,
            pack: Some(pack()),
            elevated: true,
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Dozen, &found, &GpuRequest::Luid(ARC));
        assert_eq!(plan.outcome, DriverOutcome::Dozen);
        assert_eq!(env_of(&plan, "VK_DRIVER_FILES"), None);
        assert_eq!(env_of(&plan, "VK_ICD_FILENAMES"), None);
        assert_eq!(
            plan.request,
            Some(GpuRequest::Named(format!(
                "{DOZEN_NAME_PREFIX}Intel(R) Arc(TM) Graphics)"
            )))
        );
        // Not elevated, the pack's manifest is all the renderer sees, as before.
        let unelevated = Found {
            elevated: false,
            ..found
        };
        let plan = super::plan(
            VulkanDriverSetting::Dozen,
            &unelevated,
            &GpuRequest::Luid(ARC),
        );
        assert!(env_of(&plan, "VK_DRIVER_FILES").is_some());
        assert_eq!(plan.request, None);
    }

    #[test]
    fn pr3_a_loader_found_in_the_pack_reaches_gfxstream() {
        let found = Found {
            devices: vec![dozen(
                "Microsoft Basic Render Driver",
                WARP,
                DEVICE_TYPE_CPU,
            )],
            pack: Some(pack()),
            loader: Some(PathBuf::from(r"C:\Pack\vulkan-1.dll")),
            ..Found::default()
        };
        let plan = plan(VulkanDriverSetting::Auto, &found, &GpuRequest::Ranking);
        assert_eq!(
            env_of(&plan, "ANDROID_EMU_VK_LOADER_PATH"),
            Some(r"C:\Pack\vulkan-1.dll")
        );
    }

    #[test]
    fn pr3_a_device_is_told_apart_by_its_driver_and_type() {
        let arc = native("Intel(R) Arc(TM) Graphics", ARC, DEVICE_TYPE_INTEGRATED);
        assert!(arc.is_native() && !arc.is_dozen());
        let layered = dozen("Intel(R) Arc(TM) Graphics", ARC, DEVICE_TYPE_INTEGRATED);
        assert!(layered.is_dozen() && !layered.is_native());
        // An old Dozen without driver properties is known by its name.
        let unnamed = VulkanDevice {
            name: format!("{DOZEN_NAME_PREFIX}Adreno 680)"),
            ..VulkanDevice::default()
        };
        assert!(unnamed.is_dozen());
        // Another driver layered on Direct3D 12 is not native either.
        let other = VulkanDevice {
            name: "Some layer".into(),
            layered_on_d3d12: true,
            device_type: DEVICE_TYPE_DISCRETE,
            ..VulkanDevice::default()
        };
        assert!(!other.is_native() && !other.is_dozen());
    }

    #[test]
    fn pr3_the_packs_dozen_manifest_is_found_by_its_library() {
        let files = vec![
            (
                PathBuf::from(r"C:\Pack\OpenCLOn12.json"),
                r#"{"file_format_version": "1.0.0", "ICD": {"library_path": "OpenCLOn12.dll"}}"#
                    .to_string(),
            ),
            (PathBuf::from(r"C:\Pack\broken.json"), "{".to_string()),
            (
                PathBuf::from(r"C:\Pack\x64\dzn_icd.json"),
                r#"{"file_format_version": "1.0.1", "ICD": {"library_path": ".\\vulkan_dzn.dll", "api_version": "1.2.0"}}"#
                    .to_string(),
            ),
        ];
        assert_eq!(
            dozen_manifest_among(&files),
            Some(PackIcd {
                manifest: PathBuf::from(r"C:\Pack\x64\dzn_icd.json")
            })
        );
        assert_eq!(dozen_manifest_among(&files[..2]), None);
    }

    #[test]
    fn pr3_the_setting_reads_as_auto_unless_it_says_otherwise() {
        assert_eq!(
            VulkanDriverSetting::parse("native"),
            VulkanDriverSetting::Native
        );
        assert_eq!(
            VulkanDriverSetting::parse(" Dozen "),
            VulkanDriverSetting::Dozen
        );
        assert_eq!(VulkanDriverSetting::parse(""), VulkanDriverSetting::Auto);
        assert_eq!(
            VulkanDriverSetting::parse("bogus"),
            VulkanDriverSetting::Auto
        );
    }
}
