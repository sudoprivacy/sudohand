//! Do not leak the caller's original pipe handles into detached Chrome.
//!
//! Rust duplicates selected standard streams when spawning children. Clearing
//! inheritance on the originals preserves intentional redirection, while a
//! browser with null stdio cannot accidentally keep the caller's pipes alive.
#![allow(unsafe_code)] // One Windows API call, before any worker threads start.

use std::{io, os::windows::io::AsRawHandle};
use windows_sys::Win32::Foundation::{
    SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
};

pub fn clear_inheritance() -> io::Result<()> {
    for handle in [
        io::stdin().as_raw_handle(),
        io::stdout().as_raw_handle(),
        io::stderr().as_raw_handle(),
    ] {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: standard stream handles are owned by the process and remain
        // valid here. This changes only HANDLE_FLAG_INHERIT; it neither closes
        // the handle nor accesses memory through it. Called before spawning.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
