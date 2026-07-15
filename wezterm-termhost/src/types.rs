//! Structs matching `microsoft/terminal`'s
//! `src/host/proxy/ITerminalHandoff.idl`.
//!
//! Must be `#[repr(C)]` with field order matching the IDL — the COM
//! proxy stub (the marshalling glue that serializes the handoff call
//! across process boundaries) copies these structs byte-for-byte, so
//! any layout drift corrupts the handoff.

#![allow(non_snake_case, non_camel_case_types)]

use winapi::shared::wtypes::BSTR;
use winapi::um::oleauto::SysStringLen;

/// `TERMINAL_STARTUP_INFO` — a subset of Windows' `STARTUPINFO` struct
/// (window-position and show hints for a new process) passed across COM
/// in `ITerminalHandoff3::EstablishPtyHandoff`. IDL lines 6-25.
///
/// The `*const u16` fields are BSTRs on the wire — COM's length-prefixed
/// wide-string type. The proxy stub owns the allocation and frees it
/// after the call returns, so we read but do not free.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TerminalStartupInfo {
    pub pszTitle: *const u16,
    pub pszIconPath: *const u16,
    pub iconIndex: i32,
    pub dwX: u32,
    pub dwY: u32,
    pub dwXSize: u32,
    pub dwYSize: u32,
    pub dwXCountChars: u32,
    pub dwYCountChars: u32,
    pub dwFillAttribute: u32,
    pub dwFlags: u32,
    pub wShowWindow: u16,
}

/// Copy a BSTR pointer (COM's length-prefixed wide string) into an
/// owned `String`, returning an empty `String` if null. Invalid UTF-16
/// surrogates become U+FFFD via `from_utf16_lossy` — conhost never
/// produces invalid UTF-16 in practice, but lossy decoding avoids
/// silently dropping the title if it ever does. Does not free the
/// BSTR; the proxy stub owns the allocation.
///
/// # Safety
///
/// `ptr` must be null or a valid BSTR (a pointer returned by COM's
/// `SysAllocString`).
pub unsafe fn bstr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let len_utf16 = SysStringLen(ptr as BSTR) as usize;
    let slice = std::slice::from_raw_parts(ptr, len_utf16);
    String::from_utf16_lossy(slice)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use winapi::shared::wtypesbase::OLECHAR;
    use winapi::um::oleauto::{SysAllocString, SysFreeString};

    #[test]
    fn null_returns_empty() {
        assert_eq!(unsafe { bstr_to_string(std::ptr::null()) }, String::new());
    }

    #[test]
    fn round_trip_ascii() {
        let mut wide: Vec<u16> = "hello".encode_utf16().collect();
        wide.push(0);
        unsafe {
            let bstr: BSTR = SysAllocString(wide.as_ptr() as *const OLECHAR);
            assert!(!bstr.is_null());
            let s = bstr_to_string(bstr as *const u16);
            assert_eq!(s, "hello");
            SysFreeString(bstr);
        }
    }

    #[test]
    fn round_trip_non_ascii() {
        let mut wide: Vec<u16> = "héllo 世界".encode_utf16().collect();
        wide.push(0);
        unsafe {
            let bstr: BSTR = SysAllocString(wide.as_ptr() as *const OLECHAR);
            assert!(!bstr.is_null());
            let s = bstr_to_string(bstr as *const u16);
            assert_eq!(s, "héllo 世界");
            SysFreeString(bstr);
        }
    }
}
