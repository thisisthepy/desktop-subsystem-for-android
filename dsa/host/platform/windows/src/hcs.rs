//! Creating, starting and stopping the Android VM through the Host Compute System (HCS).
//!
//! `computecore.dll` is loaded at run time rather than linked. On a machine without the
//! Virtual Machine Platform feature it may be missing, and a linked import would stop the
//! whole host process from starting; loaded here, its absence is an error that says which
//! feature to turn on.
//!
//! HCS only lets administrators (and the Hyper-V Administrators group) create VMs, so in
//! the product this runs inside the DSA service, as WSL's VM runs inside its service.

use std::ffi::c_void;
use std::fmt;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use windows_sys::core::{HRESULT, PCWSTR, PWSTR};
use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
use windows_sys::Win32::System::HostComputeSystem::{
    HcsEventOptionNone, HcsEventSystemExited, HCS_EVENT, HCS_EVENT_CALLBACK, HCS_EVENT_OPTIONS,
    HCS_OPERATION, HCS_OPERATION_COMPLETION, HCS_SYSTEM,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

use crate::guid::Guid;

/// `HCS_E_ACCESS_DENIED`: the caller is not an administrator or a Hyper-V administrator.
const HCS_E_ACCESS_DENIED: HRESULT = 0x8037_011B_u32 as HRESULT;

/// What went wrong in an HCS call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HcsError {
    /// `computecore.dll` or one of its functions is missing.
    Unavailable(String),
    /// An HCS call failed.
    Call {
        what: &'static str,
        hresult: HRESULT,
        detail: String,
    },
    /// The VM did not reach the state asked for in time.
    Timeout(&'static str),
}

impl fmt::Display for HcsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HcsError::Unavailable(why) => write!(
                f,
                "the Host Compute System is not available ({why}); turn on the Windows feature \
                 \"Virtual Machine Platform\" and restart"
            ),
            HcsError::Call {
                what,
                hresult,
                detail,
            } => {
                write!(f, "{what} failed (HRESULT 0x{:08x})", *hresult as u32)?;
                if *hresult == HCS_E_ACCESS_DENIED {
                    write!(
                        f,
                        ": only administrators may create VMs, so this must run in the DSA service"
                    )?;
                }
                if !detail.is_empty() {
                    write!(f, ": {detail}")?;
                }
                Ok(())
            }
            HcsError::Timeout(what) => write!(f, "{what} did not finish in time"),
        }
    }
}

impl std::error::Error for HcsError {}

type CreateOperation =
    unsafe extern "system" fn(*const c_void, HCS_OPERATION_COMPLETION) -> HCS_OPERATION;
type CloseOperation = unsafe extern "system" fn(HCS_OPERATION);
type WaitForOperationResult = unsafe extern "system" fn(HCS_OPERATION, u32, *mut PWSTR) -> HRESULT;
type CreateComputeSystem = unsafe extern "system" fn(
    PCWSTR,
    PCWSTR,
    HCS_OPERATION,
    *const c_void,
    *mut HCS_SYSTEM,
) -> HRESULT;
type SystemOperation = unsafe extern "system" fn(HCS_SYSTEM, HCS_OPERATION, PCWSTR) -> HRESULT;
type CloseComputeSystem = unsafe extern "system" fn(HCS_SYSTEM);
type GrantVmAccess = unsafe extern "system" fn(PCWSTR, PCWSTR) -> HRESULT;
type SetComputeSystemCallback = unsafe extern "system" fn(
    HCS_SYSTEM,
    HCS_EVENT_OPTIONS,
    *const c_void,
    HCS_EVENT_CALLBACK,
) -> HRESULT;

