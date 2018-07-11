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

pub struct AppPtr<'a, L, T> {
    kernel: &'a Kernel<'a>,
    ptr: Unique<T>,
    process: AppId<'a>,
    _phantom: PhantomData<L>,
}

impl<L, T> AppPtr<'a, L, T> {
    unsafe fn new(kernel: &'a Kernel<'a>, ptr: *mut T, appid: AppId<'a>) -> AppPtr<'a, L, T> {
        AppPtr {
            kernel: kernel,
            ptr: Unique::new_unchecked(ptr),
            process: appid,
            _phantom: PhantomData,
        }
    }
}

impl<L, T> Deref for AppPtr<'a, L, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { self.ptr.as_ref() }
    }
}

impl<L, T> DerefMut for AppPtr<'a, L, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { self.ptr.as_mut() }
    }
}

impl<L, T> Drop for AppPtr<'a, L, T> {
    fn drop(&mut self) {
        self.kernel
            .process_map_or((), self.process.idx(), |process| unsafe {
                process.free(self.ptr.as_mut())
            })
    }
}

pub struct AppSlice<'a, L, T> {
    kernel: &'a Kernel<'a>,
    ptr: AppPtr<'a, L, T>,
    len: usize,
}

impl<L, T> AppSlice<'a, L, T> {
    crate fn new(kernel: &'a Kernel<'a>, ptr: *mut T, len: usize, appid: AppId<'a>) -> AppSlice<'a, L, T> {
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

    crate unsafe fn expose_to(&self, appid: AppId) -> bool {
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

impl<L, T> AsRef<[T]> for AppSlice<'a, L, T> {
    fn as_ref(&self) -> &[T] {
        unsafe { slice::from_raw_parts(self.ptr.ptr.as_ref(), self.len) }
    }
}

impl<L, T> AsMut<[T]> for AppSlice<'a, L, T> {
    fn as_mut(&mut self) -> &mut [T] {
        unsafe { slice::from_raw_parts_mut(self.ptr.ptr.as_mut(), self.len) }
    }
}
