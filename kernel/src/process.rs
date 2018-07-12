//! Support for creating and running userspace applications.

use callback::AppId;
use common::{Queue, RingBuffer};

use core::cell::Cell;
use core::fmt::Write;
use core::ptr::{read_volatile, write, write_volatile};
use core::{mem, ptr, slice, str};

use common::cells::MapCell;
use common::math;
use grant;
use platform::mpu;
use returncode::ReturnCode;
use sched::Kernel;
use syscall::{self, Syscall, SyscallInterface};
use tbfheader;

/// This is used in the hardfault handler.
#[no_mangle]
#[used]
static mut SCB_REGISTERS: [u32; 5] = [0; 5];

#[allow(improper_ctypes)]
extern "C" {
    crate fn switch_to_user(user_stack: *const u8, process_regs: &[usize; 8]) -> *mut u8;
}

/// Helper function to load processes from flash into an array of active
/// processes. This is the default template for loading processes, but a board
/// is able to create its own `load_processes()` function and use that instead.
///
/// Processes are found in flash starting from the given address and iterating
/// through Tock Binary Format headers. Processes are given memory out of the
/// `app_memory` buffer until either the memory is exhausted or the allocated
/// number of processes are created, with process structures placed in the
/// provided array. How process faults are handled by the kernel is also
/// selected.
pub unsafe fn load_processes<S: SyscallInterface>(
    kernel: &'static Kernel,
    syscall: &'static S,
    start_of_flash: *const u8,
    app_memory: &mut [u8],
    // procs: &mut [Option<&Process<'static, S>>],
    procs: &'static mut [Option<&'static ProcessType>],
    fault_response: FaultResponse,
) {
    let mut apps_in_flash_ptr = start_of_flash;
    let mut app_memory_ptr = app_memory.as_mut_ptr();
    let mut app_memory_size = app_memory.len();
    for i in 0..procs.len() {
        let (process, flash_offset, memory_offset) = Process::create(
            kernel,
            syscall,
            apps_in_flash_ptr,
            app_memory_ptr,
            app_memory_size,
            fault_response,
        );

        if process.is_none() {
            // We did not get a valid process, but we may have gotten a disabled
            // process or padding. Therefore we want to skip this chunk of flash
            // and see if there is a valid app there. However, if we cannot
            // advance the flash pointer, then we are done.
            if flash_offset == 0 && memory_offset == 0 {
                break;
            }
        } else {
            procs[i] = process;
        }

        apps_in_flash_ptr = apps_in_flash_ptr.offset(flash_offset as isize);
        app_memory_ptr = app_memory_ptr.offset(memory_offset as isize);
        app_memory_size -= memory_offset;
    }
}

pub trait ProcessType {
    fn schedule(&self, callback: FunctionCall) -> bool;
    fn schedule_ipc(&self, from: AppId, cb_type: IPCType);

    fn current_state(&self) -> State;

    /// Move this process from the running state to the yield state.
    fn yield_state(&self);

    unsafe fn fault_state(&self);

    fn dequeue_task(&self) -> Option<Task>;


    //memop

    fn sbrk(&self, increment: isize) -> Result<*const u8, Error>;

    fn brk(&self, new_break: *const u8) -> Result<*const u8, Error>;

    fn mem_start(&self) -> *const u8;

    fn mem_end(&self) -> *const u8;

    fn flash_start(&self) -> *const u8;

    fn flash_end(&self) -> *const u8;

    fn kernel_memory_break(&self) -> *const u8;

    fn number_writeable_flash_regions(&self) -> usize;

    fn get_writeable_flash_region(&self, region_index: usize) -> (u32, u32);

    fn update_stack_start_pointer(&self, stack_pointer: *const u8);

    fn update_heap_start_pointer(&self, heap_pointer: *const u8);



    fn setup_mpu(&self, mpu: &mpu::MPU);

    fn add_mpu_region(&self, base: *const u8, size: u32) -> bool;


    fn flash_non_protected_start(&self) -> *const u8;

    fn in_exposed_bounds(&self, buf_start_addr: *const u8, size: usize) -> bool;

    unsafe fn alloc(&self, size: usize) -> Option<&mut [u8]>;

    // unsafe fn free<T>(&self, _: *mut T);
    unsafe fn free(&self, _grant_num: usize);

    unsafe fn grant_ptr(&self, grant_num: usize) -> *mut *mut u8;

    unsafe fn grant_for(&self, grant_num: usize) -> *mut u8;

    // unsafe fn grant_for_or_alloc<T: Default>(&self, grant_num: usize, grant_size: usize) -> Option<*mut u8>;
    // unsafe fn grant_for_or_alloc(&self, grant_num: usize, grant_size: usize) -> Option<*mut u8>;

    fn pop_syscall_stack(&self);

    /// Context switch to the process.
    unsafe fn push_function_call(&self, callback: FunctionCall);

    fn incr_syscall_count(&self, last_syscall: Option<Syscall>);






    /// Return the per-process state that the kernel must store while the
    /// process is not running. This state is passed back to the process when it
    /// starts running.
    // fn stored_state<S: SyscallInterface>(&self) -> &<S as SyscallInterface>::StoredState  where Self: Sized;
    // fn stored_state(&self) -> &SyscallInterface::StoredState  where Self: Sized;
    // fn stored_state(&self) -> usize;

    fn get_package_name(&self) -> &[u8];




    // functions for processes that are architecture specific


    fn get_context_switch_reason(&self) -> syscall::ContextSwitchReason;

    /// Get the syscall that the process called.
    fn get_syscall_number(&self) -> Option<Syscall>;

    /// Get the four u32 values that the process can pass with the syscall.
    fn get_syscall_data(&self) -> (usize, usize, usize, usize);

    /// Set the return value the process should see when it begins executing
    /// again after the syscall.
    fn set_syscall_return_value(&self, return_value: isize);

    /// Replace the last stack frame with the new function call. This function
    /// is what should be executed when the process is resumed.
    fn replace_function_call(&self, callback: FunctionCall);

