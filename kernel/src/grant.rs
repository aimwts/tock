//! Data structure to store a list of userspace applications.

use core::marker::PhantomData;
use core::mem::size_of;
use core::ops::{Deref, DerefMut};
use core::ptr::Unique;

use callback::AppId;
use process::Error;
use sched::Kernel;

pub struct Grant<T: Default> {
    crate kernel: &'static Kernel,
    grant_num: usize,
    ptr: PhantomData<T>,
}

pub struct AppliedGrant<T> {
    appid: AppId,
    grant: *mut T,
    _phantom: PhantomData<T>,
}

impl<T> AppliedGrant<T> {
    pub fn enter<F, R>(self, fun: F) -> R
    where
        F: FnOnce(&mut Owned<T>, &mut Allocator) -> R,
        R: Copy,
    {
        let mut allocator = Allocator { appid: self.appid };
        let mut root = unsafe { Owned::new(self.grant, self.appid) };
        fun(&mut root, &mut allocator)
    }
}

pub struct Allocator {
    appid: AppId,
}

pub struct Owned<T: ?Sized> {
    data: Unique<T>,
    appid: AppId,
}

impl<T: ?Sized> Owned<T> {
    unsafe fn new(data: *mut T, appid: AppId) -> Owned<T> {
        Owned {
            data: Unique::new_unchecked(data),
            appid: appid,
        }
    }

    pub fn appid(&self) -> AppId {
        self.appid
    }
}

impl<T: ?Sized> Drop for Owned<T> {
    fn drop(&mut self) {
        unsafe {
            let data = self.data.as_ptr() as *mut u8;
            self.appid.process.free(data);
        }
    }
}

impl<T: ?Sized> Deref for Owned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { self.data.as_ref() }
    }
}

impl<T: ?Sized> DerefMut for Owned<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { self.data.as_mut() }
    }
}

impl Allocator {
    pub fn alloc<T>(&mut self, data: T) -> Result<Owned<T>, Error> {
        unsafe {
            self.appid
                .process
                .alloc(size_of::<T>())
                .map_or(Err(Error::OutOfMemory), |arr| {
                    let mut owned = Owned::new(arr.as_mut_ptr() as *mut T, self.appid);
                    *owned = data;
                    Ok(owned)
                })
        }
    }
}

pub struct Borrowed<'a, T: 'a + ?Sized> {
    data: &'a mut T,
    appid: AppId,
}

impl<T: 'a + ?Sized> Borrowed<'a, T> {
    pub fn new(data: &'a mut T, appid: AppId) -> Borrowed<'a, T> {
        Borrowed {
            data: data,
            appid: appid,
        }
    }

    pub fn appid(&self) -> AppId {
        self.appid
    }
}

impl<T: 'a + ?Sized> Deref for Borrowed<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.data
    }
}

impl<T: 'a + ?Sized> DerefMut for Borrowed<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.data
    }
}

impl<T: Default> Grant<T> {
    crate fn new(kernel: &'static Kernel, grant_index: usize) -> Grant<T> {
        Grant {
            kernel: kernel,
            grant_num: grant_index,
            ptr: PhantomData,
        }
    }

    pub fn grant(&self, appid: AppId) -> Option<AppliedGrant<T>> {
        unsafe {
            let cntr = appid.process.grant_for::<T>(self.grant_num);
            if cntr.is_null() {
                None
            } else {
                Some(AppliedGrant {
                    appid: appid,
                    grant: cntr,
                    _phantom: PhantomData,
                })
            }
        }
    }

    pub fn enter<F, R>(&self, appid: AppId, fun: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Borrowed<T>, &mut Allocator) -> R,
        R: Copy,
    {
        unsafe {
            appid
                .process
                .grant_for_or_alloc::<T>(self.grant_num)
                .map_or(Err(Error::OutOfMemory), move |root_ptr| {
                    let mut root = Borrowed::new(&mut *root_ptr, appid);
                    let mut allocator = Allocator { appid: appid };
                    let res = fun(&mut root, &mut allocator);
                    Ok(res)
                })
        }
    }

    pub fn each<F>(&self, fun: F)
    where
        F: Fn(&mut Owned<T>),
    {
        self.kernel
            .process_each_enumerate(|app_id, process| unsafe {
                let root_ptr = process.grant_for::<T>(self.grant_num);
                if !root_ptr.is_null() {
                    let mut root = Owned::new(root_ptr, AppId::new(process, app_id));
                    fun(&mut root);
                }
            });
    }

    pub fn iter(&self) -> Iter<T> {
        Iter {
            grant: self,
            index: 0,
            len: self.kernel.number_of_process_slots(),
        }
    }
}

pub struct Iter<'a, T: 'a + Default> {
    grant: &'a Grant<T>,
    index: usize,
    len: usize,
}

impl<T: Default> Iterator for Iter<'a, T> {
    type Item = AppliedGrant<T>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.len {
            let idx = self.index;
            self.index += 1;
            let res = self.grant.kernel.process_map_or(None, idx, |process| {
                self.grant.grant(AppId::new(process, idx))
            });
            if res.is_some() {
                return res;
            }
        }
        None
    }
}
