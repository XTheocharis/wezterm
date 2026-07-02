//! Windows Default Terminal support via COM's `ITerminalHandoff3` interface.
//! When conhost delegates a console session to us, we receive the PTY
//! pipe handles via COM and wrap them as a new pane.
//!
//! COM is Windows' inter-process communication system. A COM interface is
//! a versioned contract identified by a 128-bit IID; a class implementing
//! one is identified by a separate 128-bit CLSID. Windows' process launcher
//! (the SCM, or Service Control Manager) uses the CLSID to start WezTerm
//! when a handoff request arrives.

#[cfg(windows)]
pub mod com_interfaces;
#[cfg(windows)]
pub mod handoff;
#[cfg(windows)]
pub mod raw_pty;
#[cfg(windows)]
pub mod registration;
#[cfg(windows)]
pub mod server;
#[cfg(windows)]
pub mod types;

#[cfg(windows)]
mod integration;

#[cfg(windows)]
pub(crate) mod cli;

#[cfg(windows)]
pub use handoff::{HandoffCallback, TerminalStartupInfoOwned};
#[cfg(windows)]
pub use raw_pty::{create_anon_pipe, RawHandlesMasterPty, TermHostChild};
#[cfg(windows)]
pub use registration::{
    is_wt_installed, register_openconsole_fallback, register_proxy_stub_per_user,
    register_termhost, resolve_bundled_openconsole_path, resolve_proxy_stub_dll_path,
    TERMHOST_HANDOFF_IIDS, WEZTERM_PROXY_STUB_CLSID, WEZTERM_TERMHOST_FALLBACK_CONSOLE_CLSID,
    WEZTERM_TERMHOST_TERMINAL_CLSID,
};
#[cfg(windows)]
pub(crate) use registration::{key_is_wezterm_owned, WEZTERM_OWNED_VALUE};
#[cfg(windows)]
pub use server::{start_listening, CoinitGuard, HandoffGuard};

use anyhow::Context;
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use winapi::shared::ntdef::HANDLE;

static LISTENER_STARTED: OnceLock<()> = OnceLock::new();

static SCM_LAUNCHED: OnceLock<bool> = OnceLock::new();

static HANDOFF_RECEIVED: AtomicBool = AtomicBool::new(false);

// SW_* values (winuser).
#[cfg(windows)]
const SW_SHOWMAXIMIZED: u16 = 3;
// Sentinel distinct from every real SW_* value (all < 16).
#[cfg(windows)]
const SHOW_WINDOW_NONE: u16 = u16::MAX;

// Window-state hint from the Windows STARTUPINFO struct, staged for the
// next handoff-created window. Writer: handle_handoff (sync, before spawn).
// Reader: apply_pending_window_state.
#[cfg(windows)]
struct PendingWindowState {
    show_window: u16,
}

#[cfg(windows)]
static PENDING_WINDOW_STATE: Mutex<Option<PendingWindowState>> = Mutex::new(None);

pub fn set_scm_launched(v: bool) {
    let _ = SCM_LAUNCHED.set(v);
}

pub fn scm_launched() -> bool {
    *SCM_LAUNCHED.get().unwrap_or(&false)
}

/// Holds the termhost COM registration. Drop order matters: `handoff`
/// must drop before `coinit` so `CoRevokeClassObject` runs while COM is
/// still initialized on this thread. Rust drops fields in declaration
/// order, so `handoff` is declared first.
#[allow(dead_code)]
pub struct TermHostState {
    handoff: Option<HandoffGuard>,
    coinit: CoinitGuard,
}

pub fn install() -> Option<TermHostState> {
    let coinit = match CoinitGuard::new() {
        Ok(g) => g,
        Err(e) => {
            log::error!("CoInitializeEx(STA) on main thread failed: {e:#}");
            return None;
        }
    };
    let handoff = match try_start_listener() {
        Ok(g) => g,
        Err(e) => {
            log::error!("termhost listener failed to start: {e:#}");
            return None;
        }
    };
    Some(TermHostState { handoff, coinit })
}

/// Detect SCM launch (`-Embedding` / `/Embedding`) and strip the flag
/// before clap parsing.
pub fn preprocess_argv() -> (Vec<OsString>, bool) {
    filter_embedding_flags(std::env::args_os())
}

