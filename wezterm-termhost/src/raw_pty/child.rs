use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::sync::Mutex;

use filedescriptor::OwnedHandle as FileOwnedHandle;
use portable_pty::{Child, ChildKiller, ExitStatus};
use winapi::shared::ntdef::HANDLE;

use super::io::dup_handle;

// No `SlavePty` is exposed: the slave end of the ConPTY pair is owned by
// the originally-launched CLI app (cmd.exe, pwsh.exe, etc.) and we have
// no way to spawn new processes into it. `LocalPane` only needs
// `MasterPty` for an already-spawned child.

/// `Child` wrapping the PTY session process handle delivered as `server`
/// by conhost. We wait on the PTY server so the pane stays alive until
/// the ConPTY session ends, even if the initial client process exits and
/// another console client continues running. The initial client PID is
/// still kept for process metadata, and `kill()` continues to use a
/// terminate handle opened from that client PID.
pub struct TermHostChild {
    session_handle: Mutex<Option<FileOwnedHandle>>,
    killer_handle: Mutex<Option<FileOwnedHandle>>,
    client_pid: Option<u32>,
}

/// Open a fresh handle to `pid` with `PROCESS_TERMINATE` access only.
/// Returns `None` on failure — typically a race: the process already
/// exited between handoff and this call, or the caller lacks rights.
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

fn terminate_via(handle: Option<&FileOwnedHandle>) -> io::Result<()> {
    use winapi::um::processthreadsapi::TerminateProcess;
    if let Some(h) = handle {
        let ok = unsafe { TerminateProcess(h.as_raw_handle() as HANDLE, 1) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
    } else {
        log::debug!("kill() called but no killer handle; process likely already exited");
    }
    Ok(())
}

impl TermHostChild {
    /// # Safety
    ///
    /// `session_handle` must be a valid process handle with at least
    /// `SYNCHRONIZE` access. See struct doc for the lifetime split.
    pub unsafe fn from_raw(session_handle: HANDLE, client_pid: Option<u32>) -> Self {
        if session_handle.is_null() {
            // Preserve original silent-null behavior; the PID still
            // allows `kill()` to try to terminate the client.
            return Self {
                session_handle: Mutex::new(None),
                killer_handle: Mutex::new(client_pid.and_then(open_terminate_handle)),
                client_pid,
            };
        }
        let owned = match dup_handle(session_handle) {
            Some(raw) => Some(unsafe { FileOwnedHandle::from_raw_handle(raw) }),
            None => {
                log::warn!(
                    "dup_handle failed for pid {:?}; try_wait/wait will fail",
                    client_pid
                );
                None
            }
        };
        let killer = client_pid.and_then(open_terminate_handle);
        Self {
            session_handle: Mutex::new(owned),
            killer_handle: Mutex::new(killer),
            client_pid,
        }
    }
}

impl std::fmt::Debug for TermHostChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermHostChild")
            .field("client_pid", &self.client_pid)
            .finish()
    }
}

impl Child for TermHostChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let guard = self.session_handle.lock().unwrap();
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
        // Clone the handle first so we can drop the lock before the
        // blocking wait — otherwise `kill()` would deadlock waiting for
        // this lock. Matches the pattern in `pty::win::WinChild::wait`.
        let clone = {
            let guard = self.session_handle.lock().unwrap();
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
        if let Some(ref c) = clone {
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
                if clone.is_none() {
                    "wait() called with no session handle; child is unwaitable"
                } else {
                    "WaitForSingleObject returned but try_wait found no exit status"
                },
            )),
        }
    }

    fn process_id(&self) -> Option<u32> {
        self.client_pid
    }

    fn as_raw_handle(&self) -> Option<RawHandle> {
        self.session_handle
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.as_raw_handle())
    }
}

impl ChildKiller for TermHostChild {
    fn kill(&mut self) -> io::Result<()> {
        let guard = self.killer_handle.lock().unwrap();
        terminate_via(guard.as_ref())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(TermHostKiller::dup_from(&self.killer_handle))
    }
}

#[derive(Debug)]
struct TermHostKiller {
    handle: Mutex<Option<FileOwnedHandle>>,
}

impl TermHostKiller {
    // Duplicate rather than share, so each killer owns an independent
    // kernel handle — otherwise one Drop would close the shared handle
    // and invalidate all other clones.
    fn dup_from(src: &Mutex<Option<FileOwnedHandle>>) -> Self {
        let dup = src
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|h| h.try_clone().ok());
        TermHostKiller {
            handle: Mutex::new(dup),
        }
    }
}

impl ChildKiller for TermHostKiller {
    fn kill(&mut self) -> io::Result<()> {
        terminate_via(self.handle.lock().unwrap().as_ref())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(TermHostKiller::dup_from(&self.handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::Duration;

    fn spawn_cmd(command: &str) -> std::process::Child {
        Command::new("cmd")
            .args(["/C", command])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd")
    }

    #[test]
    fn wait_tracks_session_server_instead_of_initial_client() {
        use std::os::windows::io::AsRawHandle;
        use winapi::shared::ntdef::HANDLE;

        let mut server = spawn_cmd("for /L %i in (1,1,1000000000) do @rem");
        let mut client = spawn_cmd("exit /B 0");

        let client_pid = Some(client.id());
        let server_handle = AsRawHandle::as_raw_handle(&server) as HANDLE;
        let mut child = unsafe { TermHostChild::from_raw(server_handle, client_pid) };

        assert_eq!(child.process_id(), client_pid);

        let client_status = client.wait().expect("client wait");
        assert!(client_status.success());

        thread::sleep(Duration::from_millis(100));
        assert!(child.try_wait().expect("try_wait").is_none());

        server.kill().expect("server kill");
        let _ = server.wait().expect("server wait");

        let child_status = child.wait().expect("child wait");
        assert_eq!(child_status.exit_code(), 1);
    }
}
