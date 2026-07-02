//! GUI-integration callback for the termhost COM handoff.
//!
//! Runs on the STA — COM serializes all handoff calls through this one
//! thread, so no synchronization is needed inside the handler. We do
//! the minimum work here (pipe allocation, out-params, handle wrapping)
//! and dispatch the mux-attaching work onto WezTerm's async executor.

use std::mem;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Context;
use config::{Dimension, GeometryOrigin, GuiPosition};
use portable_pty::{Child, MasterPty, PtySize};
use winapi::shared::ntdef::HANDLE;

use wezterm_term::TerminalSize;

use super::{
    create_anon_pipe, pid_of, spawn_replacement_tab, RawHandlesMasterPty, TermHostChild,
    TerminalStartupInfoOwned, HANDOFF_RECEIVED,
};

// STARTF_USEPOSITION (a Windows STARTUPINFO dwFlags bit) — only used here
// for handoff-position support.
const STARTF_USEPOSITION: u32 = 0x00000004;

/// Per `ITerminalHandoff3` (IDL lines 75-76), `in_handle_out` and
/// `out_handle_out` are `[out] HANDLE*` (out-parameters holding Windows
/// kernel object references). WezTerm allocates the ConPTY (Windows'
/// pseudo-terminal) pipe pair and writes the ConPTY-side ends through
/// these pointers, keeping the terminal-side ends. Conhost takes
/// ownership of the handles we hand back (extracts via
/// `wil::unique_handle::release`, closes on session end).
pub(crate) fn handle_handoff(
    in_handle_out: *mut HANDLE,
    out_handle_out: *mut HANDLE,
    signal: HANDLE,
    reference: HANDLE,
    server: HANDLE,
    client: HANDLE,
    startup: TerminalStartupInfoOwned,
) -> anyhow::Result<()> {
    let client_pid = pid_of(client);

    debug_assert!(
        !in_handle_out.is_null() && !out_handle_out.is_null(),
        "IDL contract: ITerminalHandoff3 [out] HANDLE* must be non-null"
    );

    let (their_read_in, our_write) =
        create_anon_pipe(true, false).context("create_anon_pipe for ConPTY stdin")?;
    let (our_read, their_write_out) =
        create_anon_pipe(false, true).context("create_anon_pipe for ConPTY stdout")?;

    // Forget the ConPTY-side OwnedHandles: their handles are now owned
    // by the caller's out-param slots. If we let them drop, they would
    // close before the COM runtime marshals them to conhost.
    let their_read_raw = their_read_in.as_raw_handle() as HANDLE;
    let their_write_raw = their_write_out.as_raw_handle() as HANDLE;
    mem::forget(their_read_in);
    mem::forget(their_write_out);

    unsafe {
        *in_handle_out = their_read_raw;
        *out_handle_out = their_write_raw;
    }

    // Mark received only after out-params are written: on failure, conhost
    // gets E_FAIL and await_handoff()'s fallback still fires.
    HANDOFF_RECEIVED.store(true, Ordering::SeqCst);

    let config = config::configuration();
    let initial_size = PtySize {
        rows: if startup.height != 0 {
            startup.height
        } else {
            config.initial_rows
        },
        cols: if startup.width != 0 {
            startup.width
        } else {
            config.initial_cols
        },
        pixel_width: 0,
        pixel_height: 0,
    };

    // Same rationale as above: detach the terminal-side OwnedHandles
    // before from_raw_handles takes ownership (it wraps the read handle
    // and duplicates the write handle).
    let our_read_raw = our_read.as_raw_handle() as HANDLE;
    let our_write_raw = our_write.as_raw_handle() as HANDLE;
    mem::forget(our_read);
    mem::forget(our_write);

    let master: Box<dyn MasterPty + Send> = Box::new(unsafe {
        RawHandlesMasterPty::from_raw_handles(
            our_read_raw,
            our_write_raw,
            signal,
            reference,
            initial_size,
        )
    });
    // Wait on the PTY server handle rather than the initial client
    // process: the client can exit after spawning another console
    // process, but the ConPTY session is still alive.
    let child: Box<dyn Child + Send + Sync> =
        Box::new(unsafe { TermHostChild::from_raw(server, client_pid) });

    let title = startup
        .title
        .as_ref()
        .filter(|s| !s.is_empty())
        .cloned()
        .unwrap_or_else(|| "WezTerm (termhost)".to_string());

    // Stage the show-window hint for apply_pending_window_state.
    super::set_pending_startup_state(&startup);

    let position = startup_position(&startup);

    promise::spawn::spawn(async move {
        let _activity = mux::activity::Activity::new();

        // Panics during attach propagate to the spawn executor, which
        // routes them to wnd_proc's catch_unwind (window.rs:2994) →
        // process::exit(1). There is no recovery path: out-params were
        // delivered to the COM caller at S_OK return, so conhost has no
        // retry either.
        if let Err(e) =
            attach_pane_to_new_window(master, child, title, initial_size, position).await
        {
            log::error!("Termhost attach failed: {e:#}");
            // Out-params already delivered (S_OK returned to COM caller), so the
            // attach failure is unrecoverable: master/child are consumed, conhost
            // has no retry path. Spawn a default-profile window so the user sees
            // something rather than nothing. Use the unconditional helper: on a
            // warm instance the `_if_mux_is_empty` guard would no-op here.
            spawn_replacement_tab().await;
        }
    })
    .detach();

    Ok(())
}

