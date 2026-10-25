//! The binder service the guest's clients (the agent and the hwcomposer) reach the daemon
//! through: `io.github.thisisthepy.dsa.channel.IChannel/default`, registered with
//! servicemanager on `/dev/binder`.
//!
//! It is written over the NDK binder (`libbinder_ndk`), which a vendor process may use. The
//! library is opened at run time rather than linked: the NDK's stub library leaves out the
//! calls that register a service and run the thread pool, which only platform code links
//! against, though the device's library has them all.
//!
//! This layer only turns transactions into calls on the [`Router`] and listener callbacks
//! into one-way transactions; what goes where is decided there.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Arc, OnceLock};

use crate::aidl;
use crate::router::{Delivery, Listener, ListenerId, Peer, Router};

#[allow(non_camel_case_types)]
type binder_status_t = i32;
#[allow(non_camel_case_types)]
type binder_exception_t = i32;

#[repr(C)]
struct AIBinder {
    _private: [u8; 0],
}
#[repr(C)]
struct AIBinderClass {
    _private: [u8; 0],
}
#[repr(C)]
struct AParcel {
    _private: [u8; 0],
}
#[repr(C)]
struct AStatus {
    _private: [u8; 0],
}
#[repr(C)]
struct AIBinderDeathRecipient {
    _private: [u8; 0],
}

const STATUS_OK: binder_status_t = 0;
const STATUS_DEAD_OBJECT: binder_status_t = -32; // -EPIPE
const STATUS_UNKNOWN_TRANSACTION: binder_status_t = -74; // -EBADMSG
const EX_NONE: binder_exception_t = 0;
const EX_ILLEGAL_ARGUMENT: binder_exception_t = -3;
const EX_NULL_POINTER: binder_exception_t = -4;
const FLAG_ONEWAY: u32 = 0x01;
/// What a vendor process's generated code adds to each call: check the callee's stability
/// against the vendor's level rather than the system library's own.
const FLAG_PRIVATE_VENDOR: u32 = 0x1000_0000;
const RTLD_NOW: c_int = 2;

type OnCreate = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type OnDestroy = unsafe extern "C" fn(*mut c_void);
type OnTransact =
    unsafe extern "C" fn(*mut AIBinder, u32, *const AParcel, *mut AParcel) -> binder_status_t;
type ByteArrayAllocator = unsafe extern "C" fn(*mut c_void, i32, *mut *mut i8) -> bool;

#[link(name = "dl")]
extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

/// The `libbinder_ndk` calls the service makes.
#[allow(non_snake_case)]
struct Ndk {
    AIBinder_Class_define:
        unsafe extern "C" fn(*const c_char, OnCreate, OnDestroy, OnTransact) -> *mut AIBinderClass,
    AIBinder_new: unsafe extern "C" fn(*const AIBinderClass, *mut c_void) -> *mut AIBinder,
    AIBinder_markVintfStability: unsafe extern "C" fn(*mut AIBinder),
    AIBinder_associateClass: unsafe extern "C" fn(*mut AIBinder, *const AIBinderClass) -> bool,
    AIBinder_decStrong: unsafe extern "C" fn(*mut AIBinder),
    AIBinder_getCallingPid: unsafe extern "C" fn() -> i32,
    AIBinder_prepareTransaction:
        unsafe extern "C" fn(*mut AIBinder, *mut *mut AParcel) -> binder_status_t,
    AIBinder_transact: unsafe extern "C" fn(
        *mut AIBinder,
        u32,
        *mut *mut AParcel,
        *mut *mut AParcel,
        u32,
    ) -> binder_status_t,
    AIBinder_DeathRecipient_new:
        unsafe extern "C" fn(unsafe extern "C" fn(*mut c_void)) -> *mut AIBinderDeathRecipient,
    AIBinder_linkToDeath: unsafe extern "C" fn(
        *mut AIBinder,
        *mut AIBinderDeathRecipient,
        *mut c_void,
    ) -> binder_status_t,
    AParcel_delete: unsafe extern "C" fn(*mut AParcel),
    AParcel_readByteArray:
        unsafe extern "C" fn(*const AParcel, *mut c_void, ByteArrayAllocator) -> binder_status_t,
    AParcel_writeByteArray: unsafe extern "C" fn(*mut AParcel, *const i8, i32) -> binder_status_t,
    AParcel_readStrongBinder:
        unsafe extern "C" fn(*const AParcel, *mut *mut AIBinder) -> binder_status_t,
    AParcel_writeInt32: unsafe extern "C" fn(*mut AParcel, i32) -> binder_status_t,
    AParcel_writeString: unsafe extern "C" fn(*mut AParcel, *const c_char, i32) -> binder_status_t,
    AParcel_writeStatusHeader:
        unsafe extern "C" fn(*mut AParcel, *const AStatus) -> binder_status_t,
    AStatus_newOk: unsafe extern "C" fn() -> *mut AStatus,
    AStatus_fromExceptionCodeWithMessage:
        unsafe extern "C" fn(binder_exception_t, *const c_char) -> *mut AStatus,
    AStatus_delete: unsafe extern "C" fn(*mut AStatus),
    AServiceManager_addService:
        unsafe extern "C" fn(*mut AIBinder, *const c_char) -> binder_exception_t,
    ABinderProcess_setThreadPoolMaxThreadCount: unsafe extern "C" fn(u32) -> bool,
    ABinderProcess_startThreadPool: unsafe extern "C" fn(),
}

