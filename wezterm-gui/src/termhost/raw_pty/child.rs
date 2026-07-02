use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::sync::Mutex;

use filedescriptor::OwnedHandle as FileOwnedHandle;
use portable_pty::{Child, ChildKiller, ExitStatus};
use winapi::shared::ntdef::HANDLE;

use super::io::dup_handle;

// No `SlavePty` is exposed: the slave end is owned by the
// originally-launched CLI app (cmd.exe, pwsh.exe, etc.) and we have no
// way to spawn new processes into it. `LocalPane` only needs
// `MasterPty` for an already-spawned child.

/// `Child` wrapping the originally-launched CLI app's process handle
/// (which conhost passes us as `client`). Uses
/// `filedescriptor::OwnedHandle` (not std's) for `try_clone()`, same
/// pattern as `pty::win::WinChild`/`WinChildKiller`.
///
/// The COM `client` handle conhost hands us only grants `SYNCHRONIZE`
/// and `PROCESS_QUERY_LIMITED_INFORMATION` — enough to wait on the
/// process but NOT enough for `TerminateProcess` (which needs
/// `PROCESS_TERMINATE`). We therefore keep two handles:
///
///   * `handle`         — duplicated from `client`, used to wait.
///   * `killer_handle`  — opened separately via `OpenProcess` using the
///                        client pid, with `PROCESS_TERMINATE` access.
///
/// When the pid is unavailable (or `OpenProcess` loses the race with an
/// already-exited process), `killer_handle` is `None` and `kill()`
/// becomes a logged no-op.
pub struct TermHostChild {
    handle: Mutex<Option<FileOwnedHandle>>,
    killer_handle: Mutex<Option<FileOwnedHandle>>,
    pid: Option<u32>,
}

/// Open a fresh handle to `pid` with `PROCESS_TERMINATE` access only.
/// Returns `None` on failure (typically a race: the process already
/// exited between handoff and this call, or the caller lacks rights).
fn open_terminate_handle(pid: u32) -> Option<FileOwnedHandle> {
    use winapi::um::processthreadsapi::OpenProcess;
    use winapi::um::winnt::PROCESS_TERMINATE;
    let raw = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if raw.is_null() {
        log::warn!(
            "OpenProcess(PROCESS_TERMINATE) failed for pid {}; \
             child unkillable: {}",
            pid,
            io::Error::last_os_error()
        );
        None
    } else {
        Some(unsafe { FileOwnedHandle::from_raw_handle(raw as RawHandle) })
    }
}

impl TermHostChild {
    /// # Safety
    ///
    /// `handle` must be a valid process handle with `SYNCHRONIZE` and
    /// `PROCESS_QUERY_LIMITED_INFORMATION` access, owned by the caller.
    /// Those rights are sufficient for waiting but not for
    /// `TerminateProcess`; the killer handle is opened separately from
    /// `pid` (when available) with `PROCESS_TERMINATE` access.
    pub unsafe fn from_raw(handle: HANDLE, pid: Option<u32>) -> Self {
        if handle.is_null() {
            // Preserve original silent-null behavior; pid alone can still kill.
            return Self {
                handle: Mutex::new(None),
                killer_handle: Mutex::new(pid.and_then(open_terminate_handle)),
                pid,
            };
        }
        let owned = match dup_handle(handle) {
            Some(raw) => Some(unsafe { FileOwnedHandle::from_raw_handle(raw) }),
            None => {
                log::warn!(
                    "dup_handle failed for pid {:?}; try_wait/wait will fail",
                    pid
                );
                None
            }
        };
        let killer = pid.and_then(open_terminate_handle);
        Self {
            handle: Mutex::new(owned),
            killer_handle: Mutex::new(killer),
            pid,
        }
    }
}

impl std::fmt::Debug for TermHostChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermHostChild")
            .field("pid", &self.pid)
            .finish()
    }
}

impl Child for TermHostChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let guard = self.handle.lock().unwrap();
        let owned = match guard.as_ref() {
            Some(h) => h,
            None => {
                log::warn!("try_wait called with no valid handle; assuming still running");
                return Ok(None);
            }
        };
        let raw = owned.as_raw_handle() as HANDLE;
        use winapi::um::synchapi::WaitForSingleObject;
        let r = unsafe { WaitForSingleObject(raw, 0) };
        if r == winapi::um::winbase::WAIT_OBJECT_0 {
            use winapi::um::processthreadsapi::GetExitCodeProcess;
            let mut code: u32 = 0;
            let ok = unsafe { GetExitCodeProcess(raw, &mut code) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Some(ExitStatus::with_exit_code(code)))
        } else if r == winapi::shared::winerror::WAIT_TIMEOUT {
            Ok(None)
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        // Clone first so we can drop the lock before the blocking wait —
        // otherwise `kill()` would deadlock. Matches `pty::win::WinChild::wait`.
        let clone = {
            let guard = self.handle.lock().unwrap();
            match guard.as_ref() {
                None => None,
                Some(h) => Some(h.try_clone().map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::Other,
                        format!("DuplicateHandle for wait() failed: {e}"),
                    )
                })?),
            }
        };
        if let Some(c) = clone {
            use winapi::um::synchapi::WaitForSingleObject;
            use winapi::um::winbase::{INFINITE, WAIT_FAILED};
            let raw = c.as_raw_handle() as HANDLE;
            let r = unsafe { WaitForSingleObject(raw, INFINITE) };
            if r == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
        }
        match self.try_wait()? {
            Some(status) => Ok(status),
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "WaitForSingleObject returned but try_wait found no exit status",
            )),
        }
    }

    fn process_id(&self) -> Option<u32> {
        self.pid
    }

    fn as_raw_handle(&self) -> Option<RawHandle> {
        self.handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.as_raw_handle())
    }
}

impl ChildKiller for TermHostChild {
    fn kill(&mut self) -> io::Result<()> {
        let guard = self.killer_handle.lock().unwrap();
        if let Some(ref h) = *guard {
            use winapi::um::processthreadsapi::TerminateProcess;
            let ok = unsafe { TerminateProcess(h.as_raw_handle() as HANDLE, 1) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
        } else {
            log::debug!("kill() called but no killer handle; process likely already exited");
        }
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        let dup = self
            .killer_handle
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| h.try_clone().ok());
        Box::new(TermHostKiller {
            handle: Mutex::new(dup),
        })
    }
}

#[derive(Debug)]
struct TermHostKiller {
    handle: Mutex<Option<FileOwnedHandle>>,
}

impl ChildKiller for TermHostKiller {
    fn kill(&mut self) -> io::Result<()> {
        if let Some(ref h) = *self.handle.lock().unwrap() {
            use winapi::um::processthreadsapi::TerminateProcess;
            let ok = unsafe { TerminateProcess(h.as_raw_handle() as HANDLE, 1) };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
        } else {
            log::debug!("kill() called but no killer handle; process likely already exited");
        }
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        // Duplicate rather than share, so each killer owns an independent
        // kernel handle — otherwise one Drop would close the shared handle.
        let dup = self
            .handle
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| h.try_clone().ok());
        Box::new(TermHostKiller {
            handle: Mutex::new(dup),
        })
    }
}
