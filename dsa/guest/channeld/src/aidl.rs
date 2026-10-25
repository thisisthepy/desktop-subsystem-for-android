//! The binder interface the daemon serves, as `aidl` generates it from
//! `dsa/guest/aidl/io/github/thisisthepy/dsa/channel/`.
//!
//! The daemon implements the interface by hand over the NDK binder instead of from
//! generated code, so these values have to be exactly what the generated Java and NDK
//! clients send. A test reads the `.aidl` files and checks them.

/// The interface token every call starts with.
pub const CHANNEL_DESCRIPTOR: &str = "io.github.thisisthepy.dsa.channel.IChannel";
/// The listener's interface token.
pub const LISTENER_DESCRIPTOR: &str = "io.github.thisisthepy.dsa.channel.IChannelListener";
/// The name the service is registered under with servicemanager. It is a VINTF instance:
/// the package, the interface and the instance, as the vendor manifest declares it.
pub const INSTANCE: &str = "io.github.thisisthepy.dsa.channel.IChannel/default";

/// The interface's version.
pub const VERSION: i32 = 1;
/// The interface's hash. `aidl` reports "notfrozen" for a version that has no frozen API
/// dump, and this interface is not built by Soong, which is what freezes versions.
pub const HASH: &str = "notfrozen";

/// `binder::FIRST_CALL_TRANSACTION`: the methods are numbered from it in the order they
/// are declared.
pub const FIRST_CALL_TRANSACTION: u32 = 1;
/// `IChannel.send(in byte[] frame)`.
pub const SEND: u32 = FIRST_CALL_TRANSACTION;
/// `IChannel.subscribe(in IChannelListener listener)`.
pub const SUBSCRIBE: u32 = FIRST_CALL_TRANSACTION + 1;
/// `IChannelListener.onFrame(in byte[] frame)`, one way.
pub const ON_FRAME: u32 = FIRST_CALL_TRANSACTION;
/// The version query every versioned AIDL interface answers.
pub const GET_INTERFACE_VERSION: u32 = FIRST_CALL_TRANSACTION + 16_777_214;
/// The hash query every versioned AIDL interface answers.
pub const GET_INTERFACE_HASH: u32 = FIRST_CALL_TRANSACTION + 16_777_213;