/// The `computecore.dll` functions this module uses.
struct Api {
    create_operation: CreateOperation,
    close_operation: CloseOperation,
    wait_for_operation_result: WaitForOperationResult,
    create_compute_system: CreateComputeSystem,
    start_compute_system: SystemOperation,
    shut_down_compute_system: SystemOperation,
    terminate_compute_system: SystemOperation,
    pause_compute_system: SystemOperation,
    resume_compute_system: SystemOperation,
    get_compute_system_properties: SystemOperation,
    close_compute_system: CloseComputeSystem,
    set_compute_system_callback: SetComputeSystemCallback,
    grant_vm_access: GrantVmAccess,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn api() -> Result<&'static Api, HcsError> {
    static API: OnceLock<Result<Api, HcsError>> = OnceLock::new();
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn load() -> Result<Api, HcsError> {
    let name = wide("computecore.dll");
    // SAFETY: a NUL-terminated name; only System32 is searched.
    let module = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        return Err(HcsError::Unavailable(format!(
            "computecore.dll could not be loaded: {}",
            std::io::Error::last_os_error()
        )));
    }
    macro_rules! function {
        ($name:literal, $type:ty) => {{
            // SAFETY: a NUL-terminated ASCII name on a loaded module.
            match unsafe { GetProcAddress(module, concat!($name, "\0").as_ptr()) } {
                // SAFETY: the export has this signature in ComputeCore.h.
                Some(f) => unsafe {
                    std::mem::transmute::<unsafe extern "system" fn() -> isize, $type>(f)
                },
                None => {
                    return Err(HcsError::Unavailable(format!(
                        "computecore.dll has no {}",
                        $name
                    )))
                }
            }
        }};
    }
    // The module stays loaded for the life of the process; nothing frees it.
    Ok(Api {
        create_operation: function!("HcsCreateOperation", CreateOperation),
        close_operation: function!("HcsCloseOperation", CloseOperation),
        wait_for_operation_result: function!("HcsWaitForOperationResult", WaitForOperationResult),
        create_compute_system: function!("HcsCreateComputeSystem", CreateComputeSystem),
        start_compute_system: function!("HcsStartComputeSystem", SystemOperation),
        shut_down_compute_system: function!("HcsShutDownComputeSystem", SystemOperation),
        terminate_compute_system: function!("HcsTerminateComputeSystem", SystemOperation),
        pause_compute_system: function!("HcsPauseComputeSystem", SystemOperation),
        resume_compute_system: function!("HcsResumeComputeSystem", SystemOperation),
        get_compute_system_properties: function!("HcsGetComputeSystemProperties", SystemOperation),
        close_compute_system: function!("HcsCloseComputeSystem", CloseComputeSystem),
        set_compute_system_callback: function!(
            "HcsSetComputeSystemCallback",
            SetComputeSystemCallback
        ),
        grant_vm_access: function!("HcsGrantVmAccess", GrantVmAccess),
    })
}

