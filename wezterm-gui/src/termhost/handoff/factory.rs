//! Class factory that produces handoff instances.
//!
//! `CoCreateInstance` queries the registered class for
//! `IID_IClassFactory` first, so we register this factory rather than
//! the singleton `Instance` directly.

// Vtable field names are mandated by the COM ABI / MIDL convention.
#![allow(non_snake_case)]

use std::ffi::c_void;

use parking_lot::Mutex;
use winapi::shared::guiddef::GUID;
use winapi::shared::winerror::{E_NOINTERFACE, E_POINTER, S_OK};

use super::super::com_interfaces::{IID_IClassFactory, IID_IUNKNOWN};
use super::instance::{guid_eq, query_interface, take_singleton};

// winerror.h: `CLASS_E_NOAGGREGATION = 0x80040110`.
const CLASS_E_NOAGGREGATION: i32 = 0x80040110u32 as i32;

#[repr(C)]
struct ClassFactoryVtable {
    QueryInterface: unsafe extern "system" fn(
        This: *mut c_void,
        iid: *const GUID,
        interface: *mut *mut c_void,
    ) -> i32,
    AddRef: unsafe extern "system" fn(This: *mut c_void) -> u32,
    Release: unsafe extern "system" fn(This: *mut c_void) -> u32,
    CreateInstance: unsafe extern "system" fn(
        This: *mut c_void,
        punk_outer: *mut c_void,
        iid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> i32,
    LockServer: unsafe extern "system" fn(This: *mut c_void, flock: i32) -> i32,
}

/// COM activation goes through a class factory; the factory hands out
/// the singleton `Instance`.
#[repr(C)]
struct ClassFactory {
    vtable: *const ClassFactoryVtable,
}

unsafe impl Send for ClassFactory {}
unsafe impl Sync for ClassFactory {}

unsafe extern "system" fn factory_query_interface(
    this: *mut c_void,
    iid: *const GUID,
    interface: *mut *mut c_void,
) -> i32 {
    if iid.is_null() || interface.is_null() {
        return E_POINTER;
    }
    let requested = &*iid;
    if guid_eq(requested, &IID_IClassFactory) || guid_eq(requested, &IID_IUNKNOWN) {
        *interface = this;
        factory_add_ref(this);
        S_OK
    } else {
        *interface = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

// Singleton factory: refcount is a stable 1 (process lifetime).
unsafe extern "system" fn factory_add_ref(_this: *mut c_void) -> u32 {
    1
}

unsafe extern "system" fn factory_release(_this: *mut c_void) -> u32 {
    1
}

unsafe extern "system" fn factory_create_instance(
    _this: *mut c_void,
    punk_outer: *mut c_void,
    iid: *const GUID,
    ppv: *mut *mut c_void,
) -> i32 {
    if !punk_outer.is_null() {
        if !ppv.is_null() {
            *ppv = std::ptr::null_mut();
        }
        return CLASS_E_NOAGGREGATION;
    }
    if iid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    let inst = take_singleton();
    query_interface(inst, iid, ppv)
}

unsafe extern "system" fn factory_lock_server(_this: *mut c_void, _flock: i32) -> i32 {
    S_OK
}

static FACTORY_VTABLE: ClassFactoryVtable = ClassFactoryVtable {
    QueryInterface: factory_query_interface,
    AddRef: factory_add_ref,
    Release: factory_release,
    CreateInstance: factory_create_instance,
    LockServer: factory_lock_server,
};

static CLASS_FACTORY: Mutex<Option<Box<ClassFactory>>> = Mutex::new(None);

pub(crate) fn take_factory() -> *mut c_void {
    let mut guard = CLASS_FACTORY.lock();
    if guard.is_none() {
        let cf = Box::new(ClassFactory {
            vtable: &FACTORY_VTABLE,
        });
        *guard = Some(cf);
    }
    guard.as_mut().unwrap().as_mut() as *mut ClassFactory as *mut c_void
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;
    use crate::termhost::com_interfaces::IID_ITerminalHandoff3;

    #[test]
    fn factory_query_interface_iid_iclassfactory_returns_s_ok() {
        let factory = take_factory();
        let mut interface: *mut c_void = std::ptr::null_mut();
        let hr =
            unsafe { (FACTORY_VTABLE.QueryInterface)(factory, &IID_IClassFactory, &mut interface) };
        assert_eq!(hr, S_OK);
        assert!(!interface.is_null());
    }

    #[test]
    fn factory_query_interface_iid_iunknown_returns_s_ok() {
        let factory = take_factory();
        let mut interface: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { (FACTORY_VTABLE.QueryInterface)(factory, &IID_IUNKNOWN, &mut interface) };
        assert_eq!(hr, S_OK);
        assert!(!interface.is_null());
    }

    #[test]
    fn factory_create_instance_returns_terminal_handoff() {
        let factory = take_factory();
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            (FACTORY_VTABLE.CreateInstance)(
                factory,
                std::ptr::null_mut(),
                &IID_ITerminalHandoff3,
                &mut ppv,
            )
        };
        assert_eq!(hr, S_OK);
        assert!(!ppv.is_null());
    }

    #[test]
    fn factory_create_instance_with_aggregation_returns_noaggregation() {
        let factory = take_factory();
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let dummy_outer = 1usize;
        let hr = unsafe {
            (FACTORY_VTABLE.CreateInstance)(
                factory,
                &dummy_outer as *const _ as *mut c_void,
                &IID_ITerminalHandoff3,
                &mut ppv,
            )
        };
        assert_eq!(hr, CLASS_E_NOAGGREGATION);
        assert!(ppv.is_null());
    }

    #[test]
    fn factory_query_interface_null_iid_returns_pointer() {
        let factory = take_factory();
        let mut interface: *mut c_void = std::ptr::null_mut();
        let hr =
            unsafe { (FACTORY_VTABLE.QueryInterface)(factory, std::ptr::null(), &mut interface) };
        assert_eq!(hr, E_POINTER);
        assert!(interface.is_null());
    }

    #[test]
    fn factory_query_interface_null_out_returns_pointer() {
        let factory = take_factory();
        let hr = unsafe {
            (FACTORY_VTABLE.QueryInterface)(factory, &IID_IClassFactory, std::ptr::null_mut())
        };
        assert_eq!(hr, E_POINTER);
    }

    #[test]
    fn factory_query_interface_unknown_iid_returns_nointerface() {
        const UNKNOWN_IID: GUID = GUID {
            Data1: 0xDEADBEEF,
            Data2: 0xBEEF,
            Data3: 0xDEAD,
            Data4: [0, 0, 0, 0, 0, 0, 0, 0],
        };
        let factory = take_factory();
        let mut interface: *mut c_void = std::ptr::null_mut();
        let hr = unsafe { (FACTORY_VTABLE.QueryInterface)(factory, &UNKNOWN_IID, &mut interface) };
        assert_eq!(hr, E_NOINTERFACE);
        assert!(interface.is_null());
    }

    #[test]
    fn factory_add_ref_returns_one() {
        let factory = take_factory();
        let count = unsafe { (FACTORY_VTABLE.AddRef)(factory) };
        assert_eq!(count, 1);
    }

    #[test]
    fn factory_release_returns_one() {
        let factory = take_factory();
        let count = unsafe { (FACTORY_VTABLE.Release)(factory) };
        assert_eq!(count, 1);
    }

    #[test]
    fn factory_lock_server_returns_s_ok() {
        let factory = take_factory();
        let hr_lock = unsafe { (FACTORY_VTABLE.LockServer)(factory, 1) };
        let hr_unlock = unsafe { (FACTORY_VTABLE.LockServer)(factory, 0) };
        assert_eq!(hr_lock, S_OK);
        assert_eq!(hr_unlock, S_OK);
    }

    #[test]
    fn factory_create_instance_null_iid_returns_pointer() {
        let factory = take_factory();
        let mut ppv: *mut c_void = std::ptr::null_mut();
        let hr = unsafe {
            (FACTORY_VTABLE.CreateInstance)(
                factory,
                std::ptr::null_mut(),
                std::ptr::null(),
                &mut ppv,
            )
        };
        assert_eq!(hr, E_POINTER);
    }

    #[test]
    fn factory_create_instance_null_ppv_returns_pointer() {
        let factory = take_factory();
        let hr = unsafe {
            (FACTORY_VTABLE.CreateInstance)(
                factory,
                std::ptr::null_mut(),
                &IID_ITerminalHandoff3,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(hr, E_POINTER);
    }
}