fn last_dl_error() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated string valid until the next dl call.
    let text = unsafe { dlerror() };
    if text.is_null() {
        "unknown error".to_string()
    } else {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Ndk {
    // Each symbol becomes the type of the field it fills, which the macro cannot name.
    #[allow(clippy::missing_transmute_annotations)]
    fn open() -> Result<Ndk, String> {
        // SAFETY: a constant NUL-terminated name.
        let library = unsafe { dlopen(c"libbinder_ndk.so".as_ptr(), RTLD_NOW) };
        if library.is_null() {
            return Err(format!(
                "could not open libbinder_ndk.so: {}",
                last_dl_error()
            ));
        }
        macro_rules! symbols {
            ($($name:ident),* $(,)?) => {
                Ndk {
                    $($name: {
                        let symbol = concat!(stringify!($name), "\0");
                        // SAFETY: a NUL-terminated name and a handle dlopen returned.
                        let address = unsafe { dlsym(library, symbol.as_ptr().cast()) };
                        if address.is_null() {
                            return Err(format!(
                                "libbinder_ndk.so has no {}: {}",
                                stringify!($name),
                                last_dl_error()
                            ));
                        }
                        // SAFETY: the symbol is the NDK function of that name, whose C
                        // signature the field's type spells out.
                        unsafe { std::mem::transmute::<*mut c_void, _>(address) }
                    },)*
                }
            };
        }
        Ok(symbols!(
            AIBinder_Class_define,
            AIBinder_new,
            AIBinder_markVintfStability,
            AIBinder_associateClass,
            AIBinder_decStrong,
            AIBinder_getCallingPid,
            AIBinder_prepareTransaction,
            AIBinder_transact,
            AIBinder_DeathRecipient_new,
            AIBinder_linkToDeath,
            AParcel_delete,
            AParcel_readByteArray,
            AParcel_writeByteArray,
            AParcel_readStrongBinder,
            AParcel_writeInt32,
            AParcel_writeString,
            AParcel_writeStatusHeader,
            AStatus_newOk,
            AStatus_fromExceptionCodeWithMessage,
            AStatus_delete,
            AServiceManager_addService,
            ABinderProcess_setThreadPoolMaxThreadCount,
            ABinderProcess_startThreadPool,
        ))
    }
}

/// Everything the binder callbacks reach. Set once, when the service starts.
struct Service {
    ndk: Ndk,
    router: Arc<Router>,
    listener_class: *mut AIBinderClass,
    death: *mut AIBinderDeathRecipient,
}

// SAFETY: the class and the death recipient are created once and never freed; libbinder_ndk
// allows them to be used from any thread.
unsafe impl Send for Service {}
unsafe impl Sync for Service {}

static SERVICE: OnceLock<Service> = OnceLock::new();

/// How many binder threads take the clients' calls. The clients are the agent and the
/// hwcomposer, and no call blocks for long.
const THREADS: u32 = 4;

/// Opens the NDK binder, registers the service with servicemanager and starts the threads
/// that take calls. Frames the clients send go to `router`; the caller hands the host's
/// frames to it.
pub fn serve(router: Arc<Router>) -> Result<(), String> {
    if SERVICE.get().is_some() {
        return Err("the binder service is already running".to_string());
    }
    let ndk = Ndk::open()?;
    let channel_descriptor = CString::new(aidl::CHANNEL_DESCRIPTOR).unwrap();
    let listener_descriptor = CString::new(aidl::LISTENER_DESCRIPTOR).unwrap();
    // SAFETY: valid function pointers; the descriptors are leaked below, since a class keeps
    // its descriptor for the life of the process.
    let (channel_class, listener_class, death) = unsafe {
        let channel_class = (ndk.AIBinder_Class_define)(
            channel_descriptor.into_raw(),
            on_create,
            on_destroy,
            channel_on_transact,
        );
        let listener_class = (ndk.AIBinder_Class_define)(
            listener_descriptor.into_raw(),
            on_create,
            on_destroy,
            listener_on_transact,
        );
        let death = (ndk.AIBinder_DeathRecipient_new)(on_listener_died);
        (channel_class, listener_class, death)
    };
    if channel_class.is_null() || listener_class.is_null() || death.is_null() {
        return Err("libbinder_ndk could not define the channel's classes".to_string());
    }
    let service = Service {
        ndk,
        router,
        listener_class,
        death,
    };
    if SERVICE.set(service).is_err() {
        return Err("the binder service is already running".to_string());
    }
    let service = SERVICE.get().unwrap();
    let ndk = &service.ndk;
    let instance = CString::new(aidl::INSTANCE).unwrap();
    // SAFETY: the class is valid; the binder is handed to servicemanager, which keeps it.
    unsafe {
        let binder = (ndk.AIBinder_new)(channel_class, std::ptr::null_mut());
        if binder.is_null() {
            return Err("libbinder_ndk could not make the channel's binder".to_string());
        }
        // The interface is declared in the vendor manifest, and system apps call it.
        (ndk.AIBinder_markVintfStability)(binder);
        let exception = (ndk.AServiceManager_addService)(binder, instance.as_ptr());
        if exception != EX_NONE {
            (ndk.AIBinder_decStrong)(binder);
            return Err(format!(
                "servicemanager refused {} (exception {exception}); is it in the vendor \
                 manifest and in vendor_service_contexts?",
                aidl::INSTANCE
            ));
        }
        (ndk.ABinderProcess_setThreadPoolMaxThreadCount)(THREADS);
        (ndk.ABinderProcess_startThreadPool)();
    }
    Ok(())
}

unsafe extern "C" fn on_create(args: *mut c_void) -> *mut c_void {
    args
}

unsafe extern "C" fn on_destroy(_user_data: *mut c_void) {}

/// The listener class is only ever a proxy here; nothing calls into it locally.
unsafe extern "C" fn listener_on_transact(
    _binder: *mut AIBinder,
    _code: u32,
    _input: *const AParcel,
    _output: *mut AParcel,
) -> binder_status_t {
    STATUS_UNKNOWN_TRANSACTION
}

unsafe extern "C" fn on_listener_died(cookie: *mut c_void) {
    if let Some(service) = SERVICE.get() {
        service.router.remove(ListenerId(cookie as u64));
    }
}

/// Collects a byte array from a parcel: `None` for a null array.
unsafe extern "C" fn allocate_bytes(
    target: *mut c_void,
    length: i32,
    buffer: *mut *mut i8,
) -> bool {
    let target = &mut *(target as *mut Option<Vec<u8>>);
    if length < 0 {
        *target = None;
        *buffer = std::ptr::null_mut();
        return true;
    }
    let bytes = target.insert(vec![0u8; length as usize]);
    *buffer = bytes.as_mut_ptr().cast();
    true
}

/// Writes a reply's status: no exception, or `exception` with `message`.
unsafe fn write_status(
    ndk: &Ndk,
    output: *mut AParcel,
    error: Option<(binder_exception_t, &str)>,
) -> binder_status_t {
    let status = match error {
        None => (ndk.AStatus_newOk)(),
        Some((exception, message)) => {
            let message = CString::new(message.replace('\0', " ")).unwrap();
            (ndk.AStatus_fromExceptionCodeWithMessage)(exception, message.as_ptr())
        }
    };
    let result = (ndk.AParcel_writeStatusHeader)(output, status);
    (ndk.AStatus_delete)(status);
    result
}

unsafe extern "C" fn channel_on_transact(
    _binder: *mut AIBinder,
    code: u32,
    input: *const AParcel,
    output: *mut AParcel,
) -> binder_status_t {
    let Some(service) = SERVICE.get() else {
        return STATUS_UNKNOWN_TRANSACTION;
    };
    // A panic must not unwind into libbinder_ndk; the call fails instead.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transact(service, code, input, output)
    }))
    .unwrap_or(STATUS_UNKNOWN_TRANSACTION)
}

