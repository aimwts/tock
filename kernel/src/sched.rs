//! Tock core scheduler.

use core::cell::Cell;
use core::ptr;
use core::ptr::NonNull;

use callback;
use callback::{AppId, Callback};
use common::cells::NumericCellExt;
use ipc;
use mem::AppSlice;
use memop;
use platform::mpu::MPU;
use platform::systick::SysTick;
use platform::{Chip, Platform};
use process;
use process::{Process, Task};
use returncode::ReturnCode;
use syscall::{Syscall, SyscallInterface, ContextSwitchReason};

/// The time a process is permitted to run before being pre-empted
const KERNEL_TICK_DURATION_US: u32 = 10000;
/// Skip re-scheduling a process if its quanta is nearly exhausted
const MIN_QUANTA_THRESHOLD_US: u32 = 500;

/// Main object for the kernel. Each board will need to create one.
pub struct Kernel<S: 'static + SyscallInterface> {
    /// How many "to-do" items exist at any given time. These include
    /// outstanding callbacks and processes in the Running state.
    work: Cell<usize>,
    /// This holds a pointer to the static array of Process pointers.
    processes: &'static [Option<&'static mut Process<'static, S>>],
}

impl<S: 'static + SyscallInterface> Kernel<S> {
    pub fn new(processes: &'static [Option<&'static mut Process<'static, S>>]) -> Kernel<S> {
        Kernel {
            work: Cell::new(0),
            processes: processes,
        }
    }

    /// Something was scheduled for a process, so there is more work to do.
    crate fn increment_work(&self) {
        self.work.increment();
    }

    /// Something finished for a process, so we decrement how much work there is
    /// to do.
    crate fn decrement_work(&self) {
        self.work.decrement();
    }

    /// Helper function for determining if we should service processes or go to
    /// sleep.
    fn processes_blocked(&self) -> bool {
        self.work.get() == 0
    }

    /// Run a closure on a specific process if it exists. If the process does
    /// not exist (i.e. it is `None` in the `processes` array) then `default`
    /// will be returned. Otherwise the closure will executed and passed a
    /// reference to the process.
    crate fn process_map_or<F, R>(&self, default: R, process_index: usize, closure: F) -> R
    where
        F: FnOnce(&Process<S>) -> R,
    {
        if process_index > self.processes.len() {
            return default;
        }
        self.processes[process_index]
            .as_ref()
            .map_or(default, |process| closure(process))
    }

    /// Run a closure on every valid process. This will iterate the array of
    /// processes and call the closure on every process that exists.
    crate fn process_each_enumerate<F>(&self, closure: F)
    where
        F: Fn(usize, &Process<S>),
    {
        for (i, process) in self.processes.iter().enumerate() {
            match process {
                Some(ref p) => {
                    closure(i, p);
                }
                None => {}
            }
        }
    }

    /// Run a closure on every process, but only continue if the closure returns
    /// `FAIL`. That is, if the closure returns any other return code than
    /// `FAIL`, that value will be returned from this function and the iteration
    /// of the array of processes will stop.
    crate fn process_each_enumerate_stop<F>(&self, closure: F) -> ReturnCode
    where
        F: Fn(usize, &Process<S>) -> ReturnCode,
    {
        for (i, process) in self.processes.iter().enumerate() {
            match process {
                Some(ref p) => {
                    let ret = closure(i, p);
                    if ret != ReturnCode::FAIL {
                        return ret;
                    }
                }
                None => {}
            }
        }
        ReturnCode::FAIL
    }

    /// Return how many processes this board supports.
    crate fn number_of_process_slots(&self) -> usize {
        self.processes.len()
    }

    /// Main loop.
    pub fn kernel_loop<P: Platform, C: Chip>(
        &'static self,
        platform: &P,
        chip: &mut C,
        ipc: Option<&ipc::IPC<S>>,
    ) {
        loop {
            unsafe {
                chip.service_pending_interrupts();

                for (i, p) in self.processes.iter().enumerate() {
                    p.as_ref().map(|process| {
                        self.do_process(
                            platform,
                            chip,
                            process,
                            callback::AppId::new(self, i),
                            ipc,
                        );
                    });
                    if chip.has_pending_interrupts() {
                        break;
                    }
                }

                chip.atomic(|| {
                    if !chip.has_pending_interrupts() && self.processes_blocked() {
                        chip.sleep();
                    }
                });
            };
        }
    }

    unsafe fn do_process<P: Platform, C: Chip>(
        &'static self,
        platform: &P,
        chip: &mut C,
        process: &Process<S>,
        appid: AppId<S>,
        ipc: Option<&::ipc::IPC<S>>,
    ) {
        let systick = chip.systick();
        systick.reset();
        systick.set_timer(KERNEL_TICK_DURATION_US);
        systick.enable(true);

        loop {
            if chip.has_pending_interrupts()
                || systick.overflowed()
                || !systick.greater_than(MIN_QUANTA_THRESHOLD_US)
            {
                break;
            }

            match process.current_state() {
                process::State::Running => {
                    process.setup_mpu(chip.mpu());
                    chip.mpu().enable_mpu();
                    systick.enable(true);
                    process.switch_to();

                    let new_sp = chip.syscall().switch_to_process(process.sp(), process.stored_state());


                    systick.enable(false);
                    chip.mpu().disable_mpu();
                }
                process::State::Yielded => match process.dequeue_task() {
                    None => break,
                    Some(cb) => {
                        match cb {
                            Task::FunctionCall(ccb) => {
                                process.push_function_call(ccb);
                            }
                            Task::IPC((otherapp, ipc_type)) => {
                                ipc.map_or_else(
                                    || {
                                        assert!(
                                            false,
                                            "Kernel consistency error: IPC Task with no IPC"
                                        );
                                    },
                                    |ipc| {
                                        ipc.schedule_callback(appid, otherapp, ipc_type);
                                    },
                                );
                            }
                        }
                        continue;
                    }
                },
                process::State::Fault => {
                    // we should never be scheduling a process in fault
                    panic!("Attempted to schedule a faulty process");
                }
            }

            // Check why the process stopped running, and handle it correctly.
            let process_state = chip.syscall().get_context_switch_reason();
            match process_state {
                ContextSwitchReason::Fault => {
                    // Let process deal with it as appropriate.
                    process.fault_state();
                    continue;
                }
                ContextSwitchReason::SyscallFired => {
                    // Keep running this function.
                }
                ContextSwitchReason::Other => {
                    // break to handle other processes.
                    break;
                }
            }

            // // Check if the reason this process stopped executing was that it
            // // called a syscall. If it did, then we can handle that syscall. If
            // // it didn't then we need to service the other processes.
            // if !chip.syscall().get_syscall_fired() {
            //     break;
            // }

            // // check if the app had a fault
            // if process.app_fault() {
            //     // let process deal with it as appropriate
            //     process.fault_state();
            //     continue;
            // }



            // Get which syscall the process called.
            let svc_number = chip.syscall().get_syscall_number(process.sp());

            // Process had a system call, count it for debugging purposes.
            process.incr_syscall_count(svc_number);

            // Retrieve the four values a process can pass with a syscall. These
            // may not all be used depending on which syscall it is.
            let (r0, r1, r2, r3) = chip.syscall().get_syscall_data(process.sp());

            match svc_number {
                Some(Syscall::MEMOP) => {
                    let res = memop::memop(process, r0, r1);
                    chip.syscall().set_syscall_return_value(process.sp(), res.into());
                }
                Some(Syscall::YIELD) => {
                    process.yield_state();
                    process.pop_syscall_stack();

                    // There might be already enqueued callbacks
                    continue;
                }
                Some(Syscall::SUBSCRIBE) => {
                    let driver_num = r0;
                    let subdriver_num = r1;
                    let callback_ptr_raw = r2 as *mut ();
                    let appdata = r3;

                    let callback_ptr = NonNull::new(callback_ptr_raw);
                    let callback =
                        callback_ptr.map(|ptr| Callback::new(appid, appdata, ptr.cast()));

                    let res = platform.with_driver(driver_num, |driver| match driver {
                        Some(d) => d.subscribe(subdriver_num, callback, appid),
                        None => ReturnCode::ENODEVICE,
                    });
                    chip.syscall().set_syscall_return_value(process.sp(), res.into());
                }
                Some(Syscall::COMMAND) => {
                    let res = platform.with_driver(r0, |driver| match driver {
                        Some(d) => d.command(r1, r2, r3, appid),
                        None => ReturnCode::ENODEVICE,
                    });
                    chip.syscall().set_syscall_return_value(process.sp(), res.into());
                }
                Some(Syscall::ALLOW) => {
                    let res = platform.with_driver(r0, |driver| {
                        match driver {
                            Some(d) => {
                                let start_addr = r2 as *mut u8;
                                if start_addr != ptr::null_mut() {
                                    let size = r3;
                                    if process.in_exposed_bounds(start_addr, size) {
                                        let slice =
                                            AppSlice::new(self, start_addr as *mut u8, size, appid);
                                        d.allow(appid, r1, Some(slice))
                                    } else {
                                        ReturnCode::EINVAL /* memory not allocated to process */
                                    }
                                } else {
                                    d.allow(appid, r1, None)
                                }
                            }
                            None => ReturnCode::ENODEVICE,
                        }
                    });
                    chip.syscall().set_syscall_return_value(process.sp(), res.into());
                }
                _ => {}
            }
        }
        systick.reset();
    }
}
