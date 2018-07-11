//! Data structure for passing application memory to the kernel.

use core::marker::PhantomData;
use core::ops::{Deref, DerefMut};
use core::ptr::Unique;
use core::slice;

use callback::AppId;
use sched::Kernel;

#[derive(Debug)]
pub struct Private;
#[derive(Debug)]
pub struct Shared;

pub struct AppPtr<L, T, S> {
    kernel: &'static Kernel<S>,
    ptr: Unique<T>,
    process: AppId<S>,
    _phantom: PhantomData<L>,
}

impl<L, T, S> AppPtr<L, T, S> {
    unsafe fn new(kernel: &'static Kernel<S>, ptr: *mut T, appid: AppId<S>) -> AppPtr<L, T, S> {
        AppPtr {
            kernel: kernel,
            ptr: Unique::new_unchecked(ptr),
            process: appid,
            _phantom: PhantomData,
        }
    }
}

impl<L, T, S> Deref for AppPtr<L, T, S> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { self.ptr.as_ref() }
    }
}

impl<L, T, S> DerefMut for AppPtr<L, T, S> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { self.ptr.as_mut() }
    }
}

impl<L, T, S> Drop for AppPtr<L, T, S> {
    fn drop(&mut self) {
        self.kernel
            .process_map_or((), self.process.idx(), |process| unsafe {
                process.free(self.ptr.as_mut())
            })
    }
}

pub struct AppSlice<L, T, S> {
    kernel: &'static Kernel<S>,
    ptr: AppPtr<L, T, S>,
    len: usize,
}

impl<L, T, S> AppSlice<L, T, S> {
    crate fn new(kernel: &'static Kernel<S>, ptr: *mut T, len: usize, appid: AppId<S>) -> AppSlice<L, T, S> {
        unsafe {
            AppSlice {
                kernel: kernel,
                ptr: AppPtr::new(kernel, ptr, appid),
                len: len,
            }
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn ptr(&self) -> *const T {
        self.ptr.ptr.as_ptr()
    }

    crate unsafe fn expose_to(&self, appid: AppId<S>) -> bool {
        if appid.idx() != self.ptr.process.idx() {
            self.kernel.process_map_or(false, appid.idx(), |process| {
                process.add_mpu_region(self.ptr() as *const u8, self.len() as u32)
            })
        } else {
            false
        }
    }

    pub fn iter(&self) -> slice::Iter<T> {
        self.as_ref().iter()
    }

    pub fn iter_mut(&mut self) -> slice::IterMut<T> {
        self.as_mut().iter_mut()
    }

    pub fn chunks(&self, size: usize) -> slice::Chunks<T> {
        self.as_ref().chunks(size)
    }

    pub fn chunks_mut(&mut self, size: usize) -> slice::ChunksMut<T> {
        self.as_mut().chunks_mut(size)
    }
}

impl<L, T, S> AsRef<[T]> for AppSlice<L, T, S> {
    fn as_ref(&self) -> &[T] {
        unsafe { slice::from_raw_parts(self.ptr.ptr.as_ref(), self.len) }
    }
}

impl<L, T, S> AsMut<[T]> for AppSlice<L, T, S> {
    fn as_mut(&mut self) -> &mut [T] {
        unsafe { slice::from_raw_parts_mut(self.ptr.ptr.as_mut(), self.len) }
    }
}
