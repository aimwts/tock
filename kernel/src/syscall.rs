//! Tock syscall number definitions.

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

/// This trait must be implemented by the architecture of the chip Tock is
/// running on. It allows the kernel to manage processes in an
/// architecture-agnostic manner.
pub trait SyscallInterface {
    /// Allows the kernel to query the architecture to see if a syscall occurred
    /// for the currently running process.
    fn get_syscall_fired(&self) -> bool;

    /// Get the syscall that the process called.
    fn get_syscall_number(&self, stack_pointer: *const u8) -> Option<Syscall>;

    /// Get the four u32 values that the process can pass with the syscall.
    fn get_syscall_data(&self, stack_pointer: *const u8) -> (u32, u32, u32, u32);

    /// Replace the last stack frame with the new function call. This function
    /// is what should be executed when the process is resumed.
    fn replace_function_call(&self, stack_pointer: *const u8, callback: FunctionCall);

    /// Context switch to a specific process.
    fn switch_to_process(&self, stack_pointer: *const u8) -> *mut u8;
}