unsafe fn transact(
    service: &Service,
    code: u32,
    input: *const AParcel,
    output: *mut AParcel,
) -> binder_status_t {
    let ndk = &service.ndk;
    match code {
        aidl::SEND => {
            let mut frame: Option<Vec<u8>> = None;
            let read = (ndk.AParcel_readByteArray)(
                input,
                (&mut frame as *mut Option<Vec<u8>>).cast(),
                allocate_bytes,
            );
            if read != STATUS_OK {
                return read;
            }
            let Some(frame) = frame else {
                return write_status(ndk, output, Some((EX_NULL_POINTER, "frame is null")));
            };
            let peer = Peer((ndk.AIBinder_getCallingPid)());
            match service.router.from_guest(peer, frame) {
                Ok(()) => write_status(ndk, output, None),
                Err(error) => {
                    write_status(ndk, output, Some((EX_ILLEGAL_ARGUMENT, &error.to_string())))
                }
            }
        }
        aidl::SUBSCRIBE => {
            let mut binder: *mut AIBinder = std::ptr::null_mut();
            let read = (ndk.AParcel_readStrongBinder)(input, &mut binder);
            if read != STATUS_OK {
                return read;
            }
            if binder.is_null() {
                return write_status(ndk, output, Some((EX_NULL_POINTER, "listener is null")));
            }
            if !(ndk.AIBinder_associateClass)(binder, service.listener_class) {
                (ndk.AIBinder_decStrong)(binder);
                let message = format!("the listener is not an {}", aidl::LISTENER_DESCRIPTOR);
                return write_status(ndk, output, Some((EX_ILLEGAL_ARGUMENT, &message)));
            }
            let peer = Peer((ndk.AIBinder_getCallingPid)());
            let listener = Arc::new(BinderListener { binder });
            let id = service.router.subscribe(peer, listener);
            let linked = (ndk.AIBinder_linkToDeath)(binder, service.death, id.0 as *mut c_void);
            if linked != STATUS_OK {
                // Died before it could be watched, or cannot be watched: either way it
                // would never be dropped otherwise.
                service.router.remove(id);
            }
            write_status(ndk, output, None)
        }
        aidl::GET_INTERFACE_VERSION => {
            let written = write_status(ndk, output, None);
            if written != STATUS_OK {
                return written;
            }
            (ndk.AParcel_writeInt32)(output, aidl::VERSION)
        }
        aidl::GET_INTERFACE_HASH => {
            let written = write_status(ndk, output, None);
            if written != STATUS_OK {
                return written;
            }
            let hash = aidl::HASH;
            (ndk.AParcel_writeString)(output, hash.as_ptr().cast(), hash.len() as i32)
        }
        _ => STATUS_UNKNOWN_TRANSACTION,
    }
}

