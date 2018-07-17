//! Data structure to store a list of userspace applications.

use core::marker::PhantomData;
use core::mem::size_of;
use core::ops::{Deref, DerefMut};
use core::ptr::{read_volatile, write, write_volatile, Unique};

use callback::AppId;
use process::Error;
use sched::Kernel;

crate static mut CONTAINER_COUNTER: usize = 0;

pub struct Grant<T: Default> {
    kernel: &'static Kernel,
    grant_num: usize,
    ptr: PhantomData<T>,
}

pub struct AppliedGrant<T> {
    kernel: &'static Kernel,
    appid: usize,
    grant: *mut T,
    _phantom: PhantomData<T>,
}

impl<T> AppliedGrant<T> {
    pub fn enter<F, R>(self, fun: F) -> R
    where
        F: FnOnce(&mut Owned<T>, &Allocator) -> R,
        R: Copy,
    {
        let allocator = Allocator {
            kernel: self.kernel,
            app_id: self.appid,
        };
        let mut root = unsafe { Owned::new(self.kernel, self.grant, self.appid) };
        fun(&mut root, &allocator)
    }
}

pub struct Allocator {
    kernel: &'static Kernel,
    app_id: usize,
}

pub struct Owned<T: ?Sized> {
    kernel: &'static Kernel,
    data: Unique<T>,
    app_id: usize,
}

impl<T: ?Sized> Owned<T> {
    unsafe fn new(kernel: &'static Kernel, data: *mut T, app_id: usize) -> Owned<T> {
        Owned {
            kernel: kernel,
            data: Unique::new_unchecked(data),
            app_id: app_id,
        }
    }

    pub fn appid(&self) -> AppId {
        AppId::new(self.kernel, self.app_id)
    }
}

impl<T: ?Sized> Drop for Owned<T> {
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
    pub fn alloc<T>(&self, data: T) -> Result<Owned<T>, Error> {
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

pub struct Borrowed<'a, T: 'a + ?Sized> {
    kernel: &'static Kernel,
    data: &'a mut T,
    app_id: usize,
}

impl<T: 'a + ?Sized> Borrowed<'a, T> {
    pub fn new(kernel: &'static Kernel, data: &'a mut T, app_id: usize) -> Borrowed<'a, T> {
        Borrowed {
            kernel: kernel,
            data: data,
            app_id: app_id,
        }
    }

    pub fn appid(&self) -> AppId {
        AppId::new(self.kernel, self.app_id)
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
    pub unsafe fn create(kernel: &'static Kernel) -> Grant<T> {
        let ctr = read_volatile(&CONTAINER_COUNTER);
        write_volatile(&mut CONTAINER_COUNTER, ctr + 1);
        Grant {
            kernel: kernel,
            grant_num: ctr,
            ptr: PhantomData,
        }
    }

    pub fn grant(&self, appid: AppId) -> Option<AppliedGrant<T>> {
        unsafe {
            let app_id = appid.idx();
            self.kernel.process_map_or(None, app_id, |process| {
                let cntr = *(process.grant_ptr(self.grant_num) as *mut *mut T);
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

    pub fn enter<F, R>(&self, appid: AppId, fun: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Borrowed<T>, &Allocator) -> R,
        R: Copy,
    {
        unsafe {
            let app_id = appid.idx();
            self.kernel
                .process_map_or(Err(Error::NoSuchApp), app_id, |process| {
                    let ctr_ptr = process.grant_ptr(self.grant_num) as *mut *mut T;
                    let new_grant = if (*ctr_ptr).is_null() {
                        process.alloc(size_of::<T>()).map(|root_arr| {
                            let root_ptr = root_arr.as_mut_ptr() as *mut T;
                            // Initialize the grant contents using ptr::write, to
                            // ensure that we don't try to drop the contents of
                            // uninitialized memory when T implements Drop.
                            write(root_ptr, Default::default());
                            // Record the location in the grant pointer.
                            write_volatile(ctr_ptr, root_ptr);
                            root_ptr
                        })
                    } else {
                        Some(*ctr_ptr)
                    };

                    new_grant.map_or(Err(Error::OutOfMemory), move |root_ptr| {
                        let root_ptr = root_ptr as *mut T;
                        let mut root = Borrowed::new(self.kernel, &mut *root_ptr, app_id);
                        let allocator = Allocator {
                            kernel: self.kernel,
                            app_id: app_id,
                        };
                        let res = fun(&mut root, &allocator);
                        Ok(res)
                    })
                })
        }
    }

    pub fn each<F>(&self, fun: F)
    where
        F: Fn(&mut Owned<T>),
    {
        self.kernel
            .process_each_enumerate(|app_id, process| unsafe {
                let root_ptr = *(process.grant_ptr(self.grant_num) as *mut *mut T);
                if !root_ptr.is_null() {
                    let mut root = Owned::new(self.kernel, root_ptr, app_id);
                    fun(&mut root);
                }
            });
    }

    pub fn iter(&self) -> Iter<T> {
        Iter {
            kernel: self.kernel,
            grant: self,
            index: 0,
            len: self.kernel.number_of_process_slots(),
        }
    }
}

pub struct Iter<'a, T: 'a + Default> {
    kernel: &'static Kernel,
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
            let res = self.grant.grant(AppId::new(self.kernel, idx));
            if res.is_some() {
                return res;
            }
        }
        None
    }
}
