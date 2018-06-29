//! Tock syscall number definitions and arch-agnostic interface trait.

use process;

/// The syscall number assignments.
#[derive(Copy, Clone, Debug)]
pub enum Syscall {
    /// Return to the kernel to allow other processes to execute or to wait for
    /// interrupts and callbacks.
    YIELD = 0,

    /// Pass a callback function to the kernel.
    SUBSCRIBE = 1,

    /// Instruct the kernel or a capsule to perform an operation.
    COMMAND = 2,

    /// Share a memory buffer with the kernel.
    ALLOW = 3,

    /// Various memory operations.
    MEMOP = 4,
}

/// Why the process stopped executing and execution returned to the kernel.
pub enum ContextSwitchReason {
    /// Process exceeded its timeslice, otherwise catch-all.
    Other,
    /// Process called a syscall.
    SyscallFired,
    /// Process triggered the hardfault handler.
    Fault,
}

// /// Architecture-specific state that must be stored for a process while it is
// /// not executing.
// pub enum ArchStoredValue {
//     CortexM {
//         r4: usize,
//         r5: usize,
//         r6: usize,
//         r7: usize,
//         r8: usize,
//         r9: usize,
//         r10: usize,
//         r11: usize,
//     }
// }

/// This trait must be implemented by the architecture of the chip Tock is
/// running on. It allows the kernel to manage processes in an
/// architecture-agnostic manner.
pub trait SyscallInterface {
    /// Some architecture-specific struct containing per-process state that must
    /// be kept while the process is not running.
    type StoredState;

    /// Allows the kernel to query to see why the process stopped running. This
    /// function can only be called once to get the last state of the process
    /// and why the process context switched back to the kernel.
    ///
    /// An implementor of this function is free to reset any state that was
    /// needed to gather this information when this function is called.
    fn get_context_switch_reason(&self) -> ContextSwitchReason;

    /// Get the syscall that the process called.
    fn get_syscall_number(&self, stack_pointer: *const usize) -> Option<Syscall>;

    /// Get the four u32 values that the process can pass with the syscall.
    fn get_syscall_data(&self, stack_pointer: *const usize) -> (usize, usize, usize, usize);

    /// Set the return value the process should see when it begins executing
    /// again after the syscall.
    fn set_syscall_return_value(&self, stack_pointer: *const usize, return_value: isize);

    /// Replace the last stack frame with the new function call. This function
    /// is what should be executed when the process is resumed.
    fn replace_function_call(&self, stack_pointer: *const usize, callback: process::FunctionCall);

    /// Context switch to a specific process.
    fn switch_to_process(&self, stack_pointer: *const usize, state: &mut Self::StoredState) -> *mut u8;
}
