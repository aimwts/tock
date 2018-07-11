//! Data structure to store a list of userspace applications.

use core::marker::PhantomData;
use core::mem::size_of;
use core::ops::{Deref, DerefMut};
use core::ptr::{read_volatile, write_volatile, Unique};

use callback::AppId;
use process::Error;
use sched::Kernel;
use syscall;

crate static mut CONTAINER_COUNTER: usize = 0;

pub struct Grant<T: Default, S> {
    kernel: &'static Kernel<S>,
    grant_num: usize,
    ptr: PhantomData<T>,
}

pub struct AppliedGrant<T, S> {
    kernel: &'static Kernel<S>,
    appid: usize,
    grant: *mut T,
    _phantom: PhantomData<T>,
}

impl AppliedGrant<T, S> {
    pub fn enter<F, R, S>(self, fun: F) -> R
    where
        F: FnOnce(&mut Owned<T, S>, &mut Allocator<S>) -> R,
        R: Copy,
    {
        let mut allocator = Allocator {
            kernel: self.kernel,
            app_id: self.appid,
        };
        let mut root = unsafe { Owned::new(self.kernel, self.grant, self.appid) };
        fun(&mut root, &mut allocator)
    }
}

pub struct Allocator<S> {
    kernel: &'static Kernel<S>,
    app_id: usize,
}

pub struct Owned<T: ?Sized, S> {
    kernel: &'static Kernel<S>,
    data: Unique<T>,
    app_id: usize,
}

impl<T: ?Sized, S> Owned<T, S> {
    unsafe fn new(kernel: &'static Kernel<S>, data: *mut T, app_id: usize) -> Owned<T, S> {
        Owned {
            kernel: kernel,
            data: Unique::new_unchecked(data),
            app_id: app_id,
        }
    }

    pub fn appid(&self) -> AppId<S> {
        AppId::new(self.kernel, self.app_id)
    }
}

impl<T: ?Sized, S> Drop for Owned<T, S> {
    fn drop(&mut self) {
        unsafe {
            let app_id = self.app_id;
            let data = self.data.as_ptr() as *mut u8;
            self.kernel.process_map_or((), app_id, |process| {
                process.free(data);
            });
        }
    }
}

impl<T: ?Sized, S> Deref for Owned<T, S> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { self.data.as_ref() }
    }
}

impl<T: ?Sized, S> DerefMut for Owned<T, S> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { self.data.as_mut() }
    }
}

impl Allocator<S> {
    pub fn alloc<T>(&mut self, data: T) -> Result<Owned<T, S>, Error> {
        unsafe {
            let app_id = self.app_id;
            self.kernel
                .process_map_or(Err(Error::NoSuchApp), app_id, |process| {
                    process
                        .alloc(size_of::<T>())
                        .map_or(Err(Error::OutOfMemory), |arr| {
                            let mut owned =
                                Owned::new(self.kernel, arr.as_mut_ptr() as *mut T, app_id);
                            *owned = data;
                            Ok(owned)
                        })
                })
        }
    }
}

pub struct Borrowed<'a, T: 'a + ?Sized, S> {
    kernel: &'static Kernel<S>,
    data: &'a mut T,
    app_id: usize,
}

impl<T: 'a + ?Sized, S> Borrowed<'a, T, S> {
    pub fn new(kernel: &'static Kernel<S>, data: &'a mut T, app_id: usize) -> Borrowed<'a, T, S> {
        Borrowed {
            kernel: kernel,
            data: data,
            app_id: app_id,
        }
    }

    pub fn appid(&self) -> AppId<S> {
        AppId::new(self.kernel, self.app_id)
    }
}

impl<T: 'a + ?Sized, S> Deref for Borrowed<'a, T, S> {
    type Target = T;
    fn deref(&self) -> &T {
        self.data
    }
}

impl<T: 'a + ?Sized, S> DerefMut for Borrowed<'a, T, S> {
    fn deref_mut(&mut self) -> &mut T {
        self.data
    }
}

impl<T: Default, S> Grant<T, S> {
    pub unsafe fn create(kernel: &'static Kernel<S>) -> Grant<T, S> {
        let ctr = read_volatile(&CONTAINER_COUNTER);
        write_volatile(&mut CONTAINER_COUNTER, ctr + 1);
        Grant {
            kernel: kernel,
            grant_num: ctr,
            ptr: PhantomData,
        }
    }

    pub fn grant(&self, appid: AppId<S>) -> Option<AppliedGrant<T, S>> {
        unsafe {
            let app_id = appid.idx();
            self.kernel.process_map_or(None, app_id, |process| {
                let cntr = process.grant_for::<T>(self.grant_num);
                if cntr.is_null() {
                    None
                } else {
                    Some(AppliedGrant {
                        kernel: self.kernel,
                        appid: app_id,
                        grant: cntr,
                        _phantom: PhantomData,
                    })
                }
            })
        }
    }

    pub fn enter<F, R, S>(&self, appid: AppId<S>, fun: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Borrowed<T, S>, &mut Allocator<S>) -> R,
        R: Copy,
    {
        unsafe {
            let app_id = appid.idx();
            self.kernel
                .process_map_or(Err(Error::NoSuchApp), app_id, |process| {
                    process.grant_for_or_alloc::<T>(self.grant_num).map_or(
                        Err(Error::OutOfMemory),
                        move |root_ptr| {
                            let mut root = Borrowed::new(self.kernel, &mut *root_ptr, app_id);
                            let mut allocator = Allocator {
                                kernel: self.kernel,
                                app_id: app_id,
                            };
                            let res = fun(&mut root, &mut allocator);
                            Ok(res)
                        },
                    )
                })
        }
    }

    pub fn each<F>(&self, fun: F)
    where
        F: Fn(&mut Owned<T, S>),
    {
        self.kernel
            .process_each_enumerate(|app_id, process| unsafe {
                let root_ptr = process.grant_for::<T>(self.grant_num);
                if !root_ptr.is_null() {
                    let mut root = Owned::new(self.kernel, root_ptr, app_id);
                    fun(&mut root);
                }
            });
    }

    pub fn iter(&self) -> Iter<T, S> {
        Iter {
            kernel: self.kernel,
            grant: self,
            index: 0,
            len: self.kernel.number_of_process_slots(),
        }
    }
}

pub struct Iter<'a, T: 'a + Default, S> {
    kernel: &'static Kernel<S>,
    grant: &'a Grant<T, S>,
    index: usize,
    len: usize,
}

impl<T: Default, S> Iterator for Iter<'a, T, S> {
    type Item = AppliedGrant<T, S>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.len {
            let idx = self.index;
            self.index += 1;
            let res = self.grant.grant(AppId::new(self.kernel, idx));
            if res.is_some() {
                return res;
            }
        }
        None
    }
}
