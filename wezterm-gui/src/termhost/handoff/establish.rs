// Vtable field names are mandated by the COM ABI / MIDL convention.
#![allow(non_snake_case)]

use std::convert::TryFrom;
use std::ffi::c_void;

use winapi::shared::ntdef::HANDLE;
use winapi::shared::winerror::{E_FAIL, E_UNEXPECTED, S_OK};

use super::super::types::{bstr_to_string, TerminalStartupInfo};
use super::{TerminalStartupInfoOwned, HANDOFF_CALLBACK};

unsafe fn startup_info_to_owned(s: &TerminalStartupInfo) -> TerminalStartupInfoOwned {
    TerminalStartupInfoOwned {
        title: Some(bstr_to_string(s.pszTitle)),
        icon_path: Some(bstr_to_string(s.pszIconPath)),
        icon_index: s.iconIndex,
        show_window: s.wShowWindow,
        // Narrow u32 -> u16; bogus out-of-range conhost values map to 0.
        width: u16::try_from(s.dwXCountChars).unwrap_or(0),
        height: u16::try_from(s.dwYCountChars).unwrap_or(0),
        dw_flags: s.dwFlags,
        position_x: s.dwX,
        position_y: s.dwY,
    }
}

pub(super) unsafe extern "system" fn establish_pty_handoff(
    _this: *mut c_void,
    in_handle: *mut HANDLE,
    out_handle: *mut HANDLE,
    signal: HANDLE,
    reference: HANDLE,
    server: HANDLE,
    client: HANDLE,
    startup_info: *const TerminalStartupInfo,
) -> i32 {
    // Zero [out] params defensively so a non-compliant caller cannot read
    // stale handles if we return a failure HRESULT.
    unsafe {
        *in_handle = std::ptr::null_mut();
        *out_handle = std::ptr::null_mut();
    }

    let callback = match HANDOFF_CALLBACK.get() {
        Some(cb) => cb,
        None => {
            log::error!("EstablishPtyHandoff called but no callback registered");
            return E_UNEXPECTED;
        }
    };

    let startup_owned = if startup_info.is_null() {
        TerminalStartupInfoOwned::default()
    } else {
        startup_info_to_owned(&*startup_info)
    };

    log::info!(
        "EstablishPtyHandoff received (title={:?}, in_out={:p}, out_out={:p}, \
         signal={:p}, reference={:p}, server={:p}, client={:p})",
        startup_owned.title,
        in_handle,
        out_handle,
        signal,
        reference,
        server,
        client,
    );

    match callback(
        in_handle,
        out_handle,
        signal,
        reference,
        server,
        client,
        startup_owned,
    ) {
        Ok(()) => S_OK,
        Err(e) => {
            log::error!("Handoff callback failed: {e:#}");
            E_FAIL
        }
    }
}

#[cfg(all(windows, test))]
mod tests {
    use super::super::{set_callback, HandoffCallback};
    use super::*;
    use crate::termhost::handoff::instance::VTABLE;
    use crate::termhost::handoff::take_singleton;
    use crate::termhost::raw_pty::create_anon_pipe;
    use std::os::windows::io::AsRawHandle;
    use std::sync::{Arc, Mutex};
    use winapi::shared::winerror::S_OK;
    use winapi::um::handleapi::CloseHandle;

    #[test]
    fn startup_size_uses_count_chars_not_pixel_size() {
        let startup = TerminalStartupInfo {
            dwXSize: 1024,
            dwYSize: 768,
            dwXCountChars: 132,
            dwYCountChars: 43,
            ..Default::default()
        };

        let owned = unsafe { startup_info_to_owned(&startup) };

        assert_eq!(owned.width, 132);
        assert_eq!(owned.height, 43);
    }

    #[test]
    fn startup_info_preserves_flags_and_position() {
        let startup = TerminalStartupInfo {
            dwFlags: 0x80, // STARTF_USEPOSITION
            dwX: 100,
            dwY: 200,
            ..Default::default()
        };

        let owned = unsafe { startup_info_to_owned(&startup) };

        assert_eq!(owned.dw_flags, 0x80);
        assert_eq!(owned.position_x, 100);
        assert_eq!(owned.position_y, 200);
    }

    #[test]
    fn out_pipe_params_are_populated_by_callee() {
        let written: Arc<Mutex<(usize, usize)>> = Arc::new(Mutex::new((0, 0)));
        let captured = written.clone();

        let callback: HandoffCallback = Box::new(move |in_out, out_out, _, _, _, _, _| {
            let (their_read, _our_write) =
                create_anon_pipe(true, false).expect("create_anon_pipe for in");
            let (their_write, _our_read) =
                create_anon_pipe(true, true).expect("create_anon_pipe for out");

            let in_raw = their_read.as_raw_handle() as HANDLE;
            let out_raw = their_write.as_raw_handle() as HANDLE;

            // Forget the ConPTY-side OwnedHandles: their handles are now
            // owned by the caller's out-param slots.
            std::mem::forget(their_read);
            std::mem::forget(their_write);

            unsafe {
                if !in_out.is_null() {
                    *in_out = in_raw;
                }
                if !out_out.is_null() {
                    *out_out = out_raw;
                }
            }

            *captured.lock().unwrap() = (in_raw as usize, out_raw as usize);
            Ok(())
        });

        if set_callback(callback).is_err() {
            eprintln!(
                "out_pipe_params_are_populated_by_callee: \
                 HANDOFF_CALLBACK already set; skipping (OnceLock)."
            );
            return;
        }

        let mut in_handle: HANDLE = std::ptr::null_mut();
        let mut out_handle: HANDLE = std::ptr::null_mut();

        let this = take_singleton();
        let hr = unsafe {
            (VTABLE.EstablishPtyHandoff)(
                this,
                &mut in_handle,
                &mut out_handle,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert_eq!(hr, S_OK);

        let (expected_in, expected_out) = *written.lock().unwrap();
        assert_eq!(in_handle as usize, expected_in);
        assert_eq!(out_handle as usize, expected_out);
        assert!(!in_handle.is_null());
        assert!(!out_handle.is_null());

        unsafe {
            CloseHandle(in_handle);
            CloseHandle(out_handle);
        }
    }
}