    /// Context switch to a specific process.
    fn switch_to_process(&self) -> *mut u8;

}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    NoSuchApp,
    OutOfMemory,
    AddressOutOfBounds,
}

impl From<Error> for ReturnCode {
    fn from(err: Error) -> ReturnCode {
        match err {
            Error::OutOfMemory => ReturnCode::ENOMEM,
            Error::AddressOutOfBounds => ReturnCode::EINVAL,
            Error::NoSuchApp => ReturnCode::EINVAL,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum State {
    Running,
    Yielded,
    Fault,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FaultResponse {
    Panic,
    Restart,
}

#[derive(Copy, Clone, Debug)]
pub enum IPCType {
    Service,
    Client,
}

#[derive(Copy, Clone)]
pub enum Task {
    FunctionCall(FunctionCall),
    IPC((AppId, IPCType)),
}

#[derive(Copy, Clone, Debug)]
pub struct FunctionCall {
    crate r0: usize,
    crate r1: usize,
    crate r2: usize,
    crate r3: usize,
    crate pc: usize,
}

// #[derive(Default)]
// struct StoredRegs {
//     r4: usize,
//     r5: usize,
//     r6: usize,
//     r7: usize,
//     r8: usize,
//     r9: usize,
//     r10: usize,
//     r11: usize,
// }

/// State for helping with debugging apps.
///
/// These pointers and counters are not strictly required for kernel operation,
/// but provide helpful information when an app crashes.
struct ProcessDebug {
    /// Where the process has started its heap in RAM.
    app_heap_start_pointer: Option<*const u8>,

    /// Where the start of the stack is for the process. If the kernel does the
    /// PIC setup for this app then we know this, otherwise we need the app to
    /// tell us where it put its stack.
    app_stack_start_pointer: Option<*const u8>,

    /// How low have we ever seen the stack pointer.
    min_stack_pointer: *const u8,

    /// How many syscalls have occurred since the process started.
    syscall_count: usize,

    /// What was the most recent syscall.
    last_syscall: Option<Syscall>,

    /// How many callbacks were dropped because the queue was insufficiently
    /// long.
    dropped_callback_count: usize,

    /// How many times this process has entered into a fault condition and the
    /// kernel has restarted it.
    restart_count: usize,
}

pub struct Process<'a, S: 'static + SyscallInterface> {
    /// Pointer to the main Kernel struct.
    kernel: &'static Kernel,

    syscall: &'static S,

    /// Application memory layout:
    ///
    /// ```text
    ///     ╒════════ ← memory[memory.len()]
    ///  ╔═ │ Grant
    ///     │   ↓
    ///  D  │ ──────  ← kernel_memory_break
    ///  Y  │
    ///  N  │ ──────  ← app_break
    ///  A  │
    ///  M  │   ↑
    ///     │  Heap
    ///  ╠═ │ ──────  ← app_heap_start
    ///     │  Data
    ///  F  │ ──────  ← data_start_pointer
    ///  I  │ Stack
    ///  X  │   ↓
    ///  E  │
    ///  D  │ ──────  ← current_stack_pointer
    ///     │
    ///  ╚═ ╘════════ ← memory[0]
    /// ```
    ///
    /// The process's memory.
    memory: &'static mut [u8],

    /// Pointer to the end of the allocated (and MPU protected) grant region.
    kernel_memory_break: Cell<*const u8>,

    /// Copy of where the kernel memory break is when the app is first started.
    /// This is handy if the app is restarted so we know where to reset
    /// the kernel_memory break to without having to recalculate it.
    original_kernel_memory_break: *const u8,

    /// Pointer to the end of process RAM that has been sbrk'd to the process.
    app_break: Cell<*const u8>,
    original_app_break: *const u8,

    /// Saved when the app switches to the kernel.
    current_stack_pointer: Cell<*const u8>,
    original_stack_pointer: *const u8,

    /// Process flash segment. This is the region of nonvolatile flash that
    /// the process occupies.
    flash: &'static [u8],

    /// Collection of pointers to the TBF header in flash.
    header: tbfheader::TbfHeader,

    /// Saved each time the app switches to the kernel.
    stored_regs: S::StoredState,

    /// The PC to jump to when switching back to the app.
    yield_pc: Cell<usize>,

    /// Process State Register.
    psr: Cell<usize>,

    /// Whether the scheduler can schedule this app.
    state: Cell<State>,

    /// How to deal with Faults occurring in the process
    fault_response: FaultResponse,

    /// MPU regions are saved as a pointer-size pair.
    ///
    /// size is encoded as X where
    /// SIZE = 2<sup>(X + 1)</sup> and X >= 4.
    ///
    /// A null pointer represents an empty region.
    ///
    /// #### Invariants
    ///
    /// The pointer must be aligned to the size. E.g. if the size is 32 bytes, the pointer must be
    /// 32-byte aligned.
    mpu_regions: [Cell<(*const u8, math::PowerOfTwo)>; 5],

    /// Essentially a list of callbacks that want to call functions in the
    /// process.
    tasks: MapCell<RingBuffer<'a, Task>>,

    /// Name of the app. Public so that IPC can use it.
    pub package_name: &'static str,

    /// Values kept so that we can print useful debug messages when apps fault.
    debug: MapCell<ProcessDebug>,
}

impl<S:SyscallInterface> ProcessType for Process<'a, S> {

    fn schedule(&self, callback: FunctionCall) -> bool {
        // If this app is in the `Fault` state then we shouldn't schedule
        // any work for it.
        if self.current_state() == State::Fault {
            return false;
        }

        self.kernel.increment_work();

        let ret = self
            .tasks
            .map_or(false, |tasks| tasks.enqueue(Task::FunctionCall(callback)));

        // Make a note that we lost this callback if the enqueue function
        // fails.
        if ret == false {
            self.debug.map(|debug| {
                debug.dropped_callback_count += 1;
            });
        }

        ret
    }

    fn schedule_ipc(&self, from: AppId, cb_type: IPCType) {
        self.kernel.increment_work();
        let ret = self
            .tasks
            .map_or(false, |tasks| tasks.enqueue(Task::IPC((from, cb_type))));

        // Make a note that we lost this callback if the enqueue function
        // fails.
        if ret == false {
            self.debug.map(|debug| {
                debug.dropped_callback_count += 1;
            });
        }
    }

    /// Retrieve the current state of this process (i.e. is it running,
    /// yielded, or in a fault state).
    fn current_state(&self) -> State {
        self.state.get()
    }

    /// Move this process from the running state to the yield state.
    fn yield_state(&self) {
        let current_state = self.state.get();
        if current_state == State::Running {
            self.state.set(State::Yielded);
            self.kernel.decrement_work();
        }
    }

    unsafe fn fault_state(&self) {
        self.state.set(State::Fault);

        match self.fault_response {
            FaultResponse::Panic => {
                // process faulted. Panic and print status
                panic!("Process {} had a fault", self.package_name);
            }
            FaultResponse::Restart => {
                // Remove the tasks that were scheduled for the app from the
                // amount of work queue.
                let tasks_len = self.tasks.map_or(0, |tasks| tasks.len());
                for _ in 0..tasks_len {
                    self.kernel.decrement_work();
                }

                // And remove those tasks
                self.tasks.map(|tasks| {
                    tasks.empty();
                });

                // Update debug information
                self.debug.map(|debug| {
                    // Mark that we restarted this process.
                    debug.restart_count += 1;

                    // Reset some state for the process.
                    debug.syscall_count = 0;
                    debug.last_syscall = None;
                    debug.dropped_callback_count = 0;
                });

                // We are going to start this process over again, so need
                // the init_fn location.
                let app_flash_address = self.flash_start();
                let init_fn = app_flash_address
                    .offset(self.header.get_init_function_offset() as isize)
                    as usize;
                self.yield_pc.set(init_fn);
                self.psr.set(0x01000000);
                self.state.set(State::Yielded);

                // Need to reset the grant region.
                self.grant_ptrs_reset();
                self.kernel_memory_break
                    .set(self.original_kernel_memory_break);

                // Reset other memory pointers.
                self.app_break.set(self.original_app_break);
                self.current_stack_pointer.set(self.original_stack_pointer);

                // And queue up this app to be restarted.
                let flash_protected_size = self.header.get_protected_size() as usize;
                let flash_app_start = app_flash_address as usize + flash_protected_size;

                self.tasks.map(|tasks| {
                    tasks.enqueue(Task::FunctionCall(FunctionCall {
                        pc: init_fn,
                        r0: flash_app_start,
                        r1: self.memory.as_ptr() as usize,
                        r2: self.memory.len() as usize,
                        r3: self.app_break.get() as usize,
                    }));
                });

                self.kernel.increment_work();
            }
        }
    }

    fn dequeue_task(&self) -> Option<Task> {
        self.tasks.map_or(None, |tasks| {
            tasks.dequeue().map(|cb| {
                self.kernel.decrement_work();
                cb
            })
        })
    }

    fn mem_start(&self) -> *const u8 {
        self.memory.as_ptr()
    }

    fn mem_end(&self) -> *const u8 {
        unsafe { self.memory.as_ptr().offset(self.memory.len() as isize) }
    }

    fn flash_start(&self) -> *const u8 {
        self.flash.as_ptr()
    }

    fn flash_non_protected_start(&self) -> *const u8 {
        ((self.flash.as_ptr() as usize) + self.header.get_protected_size() as usize) as *const u8
    }

    fn flash_end(&self) -> *const u8 {
        unsafe { self.flash.as_ptr().offset(self.flash.len() as isize) }
    }

    fn kernel_memory_break(&self) -> *const u8 {
        self.kernel_memory_break.get()
    }

    fn number_writeable_flash_regions(&self) -> usize {
        self.header.number_writeable_flash_regions()
    }

    fn get_writeable_flash_region(&self, region_index: usize) -> (u32, u32) {
        self.header.get_writeable_flash_region(region_index)
    }

    fn update_stack_start_pointer(&self, stack_pointer: *const u8) {
        if stack_pointer >= self.mem_start() && stack_pointer < self.mem_end() {
            self.debug.map(|debug| {
                debug.app_stack_start_pointer = Some(stack_pointer);

                // We also reset the minimum stack pointer because whatever value
                // we had could be entirely wrong by now.
                debug.min_stack_pointer = stack_pointer;
            });
        }
    }

    fn update_heap_start_pointer(&self, heap_pointer: *const u8) {
        if heap_pointer >= self.mem_start() && heap_pointer < self.mem_end() {
            self.debug.map(|debug| {
                debug.app_heap_start_pointer = Some(heap_pointer);
            });
        }
    }

    fn setup_mpu(&self, mpu: &mpu::MPU) {
        // Flash segment read/execute (no write)
        let flash_start = self.flash.as_ptr() as usize;
        let flash_len = self.flash.len();

        match mpu.create_region(
            0,
            flash_start,
            flash_len,
            mpu::ExecutePermission::ExecutionPermitted,
            mpu::AccessPermission::ReadOnly,
        ) {
            None => panic!(
                "Infeasible MPU allocation. Base {:#x}, Length: {:#x}",
                flash_start, flash_len
            ),
            Some(region) => mpu.set_mpu(region),
        }

        let data_start = self.memory.as_ptr() as usize;
        let data_len = self.memory.len();

        match mpu.create_region(
            1,
            data_start,
            data_len,
            mpu::ExecutePermission::ExecutionPermitted,
            mpu::AccessPermission::ReadWrite,
        ) {
            None => panic!(
                "Infeasible MPU allocation. Base {:#x}, Length: {:#x}",
                data_start, data_len
            ),
            Some(region) => mpu.set_mpu(region),
        }

        // Disallow access to grant region
        let grant_len = unsafe {
            math::PowerOfTwo::ceiling(
                self.memory.as_ptr().offset(self.memory.len() as isize) as u32
                    - (self.kernel_memory_break.get() as u32),
            ).as_num::<u32>()
        };
        let grant_base = unsafe {
            self.memory
                .as_ptr()
                .offset(self.memory.len() as isize)
                .offset(-(grant_len as isize))
        };

        match mpu.create_region(
            2,
            grant_base as usize,
            grant_len as usize,
            mpu::ExecutePermission::ExecutionNotPermitted,
            mpu::AccessPermission::PrivilegedOnly,
        ) {
            None => panic!(
                "Infeasible MPU allocation. Base {:#x}, Length: {:#x}",
                grant_base as usize, grant_len
            ),
            Some(region) => mpu.set_mpu(region),
        }

        // Setup IPC MPU regions
        for (i, region) in self.mpu_regions.iter().enumerate() {
            if region.get().0.is_null() {
                mpu.set_mpu(mpu::Region::empty(i + 3));
                continue;
            }
            match mpu.create_region(
                i + 3,
                region.get().0 as usize,
                region.get().1.as_num::<u32>() as usize,
                mpu::ExecutePermission::ExecutionPermitted,
                mpu::AccessPermission::ReadWrite,
            ) {
                None => panic!(
                    "Unexpected: Infeasible MPU allocation: Num: {}, \
                     Base: {:#x}, Length: {:#x}",
                    i + 3,
                    region.get().0 as usize,
                    region.get().1.as_num::<u32>()
                ),
                Some(region) => mpu.set_mpu(region),
            }
        }
    }

    fn add_mpu_region(&self, base: *const u8, size: u32) -> bool {
        if size >= 16 && size.count_ones() == 1 && (base as u32) % size == 0 {
            let mpu_size = math::PowerOfTwo::floor(size);
            for region in self.mpu_regions.iter() {
                if region.get().0 == ptr::null() {
                    region.set((base, mpu_size));
                    return true;
                } else if region.get().0 == base {
                    if region.get().1 < mpu_size {
                        region.set((base, mpu_size));
                    }
                    return true;
                }
            }
        }
        return false;
    }

    fn sbrk(&self, increment: isize) -> Result<*const u8, Error> {
        let new_break = unsafe { self.app_break.get().offset(increment) };
        self.brk(new_break)
    }

    fn brk(&self, new_break: *const u8) -> Result<*const u8, Error> {
        if new_break < self.mem_start() || new_break >= self.mem_end() {
            Err(Error::AddressOutOfBounds)
        } else if new_break > self.kernel_memory_break.get() {
            Err(Error::OutOfMemory)
        } else {
            let old_break = self.app_break.get();
            self.app_break.set(new_break);
            Ok(old_break)
        }
    }

    fn in_exposed_bounds(&self, buf_start_addr: *const u8, size: usize) -> bool {
        let buf_end_addr = unsafe { buf_start_addr.offset(size as isize) };

        buf_start_addr >= self.mem_start() && buf_end_addr <= self.mem_end()
    }

    unsafe fn alloc(&self, size: usize) -> Option<&mut [u8]> {
        let new_break = self.kernel_memory_break.get().offset(-(size as isize));
        if new_break < self.app_break.get() {
            None
        } else {
            self.kernel_memory_break.set(new_break);
            Some(slice::from_raw_parts_mut(new_break as *mut u8, size))
        }
    }

    unsafe fn free(&self, _grant_num: usize) {}

    unsafe fn grant_ptr(&self, grant_num: usize) -> *mut *mut u8 {
        let grant_num = grant_num as isize;
        (self.mem_end() as *mut *mut u8).offset(-(grant_num + 1))
    }

    unsafe fn grant_for(&self, grant_num: usize) -> *mut u8 {
        *self.grant_ptr(grant_num)
    }

    // unsafe fn grant_for_or_alloc(&self, grant_num: usize, grant_size: usize) -> Option<*mut u8> {
    //     let ctr_ptr = self.grant_ptr(grant_num);
    //     if (*ctr_ptr).is_null() {
    //         self.alloc(grant_size).map(|root_arr| {
    //             let root_ptr = root_arr.as_mut_ptr() as *mut u8;
    //             // Initialize the grant contents using ptr::write, to
    //             // ensure that we don't try to drop the contents of
    //             // uninitialized memory when T implements Drop.
    //     //write(root_ptr, Default::default());
    //             // Record the location in the grant pointer.
    //             write_volatile(ctr_ptr, root_ptr);
    //             root_ptr
    //         })
    //     } else {
    //         Some(*ctr_ptr)
    //     }
    // }

    fn pop_syscall_stack(&self) {
        let pspr = self.current_stack_pointer.get() as *const usize;
        unsafe {
            self.yield_pc.set(read_volatile(pspr.offset(6)));
            self.psr.set(read_volatile(pspr.offset(7)));
            self.current_stack_pointer
                .set((self.current_stack_pointer.get() as *mut usize).offset(8) as *mut u8);
            self.debug.map(|debug| {
                if self.current_stack_pointer.get() < debug.min_stack_pointer {
                    debug.min_stack_pointer = self.current_stack_pointer.get();
                }
            });
        }
    }

    /// Context switch to the process.
    unsafe fn push_function_call(&self, callback: FunctionCall) {
        self.kernel.increment_work();

        self.state.set(State::Running);
        // Fill in initial stack expected by SVC handler
        // Top minus 8 u32s for r0-r3, r12, lr, pc and xPSR
        let stack_bottom = (self.current_stack_pointer.get() as *mut usize).offset(-8);
        write_volatile(stack_bottom.offset(7), self.psr.get());
        write_volatile(stack_bottom.offset(6), callback.pc | 1);

        // Set the LR register to the saved PC so the callback returns to
        // wherever wait was called. Set lowest bit to one because of THUMB
        // instruction requirements.
        write_volatile(stack_bottom.offset(5), self.yield_pc.get() | 0x1);
        write_volatile(stack_bottom, callback.r0);
        write_volatile(stack_bottom.offset(1), callback.r1);
        write_volatile(stack_bottom.offset(2), callback.r2);
        write_volatile(stack_bottom.offset(3), callback.r3);

        self.current_stack_pointer.set(stack_bottom as *mut u8);
        self.debug.map(|debug| {
            if self.current_stack_pointer.get() < debug.min_stack_pointer {
                debug.min_stack_pointer = self.current_stack_pointer.get();
            }
        });
    }

    // /// Context switch to the process.
    // crate unsafe fn switch_to(&self) {
    //     let psp = switch_to_user(
    //         self.current_stack_pointer.get(),
    //         &*(&self.stored_regs as *const StoredRegs as *const [usize; 8]),
    //     );
    //     self.current_stack_pointer.set(psp);
    //     self.debug.map(|debug| {
    //         if self.current_stack_pointer.get() < debug.min_stack_pointer {
    //             debug.min_stack_pointer = self.current_stack_pointer.get();
    //         }
    //     });
    // }

    fn incr_syscall_count(&self, last_syscall: Option<Syscall>) {
        self.debug.map(|debug| {
            debug.syscall_count += 1;
            debug.last_syscall = last_syscall;
        });
    }




    /// Return the per-process state that the kernel must store while the
    /// process is not running. This state is passed back to the process when it
    /// starts running.
    // fn stored_state(&self) -> &S::StoredState {
    //     &self.stored_regs
    // }
    // fn stored_state(&self) -> usize {
    //     0
    // }

    fn get_package_name(&self) -> &[u8] {
        self.package_name.as_bytes()
    }

    fn get_context_switch_reason(&self) -> syscall::ContextSwitchReason {
        self.syscall.get_context_switch_reason()
    }

    fn get_syscall_number(&self) -> Option<Syscall> {
        self.syscall.get_syscall_number(self.sp())
    }

    fn get_syscall_data(&self) -> (usize, usize, usize, usize) {
        self.syscall.get_syscall_data(self.sp())
    }

    fn set_syscall_return_value(&self, return_value: isize) {
        self.syscall.set_syscall_return_value(self.sp(), return_value);
    }

    fn replace_function_call(&self, callback: FunctionCall) {
        self.syscall.replace_function_call(self.sp(), callback);
    }

    fn switch_to_process(&self) -> *mut u8 {
        self.syscall.switch_to_process(self.sp(), &self.stored_regs)
    }
}

impl<S: 'static + SyscallInterface> Process<'a, S> {
    crate unsafe fn create(
        kernel: &'static Kernel,
        syscall: &'static S,
        app_flash_address: *const u8,
        remaining_app_memory: *mut u8,
        remaining_app_memory_size: usize,
        fault_response: FaultResponse,
    ) -> (Option<&'static ProcessType>, usize, usize) {
        if let Some(tbf_header) = tbfheader::parse_and_validate_tbf_header(app_flash_address) {
            let app_flash_size = tbf_header.get_total_size() as usize;

            // If this isn't an app (i.e. it is padding) or it is an app but it
            // isn't enabled, then we can skip it but increment past its flash.
            if !tbf_header.is_app() || !tbf_header.enabled() {
                return (None, app_flash_size, 0);
            }

            // Otherwise, actually load the app.
            let mut min_app_ram_size = tbf_header.get_minimum_app_ram_size();
            let package_name = tbf_header.get_package_name(app_flash_address);
            let init_fn =
                app_flash_address.offset(tbf_header.get_init_function_offset() as isize) as usize;

            // Set the initial process stack and memory to 128 bytes.
            let initial_stack_pointer = remaining_app_memory.offset(128);
            let initial_sbrk_pointer = remaining_app_memory.offset(128);

            // First determine how much space we need in the application's
            // memory space just for kernel and grant state. We need to make
            // sure we allocate enough memory just for that.

            // Make room for grant pointers.
            let grant_ptr_size = mem::size_of::<*const usize>();
            let grant_ptrs_num = read_volatile(&grant::CONTAINER_COUNTER);
            let grant_ptrs_offset = grant_ptrs_num * grant_ptr_size;

            // Allocate memory for callback ring buffer.
            let callback_size = mem::size_of::<Task>();
            let callback_len = 10;
            let callbacks_offset = callback_len * callback_size;

            // Make room to store this process's metadata.
            let process_struct_offset = mem::size_of::<Process<S>>();

            // Need to make sure that the amount of memory we allocate for
            // this process at least covers this state.
            if min_app_ram_size
                < (grant_ptrs_offset + callbacks_offset + process_struct_offset) as u32
            {
                min_app_ram_size =
                    (grant_ptrs_offset + callbacks_offset + process_struct_offset) as u32;
            }

            // TODO round app_ram_size up to a closer MPU unit.
            // This is a very conservative approach that rounds up to power of
            // two. We should be able to make this closer to what we actually need.
            let app_ram_size = math::closest_power_of_two(min_app_ram_size) as usize;

            // Check that we can actually give this app this much memory.
            if app_ram_size > remaining_app_memory_size {
                panic!(
                    "{:?} failed to load. Insufficient memory. Requested {} have {}",
                    package_name, app_ram_size, remaining_app_memory_size
                );
            }

            let app_memory = slice::from_raw_parts_mut(remaining_app_memory, app_ram_size);

            // Set up initial grant region.
            let mut kernel_memory_break = app_memory.as_mut_ptr().offset(app_memory.len() as isize);

            // Now that we know we have the space we can setup the grant
            // pointers.
            kernel_memory_break = kernel_memory_break.offset(-(grant_ptrs_offset as isize));

            // Set all pointers to null.
            let opts =
                slice::from_raw_parts_mut(kernel_memory_break as *mut *const usize, grant_ptrs_num);
            for opt in opts.iter_mut() {
                *opt = ptr::null()
            }

            // Now that we know we have the space we can setup the memory
            // for the callbacks.
            kernel_memory_break = kernel_memory_break.offset(-(callbacks_offset as isize));

            // Set up ring buffer.
            let callback_buf =
                slice::from_raw_parts_mut(kernel_memory_break as *mut Task, callback_len);
            let tasks = RingBuffer::new(callback_buf);

            // Last thing is the process struct.
            kernel_memory_break = kernel_memory_break.offset(-(process_struct_offset as isize));
            let process_struct_memory_location = kernel_memory_break;

            // Determine the debug information to the best of our
            // understanding. If the app is doing all of the PIC fixup and
            // memory management we don't know much.
            let mut app_heap_start_pointer = None;
            let mut app_stack_start_pointer = None;

            // Create the Process struct in the app grant region.
            let mut process: &mut Process<S> =
                &mut *(process_struct_memory_location as *mut Process<'static, S>);

            process.kernel = kernel;
            process.syscall = syscall;
            process.memory = app_memory;
            process.header = tbf_header;
            process.kernel_memory_break = Cell::new(kernel_memory_break);
            process.original_kernel_memory_break = kernel_memory_break;
            process.app_break = Cell::new(initial_sbrk_pointer);
            process.original_app_break = initial_sbrk_pointer;
            process.current_stack_pointer = Cell::new(initial_stack_pointer);
            process.original_stack_pointer = initial_stack_pointer;

            process.flash = slice::from_raw_parts(app_flash_address, app_flash_size);

            process.stored_regs = Default::default();
            process.yield_pc = Cell::new(init_fn);
            // Set the Thumb bit and clear everything else
            process.psr = Cell::new(0x01000000);

            process.state = Cell::new(State::Yielded);
            process.fault_response = fault_response;

            process.mpu_regions = [
                Cell::new((ptr::null(), math::PowerOfTwo::zero())),
                Cell::new((ptr::null(), math::PowerOfTwo::zero())),
                Cell::new((ptr::null(), math::PowerOfTwo::zero())),
                Cell::new((ptr::null(), math::PowerOfTwo::zero())),
                Cell::new((ptr::null(), math::PowerOfTwo::zero())),
            ];
            process.tasks = MapCell::new(tasks);
            process.package_name = package_name;

            process.debug = MapCell::new(ProcessDebug {
                app_heap_start_pointer: app_heap_start_pointer,
                app_stack_start_pointer: app_stack_start_pointer,
                min_stack_pointer: initial_stack_pointer,
                syscall_count: 0,
                last_syscall: None,
                dropped_callback_count: 0,
                restart_count: 0,
            });

            if (init_fn & 0x1) != 1 {
                panic!(
                    "{:?} process image invalid. \
                     init_fn address must end in 1 to be Thumb, got {:#X}",
                    package_name, init_fn
                );
            }

            let flash_protected_size = process.header.get_protected_size() as usize;
            let flash_app_start = app_flash_address as usize + flash_protected_size;

            process.tasks.map(|tasks| {
                tasks.enqueue(Task::FunctionCall(FunctionCall {
                    pc: init_fn,
                    r0: flash_app_start,
                    r1: process.memory.as_ptr() as usize,
                    r2: process.memory.len() as usize,
                    r3: process.app_break.get() as usize,
                }));
            });

            kernel.increment_work();

            return (Some(process), app_flash_size, app_ram_size);
        }
        (None, 0, 0)
    }


    fn sp(&self) -> *const usize {
        self.current_stack_pointer.get() as *const usize
    }

    fn lr(&self) -> usize {
        let pspr = self.current_stack_pointer.get() as *const usize;
        unsafe { read_volatile(pspr.offset(5)) }
    }

    fn pc(&self) -> usize {
        let pspr = self.current_stack_pointer.get() as *const usize;
        unsafe { read_volatile(pspr.offset(6)) }
    }

    fn r12(&self) -> usize {
        let pspr = self.current_stack_pointer.get() as *const usize;
        unsafe { read_volatile(pspr.offset(4)) }
    }

    fn xpsr(&self) -> usize {
        let pspr = self.current_stack_pointer.get() as *const usize;
        unsafe { read_volatile(pspr.offset(7)) }
    }


    /// Reset all `grant_ptr`s to NULL.
    unsafe fn grant_ptrs_reset(&self) {
        let grant_ptrs_num = read_volatile(&grant::CONTAINER_COUNTER);
        for grant_num in 0..grant_ptrs_num {
            let grant_num = grant_num as isize;
            let ctr_ptr = (self.mem_end() as *mut *mut usize).offset(-(grant_num + 1));
            write_volatile(ctr_ptr, ptr::null_mut());
        }
    }



    crate unsafe fn fault_str<W: Write>(&self, writer: &mut W) {
        let _ccr = SCB_REGISTERS[0];
        let cfsr = SCB_REGISTERS[1];
        let hfsr = SCB_REGISTERS[2];
        let mmfar = SCB_REGISTERS[3];
        let bfar = SCB_REGISTERS[4];

        let iaccviol = (cfsr & 0x01) == 0x01;
        let daccviol = (cfsr & 0x02) == 0x02;
        let munstkerr = (cfsr & 0x08) == 0x08;
        let mstkerr = (cfsr & 0x10) == 0x10;
        let mlsperr = (cfsr & 0x20) == 0x20;
        let mmfarvalid = (cfsr & 0x80) == 0x80;

        let ibuserr = ((cfsr >> 8) & 0x01) == 0x01;
        let preciserr = ((cfsr >> 8) & 0x02) == 0x02;
        let impreciserr = ((cfsr >> 8) & 0x04) == 0x04;
        let unstkerr = ((cfsr >> 8) & 0x08) == 0x08;
        let stkerr = ((cfsr >> 8) & 0x10) == 0x10;
        let lsperr = ((cfsr >> 8) & 0x20) == 0x20;
        let bfarvalid = ((cfsr >> 8) & 0x80) == 0x80;

        let undefinstr = ((cfsr >> 16) & 0x01) == 0x01;
        let invstate = ((cfsr >> 16) & 0x02) == 0x02;
        let invpc = ((cfsr >> 16) & 0x04) == 0x04;
        let nocp = ((cfsr >> 16) & 0x08) == 0x08;
        let unaligned = ((cfsr >> 16) & 0x100) == 0x100;
        let divbysero = ((cfsr >> 16) & 0x200) == 0x200;

        let vecttbl = (hfsr & 0x02) == 0x02;
        let forced = (hfsr & 0x40000000) == 0x40000000;

        let _ = writer.write_fmt(format_args!("\r\n---| Fault Status |---\r\n"));

        if iaccviol {
            let _ = writer.write_fmt(format_args!(
                "Instruction Access Violation:       {}\r\n",
                iaccviol
            ));
        }
        if daccviol {
            let _ = writer.write_fmt(format_args!(
                "Data Access Violation:              {}\r\n",
                daccviol
            ));
        }
        if munstkerr {
            let _ = writer.write_fmt(format_args!(
                "Memory Management Unstacking Fault: {}\r\n",
                munstkerr
            ));
        }
        if mstkerr {
            let _ = writer.write_fmt(format_args!(
                "Memory Management Stacking Fault:   {}\r\n",
                mstkerr
            ));
        }
        if mlsperr {
            let _ = writer.write_fmt(format_args!(
                "Memory Management Lazy FP Fault:    {}\r\n",
                mlsperr
            ));
        }

        if ibuserr {
            let _ = writer.write_fmt(format_args!(
                "Instruction Bus Error:              {}\r\n",
                ibuserr
            ));
        }
        if preciserr {
            let _ = writer.write_fmt(format_args!(
                "Precise Data Bus Error:             {}\r\n",
                preciserr
            ));
        }
        if impreciserr {
            let _ = writer.write_fmt(format_args!(
                "Imprecise Data Bus Error:           {}\r\n",
                impreciserr
            ));
        }
        if unstkerr {
            let _ = writer.write_fmt(format_args!(
                "Bus Unstacking Fault:               {}\r\n",
                unstkerr
            ));
        }
        if stkerr {
            let _ = writer.write_fmt(format_args!(
                "Bus Stacking Fault:                 {}\r\n",
                stkerr
            ));
        }
        if lsperr {
            let _ = writer.write_fmt(format_args!(
                "Bus Lazy FP Fault:                  {}\r\n",
                lsperr
            ));
        }

        if undefinstr {
            let _ = writer.write_fmt(format_args!(
                "Undefined Instruction Usage Fault:  {}\r\n",
                undefinstr
            ));
        }
        if invstate {
            let _ = writer.write_fmt(format_args!(
                "Invalid State Usage Fault:          {}\r\n",
                invstate
            ));
        }
        if invpc {
            let _ = writer.write_fmt(format_args!(
                "Invalid PC Load Usage Fault:        {}\r\n",
                invpc
            ));
        }
        if nocp {
            let _ = writer.write_fmt(format_args!(
                "No Coprocessor Usage Fault:         {}\r\n",
                nocp
            ));
        }
        if unaligned {
            let _ = writer.write_fmt(format_args!(
                "Unaligned Access Usage Fault:       {}\r\n",
                unaligned
            ));
        }
        if divbysero {
            let _ = writer.write_fmt(format_args!(
                "Divide By Zero:                     {}\r\n",
                divbysero
            ));
        }

        if vecttbl {
            let _ = writer.write_fmt(format_args!(
                "Bus Fault on Vector Table Read:     {}\r\n",
                vecttbl
            ));
        }
        if forced {
            let _ = writer.write_fmt(format_args!(
                "Forced Hard Fault:                  {}\r\n",
                forced
            ));
        }

        if mmfarvalid {
            let _ = writer.write_fmt(format_args!(
                "Faulting Memory Address:            {:#010X}\r\n",
                mmfar
            ));
        }
        if bfarvalid {
            let _ = writer.write_fmt(format_args!(
                "Bus Fault Address:                  {:#010X}\r\n",
                bfar
            ));
        }

        if cfsr == 0 && hfsr == 0 {
            let _ = writer.write_fmt(format_args!("No faults detected.\r\n"));
        } else {
            let _ = writer.write_fmt(format_args!(
                "Fault Status Register (CFSR):       {:#010X}\r\n",
                cfsr
            ));
            let _ = writer.write_fmt(format_args!(
                "Hard Fault Status Register (HFSR):  {:#010X}\r\n",
                hfsr
            ));
        }
    }

    crate unsafe fn statistics_str<W: Write>(&self, writer: &mut W) {
        // Flash
        let flash_end = self.flash.as_ptr().offset(self.flash.len() as isize) as usize;
        let flash_start = self.flash.as_ptr() as usize;
        let flash_protected_size = self.header.get_protected_size() as usize;
        let flash_app_start = flash_start + flash_protected_size;
        let flash_app_size = flash_end - flash_app_start;
        let flash_init_fn = flash_start + self.header.get_init_function_offset() as usize;

        // SRAM addresses
        let sram_end = self.memory.as_ptr().offset(self.memory.len() as isize) as usize;
        let sram_grant_start = self.kernel_memory_break.get() as usize;
        let sram_heap_end = self.app_break.get() as usize;
        let sram_heap_start = self.debug.map_or(ptr::null(), |debug| {
            debug.app_heap_start_pointer.unwrap_or(ptr::null())
        }) as usize;
        let sram_stack_start = self.debug.map_or(ptr::null(), |debug| {
            debug.app_stack_start_pointer.unwrap_or(ptr::null())
        }) as usize;
        let sram_stack_bottom =
            self.debug
                .map_or(ptr::null(), |debug| debug.min_stack_pointer) as usize;
        let sram_start = self.memory.as_ptr() as usize;

        // SRAM sizes
        let sram_grant_size = sram_end - sram_grant_start;
        let sram_heap_size = sram_heap_end - sram_heap_start;
        let sram_data_size = sram_heap_start - sram_stack_start;
        let sram_stack_size = sram_stack_start - sram_stack_bottom;
        let sram_grant_allocated = sram_end - sram_grant_start;
        let sram_heap_allocated = sram_grant_start - sram_heap_start;
        let sram_stack_allocated = sram_stack_start - sram_start;
        let sram_data_allocated = sram_data_size as usize;

        // checking on sram
        let mut sram_grant_error_str = "          ";
        if sram_grant_size > sram_grant_allocated {
            sram_grant_error_str = " EXCEEDED!"
        }
        let mut sram_heap_error_str = "          ";
        if sram_heap_size > sram_heap_allocated {
            sram_heap_error_str = " EXCEEDED!"
        }
        let mut sram_stack_error_str = "          ";
        if sram_stack_size > sram_stack_allocated {
            sram_stack_error_str = " EXCEEDED!"
        }

        // application statistics
        let events_queued = self.tasks.map_or(0, |tasks| tasks.len());
        let syscall_count = self.debug.map_or(0, |debug| debug.syscall_count);
        let last_syscall = self.debug.map(|debug| debug.last_syscall);
        let dropped_callback_count = self.debug.map_or(0, |debug| debug.dropped_callback_count);
        let restart_count = self.debug.map_or(0, |debug| debug.restart_count);

        // register values
        let (r0, r1, r2, r3, r12, sp, lr, pc, xpsr) = (
            // self.r0(),
            // self.r1(),
            // self.r2(),
            // self.r3(),
            5, 6, 7, 8,
            self.r12(),
            self.sp() as usize,
            self.lr(),
            self.pc(),
            self.xpsr(),
        );

        let _ = writer.write_fmt(format_args!(
            "\
             App: {}   -   [{:?}]\
             \r\n Events Queued: {}   Syscall Count: {}   Dropped Callback Count: {}\
             \n Restart Count: {}\n",
            self.package_name,
            self.state,
            events_queued,
            syscall_count,
            dropped_callback_count,
            restart_count,
        ));

        let _ = match last_syscall {
            Some(syscall) => writer.write_fmt(format_args!(" Last Syscall: {:?}", syscall)),
            None => writer.write_fmt(format_args!(" Last Syscall: None")),
        };

        let _ = writer.write_fmt(format_args!("\
\r\n\
\r\n ╔═══════════╤══════════════════════════════════════════╗\
\r\n ║  Address  │ Region Name    Used | Allocated (bytes)  ║\
\r\n ╚{:#010X}═╪══════════════════════════════════════════╝\
\r\n             │ ▼ Grant      {:6} | {:6}{}\
  \r\n  {:#010X} ┼───────────────────────────────────────────\
\r\n             │ Unused\
  \r\n  {:#010X} ┼───────────────────────────────────────────\
\r\n             │ ▲ Heap       {:6} | {:6}{}     S\
  \r\n  {:#010X} ┼─────────────────────────────────────────── R\
\r\n             │ Data         {:6} | {:6}               A\
  \r\n  {:#010X} ┼─────────────────────────────────────────── M\
\r\n             │ ▼ Stack      {:6} | {:6}{}\
  \r\n  {:#010X} ┼───────────────────────────────────────────\
\r\n             │ Unused\
  \r\n  {:#010X} ┴───────────────────────────────────────────\
\r\n             .....\
  \r\n  {:#010X} ┬─────────────────────────────────────────── F\
\r\n             │ App Flash    {:6}                        L\
  \r\n  {:#010X} ┼─────────────────────────────────────────── A\
\r\n             │ Protected    {:6}                        S\
  \r\n  {:#010X} ┴─────────────────────────────────────────── H\
\r\n\
  \r\n  R0 : {:#010X}    R6 : {:#010X}\
  \r\n  R1 : {:#010X}    R7 : {:#010X}\
  \r\n  R2 : {:#010X}    R8 : {:#010X}\
  \r\n  R3 : {:#010X}    R10: {:#010X}\
  \r\n  R4 : {:#010X}    R11: {:#010X}\
  \r\n  R5 : {:#010X}    R12: {:#010X}\
  \r\n  R9 : {:#010X} (Static Base Register)\
  \r\n  SP : {:#010X} (Process Stack Pointer)\
  \r\n  LR : {:#010X}\
  \r\n  PC : {:#010X}\
  \r\n YPC : {:#010X}\
\r\n",
  sram_end,
  sram_grant_size, sram_grant_allocated, sram_grant_error_str,
  sram_grant_start,
  sram_heap_end,
  sram_heap_size, sram_heap_allocated, sram_heap_error_str,
  sram_heap_start,
  sram_data_size, sram_data_allocated,
  sram_stack_start,
  sram_stack_size, sram_stack_allocated, sram_stack_error_str,
  sram_stack_bottom,
  sram_start,
  flash_end,
  flash_app_size,
  flash_app_start,
  flash_protected_size,
  flash_start,
  // r0, self.stored_regs.r6,
  // r1, self.stored_regs.r7,
  // r2, self.stored_regs.r8,
  // r3, self.stored_regs.r10,
  // self.stored_regs.r4, self.stored_regs.r11,
  // self.stored_regs.r5, r12,
  // self.stored_regs.r9,
  r0, 6,
  r1, 7,
  r2, 8,
  r3, 10,
  4, 11,
  5, r12,
  9,
  sp,
  lr,
  pc,
  self.yield_pc.get(),
  ));
        let _ = writer.write_fmt(format_args!(
            "\
             \r\n APSR: N {} Z {} C {} V {} Q {}\
             \r\n       GE {} {} {} {}",
            (xpsr >> 31) & 0x1,
            (xpsr >> 30) & 0x1,
            (xpsr >> 29) & 0x1,
            (xpsr >> 28) & 0x1,
            (xpsr >> 27) & 0x1,
            (xpsr >> 19) & 0x1,
            (xpsr >> 18) & 0x1,
            (xpsr >> 17) & 0x1,
            (xpsr >> 16) & 0x1,
        ));
        let ici_it = (((xpsr >> 25) & 0x3) << 6) | ((xpsr >> 10) & 0x3f);
        let thumb_bit = ((xpsr >> 24) & 0x1) == 1;
        let _ = writer.write_fmt(format_args!(
            "\
             \r\n EPSR: ICI.IT {:#04x}\
             \r\n       ThumbBit {} {}",
            ici_it,
            thumb_bit,
            if thumb_bit {
                ""
            } else {
                "!!ERROR - Cortex M Thumb only!"
            },
        ));
        let _ = writer.write_fmt(format_args!("\r\n To debug, run "));
        let _ = writer.write_fmt(format_args!(
            "`make debug RAM_START={:#x} FLASH_INIT={:#x}`",
            sram_start, flash_init_fn
        ));
        let _ = writer.write_fmt(format_args!(
            "\r\n in the app's folder and open the .lst file.\r\n\r\n"
        ));
    }
}