fn startup_position(startup: &TerminalStartupInfoOwned) -> Option<GuiPosition> {
    if (startup.dw_flags & STARTF_USEPOSITION) == 0 {
        return None;
    }

    Some(GuiPosition {
        x: Dimension::Pixels(signed_startup_coordinate(startup.position_x)),
        y: Dimension::Pixels(signed_startup_coordinate(startup.position_y)),
        origin: GeometryOrigin::ScreenCoordinateSystem,
    })
}

fn signed_startup_coordinate(value: u32) -> f32 {
    // STARTUPINFO stores screen coordinates in DWORD fields, but negative
    // virtual-screen positions are encoded as signed 32-bit values.
    i32::from_ne_bytes(value.to_ne_bytes()) as f32
}

async fn attach_pane_to_new_window(
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    title: String,
    initial_size: PtySize,
    position: Option<GuiPosition>,
) -> anyhow::Result<()> {
    // Use `try_get` not `get`: if conhost delivers a handoff between
    // CoRegisterClassObject and Mux::build_initial_mux, `get` would
    // panic. Returning Err propagates to E_FAIL and conhost falls back.
    let mux = mux::Mux::try_get().context("Mux not yet initialized when handoff arrived")?;

    let domain = mux
        .get_domain_by_name("local")
        .context("no 'local' domain registered with the mux")?;
    let local_domain = domain
        .downcast_ref::<mux::domain::LocalDomain>()
        .ok_or_else(|| {
            anyhow::anyhow!("the 'local' domain is not a LocalDomain; termhost cannot attach")
        })?;

    let size = TerminalSize {
        rows: initial_size.rows as usize,
        cols: initial_size.cols as usize,
        ..Default::default()
    };

    let command_description = format!("termhost handoff: {}", title);

    let pane = local_domain
        .attach_external_pane(size, master, child, command_description)
        .context("LocalDomain::attach_external_pane")?;

    let tab = Arc::new(mux::tab::Tab::new(&size));
    tab.assign_pane(&pane);
    mux.add_tab_and_active_pane(&tab)
        .context("add_tab_and_active_pane")?;

    let workspace = mux.active_workspace();
    let builder = mux.new_empty_window(Some(workspace), position);
    mux.add_tab_to_window(&tab, *builder)
        .context("add_tab_to_window")?;

    crate::trigger_and_log_gui_attached(mux_lua::MuxDomain(domain.domain_id())).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_position_ignored_without_flag() {
        let startup = TerminalStartupInfoOwned {
            position_x: 100,
            position_y: 200,
            ..Default::default()
        };

        assert_eq!(startup_position(&startup), None);
    }

    #[test]
    fn startup_position_preserves_signed_screen_coordinates() {
        let startup = TerminalStartupInfoOwned {
            dw_flags: STARTF_USEPOSITION,
            position_x: u32::from_ne_bytes((-100i32).to_ne_bytes()),
            position_y: u32::from_ne_bytes((-50i32).to_ne_bytes()),
            ..Default::default()
        };

        assert_eq!(
            startup_position(&startup),
            Some(GuiPosition {
                x: Dimension::Pixels(-100.0),
                y: Dimension::Pixels(-50.0),
                origin: GeometryOrigin::ScreenCoordinateSystem,
            })
        );
    }
}
