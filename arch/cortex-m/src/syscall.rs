//! Implementation of the architecture-specific portions of the kernel-userland
//! system call interface.


use core::ptr::{read_volatile, write_volatile};

use kernel;
// use kernel::common::cells::VolatileCell;
// use kernel::common::math::PowerOfTwo;
// use kernel::common::StaticRef;


/// This is used in the syscall handler.
#[allow(private_no_mangle_statics)]
#[no_mangle]
#[used]
static mut PROCESS_STATE: usize = 0;

// #[allow(improper_ctypes)]
// extern "C" {
//     pub fn switch_to_user(user_stack: *const u8, process_regs: &mut [usize; 8]) -> *mut u8;
// }

#[derive(Default)]
struct StoredRegs {
    r4: usize,
    r5: usize,
    r6: usize,
    r7: usize,
    r8: usize,
    r9: usize,
    r10: usize,
    r11: usize,
}

/// Constructor field is private to limit who can create a new one.
pub struct SysCall();

impl SysCall {
    pub const unsafe fn new() -> SysCall {
        SysCall()
    }
}


impl kernel::syscall::SyscallInterface for SysCall {
    type StoredState = StoredRegs;

    fn get_context_switch_reason(&self) -> kernel::syscall::ContextSwitchReason {
        unsafe {
            let state = read_volatile(&PROCESS_STATE);
            // We are free to reset this immediately as this function will only
            // get called once.
            write_volatile(&mut PROCESS_STATE, 0);
            match state {
                0 => kernel::syscall::ContextSwitchReason::Other,
                1 => kernel::syscall::ContextSwitchReason::SyscallFired,
                2 => kernel::syscall::ContextSwitchReason::Fault,
                _ => kernel::syscall::ContextSwitchReason::Other,
            }
        }
    }

    /// Get the syscall that the process called.
    fn get_syscall_number(&self, stack_pointer: *const usize) -> Option<kernel::syscall::Syscall> {
        let sp = stack_pointer as *const *const u16;
        unsafe {
            let pcptr = read_volatile((sp as *const *const u16).offset(6));
            let svc_instr = read_volatile(pcptr.offset(-1));
            let svc_num = (svc_instr & 0xff) as u8;
            match svc_num {
                0 => Some(kernel::syscall::Syscall::YIELD),
                1 => Some(kernel::syscall::Syscall::SUBSCRIBE),
                2 => Some(kernel::syscall::Syscall::COMMAND),
                3 => Some(kernel::syscall::Syscall::ALLOW),
                4 => Some(kernel::syscall::Syscall::MEMOP),
                _ => None,
            }
        }
    }

    /// Get the four u32 values that the process can pass with the syscall.
    fn get_syscall_data(&self, stack_pointer: *const usize) -> (usize, usize, usize, usize) {
        let sp = stack_pointer as *const usize;
        unsafe {
            let r0 = read_volatile(sp.offset(0));
            let r1 = read_volatile(sp.offset(1));
            let r2 = read_volatile(sp.offset(2));
            let r3 = read_volatile(sp.offset(3));
            (r0, r1, r2, r3)
        }
    }

    fn set_syscall_return_value(&self, stack_pointer: *const usize, return_value: isize) {
        // For the Cortex-M arch we set this in the same place that r0 was
        // passed.
        let sp = stack_pointer as *mut isize;
        unsafe {
            write_volatile(sp, return_value);
        }
    }

    /// Replace the last stack frame with the new function call. This function
    /// is what should be executed when the process is resumed.
    fn replace_function_call(&self, stack_pointer: *const usize, callback: kernel::procs::FunctionCall) {

    }

    /// Context switch to a specific process.
    fn switch_to_process(&self, stack_pointer: *const usize, state: &kernel::syscall::ArchStoredValue) -> *mut u8 {
        &mut stack_pointer

        // write_volatile(&mut SYSCALL_FIRED, 0);
        switch_to_user(
            stack_pointer,
            &mut *(&mut self.stored_regs as *mut StoredRegs as *mut [usize; 8]),
        )
        // self.current_stack_pointer = psp;
        // if self.current_stack_pointer < self.debug.min_stack_pointer {
        //     self.debug.min_stack_pointer = self.current_stack_pointer;
        // }
    }
}
