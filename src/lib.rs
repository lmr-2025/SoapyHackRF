//! A Rust port of [SoapyHackRF](https://github.com/pothosware/SoapyHackRF),
//! the SoapySDR support module for the HackRF One.
//!
//! The crate reproduces the driver's behaviour (device discovery, settings,
//! half-duplex RX/TX switching, ring-buffered streaming in CS8/CS16/CF32/CF64,
//! burst transmission) on top of a small hardware abstraction
//! ([`backend::Backend`]) so that every piece of logic can be exercised
//! without a radio attached, using [`mock::MockBackend`].
//!
//! * [`HackRf`] is the device object: the SoapySDR `Device` API in snake case,
//!   returning `Result` instead of logging and throwing.
//! * [`RxStream`] / [`TxStream`] are typed stream handles (in the spirit of
//!   seify's `RxStreamer`/`TxStreamer`) created with [`HackRf::rx_stream`] and
//!   [`HackRf::tx_stream`]. They borrow the device, so they cannot outlive it.
//! * [`find_hackrf`] enumerates attached devices.
//! * With the `libhackrf` feature (default) the [`libhackrf::LibHackrf`]
//!   backend links against the C library and talks to real hardware.
//!
//! The known defects of the C++ implementation and how this port treats each
//! of them are listed in `BUGS.md`.
//!
//! ```no_run
//! use soapyhackrf::{Direction, HackRfDevice, Kwargs, StreamFormat};
//! use std::time::Duration;
//!
//! let dev = HackRfDevice::open_default(&Kwargs::new())?;
//! dev.set_sample_rate(Direction::Rx, 0, 10e6)?;
//! dev.set_frequency(Direction::Rx, 0, "RF", 100e6, &Kwargs::new())?;
//! let mut rx = dev.rx_stream(StreamFormat::CF32, &[0], &Kwargs::new())?;
//! rx.activate()?;
//! let mut buf = vec![0f32; 2 * rx.mtu()];
//! let n = rx.read(&mut buf, Duration::from_millis(100))?.samples;
//! println!("got {n} samples");
//! # Ok::<(), soapyhackrf::Error>(())
//! ```

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod convert;
pub mod device;
pub mod enumerate;
pub mod error;
pub mod gain;
pub mod mock;
pub mod ring;
pub mod stream;
pub mod types;

#[cfg(feature = "libhackrf")]
pub mod ffi;
#[cfg(feature = "libhackrf")]
pub mod libhackrf;

pub use backend::{Backend, DeviceHandle, ListedDevice, Session, StreamingStatus};
pub use convert::{Sample, SampleBuf, SampleBufMut, CONVERSION_SCALE, NATIVE_FULL_SCALE};
pub use device::HackRf;
pub use enumerate::{claimed_serials, find_hackrf};
pub use error::{Error, HackrfError, Result};
pub use stream::{ReadResult, RxBuffer, RxStream, StreamEvent, TxBuffer, TxStream};
pub use types::*;

/// A device driven by the real libhackrf backend.
#[cfg(feature = "libhackrf")]
pub type HackRfDevice = HackRf<libhackrf::LibHackrf>;

#[cfg(feature = "libhackrf")]
impl HackRfDevice {
    /// Open a device through libhackrf, selecting it with `args`
    /// (`serial`, `hackrf` index) or taking the first one found.
    pub fn open_default(args: &Kwargs) -> Result<HackRfDevice> {
        HackRf::open(libhackrf::LibHackrf::shared(), args)
    }

    /// Enumerate devices through libhackrf.
    pub fn enumerate_default(args: &Kwargs) -> Result<Vec<Kwargs>> {
        find_hackrf(&libhackrf::LibHackrf::shared(), args)
    }
}