fn filter_embedding_flags(argv: impl Iterator<Item = OsString>) -> (Vec<OsString>, bool) {
    let mut filtered = Vec::new();
    let mut argv = argv.into_iter();
    let Some(argv0) = argv.next() else {
        return (filtered, false);
    };

    filtered.push(argv0);
    let Some(first_arg) = argv.next() else {
        return (filtered, false);
    };

    let scm_launched = is_embedding_flag(&first_arg);
    if scm_launched {
        filtered.push(OsString::from("start"));
        filtered.push(OsString::from("--always-new-process"));
    } else {
        filtered.push(first_arg);
    }
    filtered.extend(argv);
    (filtered, scm_launched)
}

fn is_embedding_flag(arg: &OsString) -> bool {
    arg.as_os_str() == OsStr::new("-Embedding") || arg.as_os_str() == OsStr::new("/Embedding")
}

async fn spawn_fallback_tab() {
    // If this fails, the Activity guard drops, mux fires Empty, process
    // exits. A late handoff is lost (conhost already has S_OK). The
    // alternative is an invisible process with no window.
    if let Err(e) = crate::spawn_tab_in_domain_if_mux_is_empty(None, false, None, None).await {
        log::error!("Fallback spawn failed: {e:#}");
    }
}

// Unconditional spawn for the post-S_OK error path in integration.rs.
// The guarded `spawn_fallback_tab` would no-op on a warm instance.
async fn spawn_replacement_tab() {
    if let Err(e) = spawn_default_tab_in_new_window().await {
        log::error!("Replacement spawn failed: {e:#}");
    }
}

async fn spawn_default_tab_in_new_window() -> anyhow::Result<()> {
    let mux =
        mux::Mux::try_get().context("Mux not initialized when spawning replacement window")?;
    let workspace = Some(mux.active_workspace());
    let domain = mux.default_domain();
    let window_builder = mux.new_empty_window(workspace.clone(), None);
    let window_id = *window_builder;
    let config = config::configuration();
    config.update_ulimit()?;
    domain.attach(Some(window_id)).await?;
    let dpi = config.dpi.unwrap_or_else(|| ::window::default_dpi());
    let _tab = domain
        .spawn(
            config.initial_size(dpi as u32, Some(crate::cell_pixel_dims(&config, dpi)?)),
            None,
            None,
            window_id,
        )
        .await?;
    crate::trigger_and_log_gui_attached(mux_lua::MuxDomain(domain.domain_id())).await;
    Ok(())
}

/// Hold an Activity guard to suppress `MuxNotification::Empty` termination
/// while we wait for the COM handoff; spawn a default-profile tab as
/// fallback if none arrives within 5 seconds (empirical margin for slow
/// systems). If the handoff arrives after the fallback, both windows coexist.
pub fn await_handoff() {
    promise::spawn::spawn(async move {
        let _activity = mux::activity::Activity::new();

        smol::Timer::after(std::time::Duration::from_secs(5)).await;

        if !HANDOFF_RECEIVED.load(Ordering::SeqCst) {
            spawn_fallback_tab().await;
        }
    })
    .detach();
}

#[cfg(windows)]
pub(crate) fn set_pending_startup_state(startup: &TerminalStartupInfoOwned) {
    let show_window = if startup.show_window == SW_SHOWMAXIMIZED {
        SW_SHOWMAXIMIZED
    } else {
        SHOW_WINDOW_NONE
    };

    *PENDING_WINDOW_STATE.lock().unwrap() = Some(PendingWindowState { show_window });
}

#[cfg(windows)]
pub fn apply_pending_window_state(window: &::window::Window) {
    let state = PENDING_WINDOW_STATE.lock().unwrap().take();
    if let Some(state) = state {
        if state.show_window == SW_SHOWMAXIMIZED {
            ::window::WindowOps::maximize(window);
        }
    }
    if scm_launched() {
        ::window::WindowOps::focus(window);
    }
}

pub(crate) fn try_start_listener() -> anyhow::Result<Option<HandoffGuard>> {
    if LISTENER_STARTED.get().is_some() {
        return Ok(None);
    }

    let callback: HandoffCallback = Box::new(integration::handle_handoff);
    let guard = start_listening(callback)?;
    let _ = LISTENER_STARTED.set(());
    log::info!("Termhost listener started");
    Ok(Some(guard))
}

