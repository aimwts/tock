//! Debug capsule to cause a button press to restart all apps.
//!
//! This is useful for debugging that capsules and apps work when they are
//! restarted by the kernel.
//!
//! Usage
//! -----
//!
//! ```rust
//! let debug_process_restart = static_init!(
//!     capsules::debug_process_restart::DebugProcessRestart<'static, sam4l::gpio::GPIOPin>,
//!     capsules::debug_process_restart::DebugProcessRestart::new(&sam4l::gpio::PA[16])
//! );
//! sam4l::gpio::PA[16].set_client(debug_process_restart);
//! }
//! ```

use kernel::capabilities::ProcessManagementCapability;
use kernel::hil;
use kernel::hil::gpio::{Client, InterruptMode};
use kernel::procs::hardfault_all_apps;

pub struct DebugProcessRestart<'a, G: hil::gpio::Pin + 'a, C: ProcessManagementCapability> {
    _pin: &'a G,
    capability: C,
}

impl<'a, G: hil::gpio::Pin + hil::gpio::PinCtl, C: ProcessManagementCapability>
    DebugProcessRestart<'a, G, C>
{
    pub fn new(pin: &'a G, cap: C) -> DebugProcessRestart<'a, G, C> {
        pin.make_input();
        pin.enable_interrupt(0, InterruptMode::RisingEdge);

        DebugProcessRestart {
            _pin: pin,
            capability: cap,
        }
    }
}

impl<'a, G: hil::gpio::Pin + hil::gpio::PinCtl, C: ProcessManagementCapability> Client
    for DebugProcessRestart<'a, G, C>
{
    fn fired(&self, _pin_num: usize) {
        hardfault_all_apps(&self.capability);
    }
}
