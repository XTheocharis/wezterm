//! `MasterPty` and `Child` adapters wrapping the raw Win32 handles
//! conhost delivers during the termhost handoff. See `master.rs` for
//! the ConPTY handle layout.

mod child;
mod io;
mod master;

pub use child::TermHostChild;
pub use io::create_anon_pipe;
pub use master::RawHandlesMasterPty;
