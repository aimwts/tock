//! Data structure for storing a callback to userspace or kernelspace.

use core::fmt;
use core::ptr::NonNull;

use process;
use sched::Kernel;

/// Userspace app identifier.
#[derive(Clone, Copy)]
pub struct AppId<S> {
    kernel: &'static Kernel<S>,
    idx: usize,
}

impl<S> PartialEq for AppId<S> {
    fn eq(&self, other: &AppId<S>) -> bool {
        self.idx == other.idx
    }
}

impl<S> Eq for AppId<S> {}

impl<S> fmt::Debug for AppId<S> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.idx)
    }
}

impl<S> AppId<S> {
    crate fn new(kernel: &'static Kernel<S>, idx: usize) -> AppId<S> {
        AppId {
            kernel: kernel,
            idx: idx,
        }
    }

    pub fn idx(&self) -> usize {
        self.idx
    }

    pub fn get_editable_flash_range(&self) -> (usize, usize) {
        self.kernel.process_map_or((0, 0), self.idx, |process| {
            let start = process.flash_non_protected_start() as usize;
            let end = process.flash_end() as usize;
            (start, end)
        })
    }
}

/// Wrapper around a function pointer.
#[derive(Clone, Copy)]
pub struct Callback<S> {
    app_id: AppId<S>,
    appdata: usize,
    fn_ptr: NonNull<*mut ()>,
}

impl<S> Callback<S> {
    crate fn new(appid: AppId<S>, appdata: usize, fn_ptr: NonNull<*mut ()>) -> Callback<S> {
        Callback {
            app_id: appid,
            appdata: appdata,
            fn_ptr: fn_ptr,
        }
    }

    pub fn schedule(&mut self, r0: usize, r1: usize, r2: usize) -> bool {
        self.app_id
            .kernel
            .process_map_or(false, self.app_id.idx(), |process| {
                process.schedule(process::FunctionCall {
                    r0: r0,
                    r1: r1,
                    r2: r2,
                    r3: self.appdata,
                    pc: self.fn_ptr.as_ptr() as usize,
                })
            })
    }
}
