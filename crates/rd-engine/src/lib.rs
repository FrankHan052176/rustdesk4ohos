//! Replacement RustDesk runtime. Wire definitions and cryptography retain the
//! upstream contract; legacy Connection/VideoService/VideoHandler are not linked.
pub mod authentication;
mod executor;
pub mod handshake;
pub mod host;
pub mod media_capability;
pub mod media_color;
#[path = "platform/ohos_publisher.rs"]
pub mod publisher;
pub mod rendezvous;
pub mod session;
pub mod transport;
pub mod viewer;
#[cfg(all(target_os = "windows", feature = "windows-modern-producer"))]
#[path = "platform/windows_native.rs"]
pub mod windows_native;
#[cfg(all(target_os = "windows", feature = "windows-modern-producer"))]
pub use windows_native::NATIVE_BACKEND_IMPLEMENTED;
#[cfg(feature = "windows-modern-producer")]
#[path = "platform/windows_publisher.rs"]
pub mod windows_publisher;
