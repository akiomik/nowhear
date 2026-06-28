//! Platform-specific implementations.
//!
//! This module contains the platform-specific implementations for media watching.
//! The appropriate module is compiled based on the target operating system:
//!
//! - `linux`: MPRIS D-Bus interface implementation
//! - `macos`: JXA-based implementation
//! - `windows`: Windows Media Control API implementation
//!
//! These modules are internal and not intended for direct use. Use the
//! platform-agnostic [`crate::source::MediaSource`] trait and [`crate::source::MediaSourceBuilder`]
//! instead.

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

/// Platform-agnostic player-state snapshot and event diffing.
///
/// Consumed by the backends that read full state snapshots and diff successive reads (Windows and
/// macOS). The `cfg` lists those backends; Linux is absent because MPRIS delivers explicit deltas
/// and never diffs snapshots. The module itself has no platform dependencies.
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod state;
