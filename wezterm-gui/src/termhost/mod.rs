//! Windows Default Terminal handoff via `ITerminalHandoff3`.
//!
//! When conhost delegates a PTY to us we open a new tab attached to it.

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
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
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

// Staged STARTUPINFO hints for the next handoff-created window.
// Writer: handle_handoff (sync, before spawn). Reader: apply_pending_window_state.
#[cfg(windows)]
static PENDING_SHOW_WINDOW: AtomicU16 = AtomicU16::new(SHOW_WINDOW_NONE);
#[cfg(windows)]
static PENDING_ICON: Mutex<Option<(String, i32)>> = Mutex::new(None);

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
    if let Err(e) = crate::spawn_tab_in_domain_if_mux_is_empty(None, false, None, None).await {
        log::error!("Fallback spawn failed: {e:#}");
    }
}

// Unconditional sibling of `spawn_fallback_tab` for the post-S_OK error
// path in `integration.rs`. `spawn_fallback_tab` (guarded) is correct for
// `await_handoff` cold-start (mux is empty there by definition), but wrong
// for the warm-instance handoff-failure case where the mux already has
// panes — the guard would no-op, leaving the failed launch with no window.
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
    let window_id = *mux.new_empty_window(workspace.clone(), None);
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

/// Hold an Activity guard for 5s to suppress `MuxNotification::Empty`
/// termination while we wait for the COM handoff; spawn the
/// default-profile tab as fallback if none arrives.
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
    if startup.show_window == SW_SHOWMAXIMIZED {
        PENDING_SHOW_WINDOW.store(startup.show_window, Ordering::SeqCst);
    } else {
        PENDING_SHOW_WINDOW.store(SHOW_WINDOW_NONE, Ordering::SeqCst);
    }

    let pending_icon = startup
        .icon_path
        .as_ref()
        .filter(|path| !path.is_empty())
        .map(|path| (path.clone(), startup.icon_index));
    *PENDING_ICON.lock().unwrap() = pending_icon;
}

/// Owns an `HICON` from `ExtractIconExW`; `Drop` calls `DestroyIcon`.
/// `WM_SETICON` does not take ownership, so the handle must be retained
/// and freed by us. Only ever holds icons we extracted ourselves.
#[cfg(windows)]
pub struct OwnedHIcon(winapi::shared::windef::HICON);

#[cfg(windows)]
impl Drop for OwnedHIcon {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { winapi::um::winuser::DestroyIcon(self.0) };
        }
    }
}

#[cfg(windows)]
pub fn apply_pending_window_state(window: &::window::Window) -> Option<OwnedHIcon> {
    let show = PENDING_SHOW_WINDOW.swap(SHOW_WINDOW_NONE, Ordering::SeqCst);
    if show == SW_SHOWMAXIMIZED {
        ::window::WindowOps::maximize(window);
    }
    let pending_icon = PENDING_ICON.lock().unwrap().take();
    let owned_icon =
        pending_icon.and_then(|(path, index)| apply_icon_from_path(window, &path, index));
    if scm_launched() {
        ::window::WindowOps::focus(window);
    }
    owned_icon
}

#[cfg(windows)]
fn apply_icon_from_path(window: &::window::Window, path: &str, index: i32) -> Option<OwnedHIcon> {
    use ::window::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use winapi::shared::ntdef::LPCWSTR;
    use winapi::shared::windef::HICON;
    use winapi::um::shellapi::ExtractIconExW;
    use winapi::um::winuser::{SendMessageW, ICON_BIG, WM_SETICON};

    let hwnd = match window.window_handle() {
        Ok(handle) => match handle.as_raw() {
            RawWindowHandle::Win32(win32) => win32.hwnd.get() as winapi::shared::windef::HWND,
            _ => {
                log::warn!("Non-Win32 window, skipping icon");
                return None;
            }
        },
        Err(e) => {
            log::warn!("window_handle failed: {e}");
            return None;
        }
    };

    let mut wide: Vec<u16> = path.encode_utf16().collect();
    wide.push(0);

    let mut hicon: HICON = std::ptr::null_mut();
    let count = unsafe {
        ExtractIconExW(
            wide.as_ptr() as LPCWSTR,
            index,
            &mut hicon,
            std::ptr::null_mut(),
            1,
        )
    };
    if count == 0 || hicon.is_null() {
        log::warn!("ExtractIconExW failed for icon {path:?} index {index}");
        return None;
    }

    unsafe {
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, hicon as isize);
    }
    // WM_SETICON did not consume the handle; ownership stays with us.
    Some(OwnedHIcon(hicon))
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
        PENDING_SHOW_WINDOW.store(SHOW_WINDOW_NONE, Ordering::SeqCst);
        *PENDING_ICON.lock().unwrap() = None;
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
            icon_path: None,
            icon_index: 0,
            ..Default::default()
        };
        set_pending_startup_state(&startup);
        assert_eq!(PENDING_SHOW_WINDOW.load(Ordering::SeqCst), SW_SHOWMAXIMIZED);
        assert_eq!(
            PENDING_SHOW_WINDOW.swap(SHOW_WINDOW_NONE, Ordering::SeqCst),
            SW_SHOWMAXIMIZED
        );
        assert_eq!(PENDING_SHOW_WINDOW.load(Ordering::SeqCst), SHOW_WINDOW_NONE);
    }

    #[test]
    fn pending_show_window_cleared_when_not_maximized() {
        let _guard = pending_state_guard();
        PENDING_SHOW_WINDOW.store(SW_SHOWMAXIMIZED, Ordering::SeqCst);
        let startup = TerminalStartupInfoOwned {
            show_window: 0,
            dw_flags: 0,
            ..Default::default()
        };
        set_pending_startup_state(&startup);
        assert_eq!(PENDING_SHOW_WINDOW.load(Ordering::SeqCst), SHOW_WINDOW_NONE);
    }

    #[test]
    fn pending_icon_set_for_non_empty_path_without_flag() {
        let _guard = pending_state_guard();
        let startup = TerminalStartupInfoOwned {
            icon_path: Some("C:\\path\\to.ico".to_string()),
            icon_index: 0,
            ..Default::default()
        };
        set_pending_startup_state(&startup);
        let pending = PENDING_ICON.lock().unwrap().take();
        let (path, index) = pending.expect("pending icon");
        assert_eq!(index, 0);
        assert!(!path.is_empty());
    }

    #[test]
    fn pending_icon_cleared_for_empty_path() {
        let _guard = pending_state_guard();
        *PENDING_ICON.lock().unwrap() = Some(("C:\\old.ico".to_string(), 0));
        let startup = TerminalStartupInfoOwned {
            icon_path: Some(String::new()),
            icon_index: 1,
            ..Default::default()
        };
        set_pending_startup_state(&startup);
        let pending = PENDING_ICON.lock().unwrap().take();
        assert_eq!(pending, None);
    }

    #[test]
    fn pending_icon_can_be_replaced() {
        let _guard = pending_state_guard();
        let first = TerminalStartupInfoOwned {
            icon_path: Some("C:\\first.ico".to_string()),
            icon_index: 0,
            ..Default::default()
        };
        let second = TerminalStartupInfoOwned {
            icon_path: Some("C:\\second.ico".to_string()),
            icon_index: 1,
            ..Default::default()
        };
        set_pending_startup_state(&first);
        set_pending_startup_state(&second);
        let pending = PENDING_ICON.lock().unwrap().take();
        assert_eq!(pending, Some(("C:\\second.ico".to_string(), 1)));
    }
}