fn pid_of(handle: HANDLE) -> Option<u32> {
    if handle.is_null() {
        return None;
    }
    unsafe {
        use winapi::um::processthreadsapi::GetProcessId;
        let pid = GetProcessId(handle);
        if pid == 0 {
            None
        } else {
            Some(pid)
        }
    }
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    fn pending_state_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        *PENDING_WINDOW_STATE.lock().unwrap() = None;
        guard
    }

    #[test]
    fn filter_embedding_flags_strips_dash_embedding() {
        let argv = vec![
            os("wezterm-gui"),
            os("-Embedding"),
            os("--config-file"),
            os("foo.lua"),
        ]
        .into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 5);
        assert_eq!(filtered[0], os("wezterm-gui"));
        assert_eq!(filtered[1], os("start"));
        assert_eq!(filtered[2], os("--always-new-process"));
        assert_eq!(filtered[3], os("--config-file"));
        assert_eq!(filtered[4], os("foo.lua"));
        assert!(scm_launched);
    }

    #[test]
    fn filter_embedding_flags_strips_slash_embedding() {
        let argv = vec![os("wezterm-gui"), os("/Embedding")].into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 3);
        assert_eq!(filtered[0], os("wezterm-gui"));
        assert_eq!(filtered[1], os("start"));
        assert_eq!(filtered[2], os("--always-new-process"));
        assert!(scm_launched);
    }

    #[test]
    fn filter_embedding_flags_preserves_normal_args() {
        let argv = vec![
            os("wezterm-gui"),
            os("start"),
            os("--class"),
            os("my-class"),
        ]
        .into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 4);
        assert!(!scm_launched);
    }

    #[test]
    fn filter_embedding_flags_preserves_child_embedding_arg() {
        let argv = vec![
            os("wezterm-gui"),
            os("start"),
            os("--"),
            os("some-server.exe"),
            os("-Embedding"),
        ]
        .into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 5);
        assert_eq!(filtered[0], os("wezterm-gui"));
        assert_eq!(filtered[1], os("start"));
        assert_eq!(filtered[2], os("--"));
        assert_eq!(filtered[3], os("some-server.exe"));
        assert_eq!(filtered[4], os("-Embedding"));
        assert!(!scm_launched);
    }

    #[test]
    fn filter_embedding_flags_rejects_near_misses() {
        let argv = vec![
            os("wezterm-gui"),
            os("--embedding"),
            os("-embeddings"),
            os("-Embedding="),
            os("-embedding"),
            os("/embedding"),
        ]
        .into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 6);
        assert!(!scm_launched);
    }

    #[test]
    fn filter_embedding_flags_handles_empty_argv() {
        let argv = std::iter::empty::<OsString>();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert!(filtered.is_empty());
        assert!(!scm_launched);
    }

    #[test]
    fn filter_embedding_flags_only_strips_first_real_arg() {
        let argv = vec![
            os("wezterm-gui"),
            os("-Embedding"),
            os("/Embedding"),
            os("-Embedding"),
            os("subcommand"),
        ]
        .into_iter();
        let (filtered, scm_launched) = filter_embedding_flags(argv);
        assert_eq!(filtered.len(), 6);
        assert_eq!(filtered[0], os("wezterm-gui"));
        assert_eq!(filtered[1], os("start"));
        assert_eq!(filtered[2], os("--always-new-process"));
        assert_eq!(filtered[3], os("/Embedding"));
        assert_eq!(filtered[4], os("-Embedding"));
        assert_eq!(filtered[5], os("subcommand"));
        assert!(scm_launched);
    }

    #[test]
    fn pending_show_window_stages_maximized_without_flag() {
        let _guard = pending_state_guard();
        let startup = TerminalStartupInfoOwned {
            show_window: SW_SHOWMAXIMIZED,
            ..Default::default()
        };
        set_pending_startup_state(&startup);
        let state = PENDING_WINDOW_STATE.lock().unwrap().take().expect("state");
        assert_eq!(state.show_window, SW_SHOWMAXIMIZED);
    }

    #[test]
    fn pending_show_window_cleared_when_not_maximized() {
        let _guard = pending_state_guard();
        let startup = TerminalStartupInfoOwned {
            show_window: 0,
            dw_flags: 0,
            ..Default::default()
        };
        set_pending_startup_state(&startup);
        let state = PENDING_WINDOW_STATE.lock().unwrap().take().expect("state");
        assert_eq!(state.show_window, SHOW_WINDOW_NONE);
    }
}