/// A client's `IChannelListener`, called one way.
struct BinderListener {
    binder: *mut AIBinder,
}

// SAFETY: an AIBinder may be used and released from any thread.
unsafe impl Send for BinderListener {}
unsafe impl Sync for BinderListener {}

impl Listener for BinderListener {
    fn deliver(&self, frame: &[u8]) -> Delivery {
        let Some(service) = SERVICE.get() else {
            return Delivery::Failed;
        };
        let ndk = &service.ndk;
        // SAFETY: the binder is held strongly until drop; the parcels are created and
        // released by libbinder_ndk as AIBinder_transact documents.
        unsafe {
            let mut input: *mut AParcel = std::ptr::null_mut();
            let mut status = (ndk.AIBinder_prepareTransaction)(self.binder, &mut input);
            if status == STATUS_OK {
                status =
                    (ndk.AParcel_writeByteArray)(input, frame.as_ptr().cast(), frame.len() as i32);
                if status == STATUS_OK {
                    let mut output: *mut AParcel = std::ptr::null_mut();
                    status = (ndk.AIBinder_transact)(
                        self.binder,
                        aidl::ON_FRAME,
                        &mut input,
                        &mut output,
                        FLAG_ONEWAY | FLAG_PRIVATE_VENDOR,
                    );
                    if !output.is_null() {
                        (ndk.AParcel_delete)(output);
                    }
                    input = std::ptr::null_mut(); // AIBinder_transact took it
                }
            }
            if !input.is_null() {
                (ndk.AParcel_delete)(input);
            }
            match status {
                STATUS_OK => Delivery::Delivered,
                STATUS_DEAD_OBJECT => Delivery::Gone,
                _ => Delivery::Failed,
            }
        }
    }
}

impl Drop for BinderListener {
    fn drop(&mut self) {
        if let Some(service) = SERVICE.get() {
            // SAFETY: the reference AParcel_readStrongBinder handed us, released once.
            unsafe { (service.ndk.AIBinder_decStrong)(self.binder) };
        }
    }
}
