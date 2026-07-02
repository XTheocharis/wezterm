//! The COM class Windows calls to hand off a new terminal session.
//!
//! Implements `ITerminalHandoff3` and `IDefaultTerminalMarker` on a
//! single object. Windows looks up our CLSID in the registry, activates
//! the class, and calls `EstablishPtyHandoff` to deliver the session's
//! pipe handles.

// COM method names (`QueryInterface`, `AddRef`, `Release`, …) and the
// `This` parameter are fixed by the COM ABI (the MIDL compiler on
// Windows enforces this layout). Suppress snake_case warnings for them.
#![allow(non_snake_case)]

use std::sync::OnceLock;

use winapi::shared::ntdef::HANDLE;

mod establish;
mod factory;
mod instance;

pub(crate) use factory::take_factory;
#[cfg(all(windows, test))]
pub(crate) use instance::take_singleton;

/// Owned Rust copy of the `TerminalStartupInfo` struct that Windows
/// passes across the COM boundary. The `width` / `height` fields map to
/// `dwXCountChars` / `dwYCountChars` in the COM interface definition
/// (written in IDL, the Interface Definition Language). Consumers must
/// treat 0 as "unspecified" — Microsoft's conhost leaves them zero
/// on the wire.
#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
pub struct TerminalStartupInfoOwned {
    pub title: Option<String>,
    pub show_window: u16,
    pub width: u16,
    pub height: u16,
    pub dw_flags: u32,
    pub position_x: u32,
    pub position_y: u32,
}

/// Callback invoked when Windows hands off a ConPTY session
/// (`ITerminalHandoff3::EstablishPtyHandoff`, IDL lines 75-76).
///
/// `in_handle` / `out_handle` are `[out]` — we allocate the ConPTY
/// pipes and write our ends back through these pointers.
///
/// `signal`, `reference`, `server`, `client` are `[in]` — pre-filled
/// by Windows. The COM proxy stub closes them after the call returns,
/// so duplicate anything you want to keep (see `raw_pty/master.rs`
/// for the reference-handle contract).
pub type HandoffCallback = Box<
    dyn Fn(
            *mut HANDLE,
            *mut HANDLE,
            HANDLE,
            HANDLE,
            HANDLE,
            HANDLE,
            TerminalStartupInfoOwned,
        ) -> anyhow::Result<()>
        + Send
        + Sync,
>;

static HANDOFF_CALLBACK: OnceLock<HandoffCallback> = OnceLock::new();

pub fn set_callback(callback: HandoffCallback) -> Result<(), HandoffCallback> {
    HANDOFF_CALLBACK.set(callback)
}
