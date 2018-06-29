//! Implementation of the architecture-specific portions of the kernel-userland
//! system call interface.


use core::ptr::{read_volatile, write, write_volatile};

use kernel;
use kernel::common::cells::VolatileCell;
use kernel::common::math::PowerOfTwo;
use kernel::common::StaticRef;


/// This is used in the syscall handler.
#[allow(private_no_mangle_statics)]
#[no_mangle]
#[used]
static mut SYSCALL_FIRED: usize = 0;


/// Constructor field is private to limit who can create a new one.
pub struct SysCall();

impl SysCall {
    pub const unsafe fn new() -> SysCall {
        SysCall()
    }
}


impl kernel::syscall::SyscallInterface for SysCall {
    /// Allows the kernel to query the architecture to see if a syscall occurred
    /// for the currently running process.
    fn get_syscall_fired(&self) -> bool {
        unsafe {
            read_volatile(&SYSCALL_FIRED) != 0
        }
    }

    /// Get the syscall that the process called.
    fn get_syscall_number(&self, stack_pointer: *const u8) -> Option<kernel::syscall::Syscall> {
        None
    }

    /// Get the four u32 values that the process can pass with the syscall.
    fn get_syscall_data(&self, stack_pointer: *const u8) -> (u32, u32, u32, u32) {
        (0, 0, 0, 0)
    }

    /// Replace the last stack frame with the new function call. This function
    /// is what should be executed when the process is resumed.
    fn replace_function_call(&self, stack_pointer: *const u8, callback: kernel::procs::FunctionCall) {

    }

    // /// Context switch to a specific process.
    // fn switch_to_process(&self, stack_pointer: *const u8) -> *mut u8 {
    //     &mut stack_pointer
    // }
}