/// Takes ownership of a string HCS allocated.
fn take_string(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: HCS returns a NUL-terminated wide string allocated with LocalAlloc.
    let result = unsafe {
        let len = (0..).take_while(|&i| *text.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    };
    // SAFETY: freed once, as HCS documents.
    unsafe { LocalFree(text as HLOCAL) };
    result
}

fn millis(timeout: Duration) -> u32 {
    timeout.as_millis().min(u32::MAX as u128 - 1) as u32
}

/// Runs one HCS call that completes through an operation, and waits for its result.
fn run_operation(
    api: &Api,
    what: &'static str,
    timeout: Duration,
    call: impl FnOnce(HCS_OPERATION) -> HRESULT,
) -> Result<String, HcsError> {
    // SAFETY: a synchronous operation (no callback).
    let operation = unsafe { (api.create_operation)(std::ptr::null(), None) };
    if operation.is_null() {
        return Err(HcsError::Call {
            what: "HcsCreateOperation",
            hresult: -1,
            detail: String::new(),
        });
    }
    let started = call(operation);
    let result = if started < 0 {
        Err(HcsError::Call {
            what,
            hresult: started,
            detail: String::new(),
        })
    } else {
        let mut document: PWSTR = std::ptr::null_mut();
        // SAFETY: a live operation and an out pointer for the result document.
        let hresult =
            unsafe { (api.wait_for_operation_result)(operation, millis(timeout), &mut document) };
        let detail = take_string(document);
        if hresult < 0 {
            Err(HcsError::Call {
                what,
                hresult,
                detail,
            })
        } else {
            Ok(detail)
        }
    };
    // SAFETY: closed once, after its result was read.
    unsafe { (api.close_operation)(operation) };
    result
}

/// Set when HCS reports that the VM has exited.
#[derive(Default)]
struct ExitSignal {
    exited: Mutex<bool>,
    changed: Condvar,
}

unsafe extern "system" fn on_event(event: *const HCS_EVENT, context: *const c_void) {
    // SAFETY: HCS passes the event and the context pointer given at registration, a leaked
    // ExitSignal that is never freed.
    unsafe {
        if event.is_null() || context.is_null() || (*event).Type != HcsEventSystemExited {
            return;
        }
        let signal = &*(context as *const ExitSignal);
        *signal.exited.lock().unwrap_or_else(|e| e.into_inner()) = true;
        signal.changed.notify_all();
    }
}

/// A created VM. Dropping it closes the handle, which also ends the VM, because the document
/// sets `ShouldTerminateOnLastHandleClosed`.
pub struct ComputeSystem {
    api: &'static Api,
    handle: HCS_SYSTEM,
    /// Leaked on purpose: HCS may still be delivering an event while the handle closes, so
    /// the callback's context must never be freed. It is two words per VM start.
    exit: &'static ExitSignal,
}

// SAFETY: HCS handles may be used from any thread.
unsafe impl Send for ComputeSystem {}

impl ComputeSystem {
    /// Creates (but does not start) the compute system `id` from an HCS JSON document.
    pub fn create(id: &str, document: &str, timeout: Duration) -> Result<ComputeSystem, HcsError> {
        let api = api()?;
        let id_w = wide(id);
        let document_w = wide(document);
        let mut handle: HCS_SYSTEM = std::ptr::null_mut();
        run_operation(api, "HcsCreateComputeSystem", timeout, |operation| {
            // SAFETY: NUL-terminated strings and an out pointer that outlive the call.
            unsafe {
                (api.create_compute_system)(
                    id_w.as_ptr(),
                    document_w.as_ptr(),
                    operation,
                    std::ptr::null(),
                    &mut handle,
                )
            }
        })?;
        let system = ComputeSystem {
            api,
            handle,
            exit: Box::leak(Box::default()),
        };
        let context = system.exit as *const ExitSignal as *const c_void;
        // SAFETY: the context is never freed.
        let hresult = unsafe {
            (api.set_compute_system_callback)(handle, HcsEventOptionNone, context, Some(on_event))
        };
        if hresult < 0 {
            return Err(HcsError::Call {
                what: "HcsSetComputeSystemCallback",
                hresult,
                detail: String::new(),
            });
        }
        Ok(system)
    }

    fn system_operation(
        &self,
        what: &'static str,
        function: SystemOperation,
        options: Option<&str>,
        timeout: Duration,
    ) -> Result<String, HcsError> {
        let options_w = options.map(wide);
        let options_ptr = options_w.as_ref().map_or(std::ptr::null(), |w| w.as_ptr());
        run_operation(self.api, what, timeout, |operation| {
            // SAFETY: a live handle, a live operation and an optional NUL-terminated string.
            unsafe { function(self.handle, operation, options_ptr) }
        })
    }

    /// Boots the VM.
    pub fn start(&self, timeout: Duration) -> Result<(), HcsError> {
        self.system_operation(
            "HcsStartComputeSystem",
            self.api.start_compute_system,
            None,
            timeout,
        )
        .map(drop)
    }

    /// Asks the guest to shut down. Returns once the request is delivered, not when the VM
    /// has exited; see [`wait_exited`](Self::wait_exited).
    pub fn request_shutdown(&self, timeout: Duration) -> Result<(), HcsError> {
        self.system_operation(
            "HcsShutDownComputeSystem",
            self.api.shut_down_compute_system,
            None,
            timeout,
        )
        .map(drop)
    }

    /// Stops the VM at once, without asking the guest.
    pub fn terminate(&self, timeout: Duration) -> Result<(), HcsError> {
        self.system_operation(
            "HcsTerminateComputeSystem",
            self.api.terminate_compute_system,
            None,
            timeout,
        )
        .map(drop)
    }

    /// Pauses the VM: its virtual processors stop where they are and its memory is kept.
    pub fn pause(&self, timeout: Duration) -> Result<(), HcsError> {
        self.system_operation(
            "HcsPauseComputeSystem",
            self.api.pause_compute_system,
            None,
            timeout,
        )
        .map(drop)
    }

    /// Lets a paused VM run again.
    pub fn resume(&self, timeout: Duration) -> Result<(), HcsError> {
        self.system_operation(
            "HcsResumeComputeSystem",
            self.api.resume_compute_system,
            None,
            timeout,
        )
        .map(drop)
    }

    /// Waits at most `timeout` for the VM to exit. Returns whether it has.
    pub fn wait_exited(&self, timeout: Duration) -> bool {
        let exited = self.exit.exited.lock().unwrap_or_else(|e| e.into_inner());
        let (exited, _) = self
            .exit
            .changed
            .wait_timeout_while(exited, timeout, |exited| !*exited)
            .unwrap_or_else(|e| e.into_inner());
        *exited
    }

    /// The VM's runtime ID, which its Hyper-V sockets are addressed by.
    pub fn runtime_id(&self, timeout: Duration) -> Result<Guid, HcsError> {
        let properties = self.system_operation(
            "HcsGetComputeSystemProperties",
            self.api.get_compute_system_properties,
            Some("{}"),
            timeout,
        )?;
        crate::vm::runtime_id_from_properties(&properties).ok_or(HcsError::Call {
            what: "HcsGetComputeSystemProperties",
            hresult: 0,
            detail: format!("no RuntimeId in {properties}"),
        })
    }
}

impl Drop for ComputeSystem {
    fn drop(&mut self) {
        // SAFETY: closed once.
        unsafe { (self.api.close_compute_system)(self.handle) };
    }
}

/// Lets the VM of compute system `id` open the file at `path`, by adding that VM's own
/// account to the file's access list. A user's disk in their profile is closed to everyone
/// else, VMs included, so the VM is given this one file and nothing around it, as WSL does
/// for each distribution's disk.
pub fn grant_vm_access(id: &str, path: &std::path::Path) -> Result<(), HcsError> {
    let api = api()?;
    let (id_w, path_w) = (wide(id), wide(&path.to_string_lossy()));
    // SAFETY: two NUL-terminated strings.
    let hresult = unsafe { (api.grant_vm_access)(id_w.as_ptr(), path_w.as_ptr()) };
    if hresult < 0 {
        return Err(HcsError::Call {
            what: "HcsGrantVmAccess",
            hresult,
            detail: path.display().to_string(),
        });
    }
    Ok(())
}
