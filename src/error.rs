//! Error types: libhackrf status codes and driver-level errors.

use std::fmt;

use crate::types::{Direction, StreamFormat};

/// A libhackrf status code (`enum hackrf_error`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HackrfError {
    /// `HACKRF_ERROR_INVALID_PARAM`
    InvalidParam,
    /// `HACKRF_ERROR_NOT_FOUND`
    NotFound,
    /// `HACKRF_ERROR_BUSY`
    Busy,
    /// `HACKRF_ERROR_NO_MEM`
    NoMem,
    /// `HACKRF_ERROR_LIBUSB`
    LibUsb,
    /// `HACKRF_ERROR_THREAD`
    Thread,
    /// `HACKRF_ERROR_STREAMING_THREAD_ERR`
    StreamingThreadErr,
    /// `HACKRF_ERROR_STREAMING_STOPPED`
    StreamingStopped,
    /// `HACKRF_ERROR_STREAMING_EXIT_CALLED`
    StreamingExitCalled,
    /// `HACKRF_ERROR_USB_API_VERSION`
    UsbApiVersion,
    /// `HACKRF_ERROR_NOT_LAST_DEVICE`
    NotLastDevice,
    /// `HACKRF_ERROR_OTHER`
    Other,
    /// A negative code libhackrf does not document.
    Unknown(i32),
}

impl HackrfError {
    /// Decode a raw status code. Non-negative codes are not errors and map to
    /// [`HackrfError::Unknown`]; use [`HackrfError::check`] for those.
    pub fn from_code(code: i32) -> HackrfError {
        match code {
            -2 => HackrfError::InvalidParam,
            -5 => HackrfError::NotFound,
            -6 => HackrfError::Busy,
            -11 => HackrfError::NoMem,
            -1000 => HackrfError::LibUsb,
            -1001 => HackrfError::Thread,
            -1002 => HackrfError::StreamingThreadErr,
            -1003 => HackrfError::StreamingStopped,
            -1004 => HackrfError::StreamingExitCalled,
            -1005 => HackrfError::UsbApiVersion,
            -2000 => HackrfError::NotLastDevice,
            -9999 => HackrfError::Other,
            other => HackrfError::Unknown(other),
        }
    }

    /// The raw status code.
    pub fn code(self) -> i32 {
        match self {
            HackrfError::InvalidParam => -2,
            HackrfError::NotFound => -5,
            HackrfError::Busy => -6,
            HackrfError::NoMem => -11,
            HackrfError::LibUsb => -1000,
            HackrfError::Thread => -1001,
            HackrfError::StreamingThreadErr => -1002,
            HackrfError::StreamingStopped => -1003,
            HackrfError::StreamingExitCalled => -1004,
            HackrfError::UsbApiVersion => -1005,
            HackrfError::NotLastDevice => -2000,
            HackrfError::Other => -9999,
            HackrfError::Unknown(c) => c,
        }
    }

    /// Turn a raw return value into a `Result`: `HACKRF_SUCCESS` (0) and
    /// `HACKRF_TRUE` (1) are successes, negative values are errors.
    pub fn check(code: i32) -> std::result::Result<(), HackrfError> {
        if code >= 0 {
            Ok(())
        } else {
            Err(HackrfError::from_code(code))
        }
    }

    /// Human readable name, identical to `hackrf_error_name()` (except for
    /// `HACKRF_ERROR_LIBUSB`, where libhackrf substitutes the libusb message).
    pub fn name(self) -> &'static str {
        match self {
            HackrfError::InvalidParam => "invalid parameter(s)",
            HackrfError::NotFound => "HackRF not found",
            HackrfError::Busy => "HackRF busy",
            HackrfError::NoMem => "insufficient memory",
            HackrfError::LibUsb => "USB error",
            HackrfError::Thread => "transfer thread error",
            HackrfError::StreamingThreadErr => "streaming thread encountered an error",
            HackrfError::StreamingStopped => "streaming stopped",
            HackrfError::StreamingExitCalled => "streaming terminated",
            HackrfError::UsbApiVersion => "feature not supported by installed firmware",
            HackrfError::NotLastDevice => "one or more HackRFs still in use",
            HackrfError::Other => "unspecified error",
            HackrfError::Unknown(_) => "unknown error code",
        }
    }
}

impl fmt::Display for HackrfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.code())
    }
}

impl std::error::Error for HackrfError {}

/// Driver error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A libhackrf call failed.
    Hackrf {
        /// Name of the libhackrf function.
        call: &'static str,
        /// The status it returned.
        error: HackrfError,
    },
    /// No device matched the open arguments.
    NoDeviceMatches,
    /// `hackrf_open_by_serial` failed.
    OpenFailed(HackrfError),
    /// The device handle is gone (a re-open after a stream restart failed).
    DeviceClosed,
    /// A malformed or out-of-range argument.
    InvalidArgument(String),
    /// Only channel 0 exists.
    InvalidChannel(usize),
    /// Unknown frequency component name.
    UnknownFrequencyName(String),
    /// Unknown gain element name.
    UnknownGainName(String),
    /// Unknown settings key.
    UnknownSetting(String),
    /// Unknown antenna name.
    UnknownAntenna(String),
    /// Unsupported stream format string.
    InvalidFormat(String),
    /// The sample type used for a read/write does not match the stream format.
    FormatMismatch {
        /// Format chosen when the stream was set up.
        expected: StreamFormat,
        /// Format implied by the buffer passed to the call.
        actual: StreamFormat,
    },
    /// A stream in this direction is already open on the device.
    StreamAlreadyOpen(Direction),
    /// The operation is not supported in this direction.
    NotSupported,
    /// Timed out waiting for data or buffer space (`SOAPY_SDR_TIMEOUT`).
    Timeout,
    /// RX samples were dropped because the host fell behind (`SOAPY_SDR_OVERFLOW`).
    Overflow,
    /// TX ran dry and zeros were transmitted (`SOAPY_SDR_UNDERFLOW`).
    Underflow,
    /// Streaming could not be started or failed (`SOAPY_SDR_STREAM_ERROR`).
    StreamError(String),
}

impl Error {
    /// Helper for wrapping a libhackrf status.
    pub fn hackrf(call: &'static str, error: HackrfError) -> Error {
        Error::Hackrf { call, error }
    }

    /// The SoapySDR error code the C++ driver would have returned for this
    /// error from a stream call (`SOAPY_SDR_TIMEOUT` = -1, ...).
    pub fn soapy_code(&self) -> i32 {
        match self {
            Error::Timeout => -1,
            Error::Overflow => -4,
            Error::NotSupported => -5,
            Error::Underflow => -7,
            _ => -2,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Hackrf { call, error } => write!(f, "{call}() returned {error}"),
            Error::NoDeviceMatches => f.write_str("no hackrf device matches"),
            Error::OpenFailed(e) => write!(f, "hackrf open failed: {e}"),
            Error::DeviceClosed => f.write_str("hackrf device handle is closed"),
            Error::InvalidArgument(s) => write!(f, "invalid argument: {s}"),
            Error::InvalidChannel(c) => write!(f, "invalid channel {c} (only channel 0 exists)"),
            Error::UnknownFrequencyName(n) => write!(f, "unknown frequency component {n:?}"),
            Error::UnknownGainName(n) => write!(f, "unknown gain element {n:?}"),
            Error::UnknownSetting(n) => write!(f, "unknown setting {n:?}"),
            Error::UnknownAntenna(n) => write!(f, "unknown antenna {n:?}"),
            Error::InvalidFormat(n) => write!(f, "invalid stream format {n:?}"),
            Error::FormatMismatch { expected, actual } => {
                write!(f, "stream format is {expected} but buffer is {actual}")
            }
            Error::StreamAlreadyOpen(d) => write!(f, "{d} stream already opened"),
            Error::NotSupported => f.write_str("operation not supported"),
            Error::Timeout => f.write_str("timeout"),
            Error::Overflow => f.write_str("overflow"),
            Error::Underflow => f.write_str("underflow"),
            Error::StreamError(s) => write!(f, "stream error: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<HackrfError> for Error {
    fn from(error: HackrfError) -> Error {
        Error::Hackrf {
            call: "hackrf",
            error,
        }
    }
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_round_trip() {
        for code in [
            -2, -5, -6, -11, -1000, -1001, -1002, -1003, -1004, -1005, -2000, -9999, -42,
        ] {
            assert_eq!(HackrfError::from_code(code).code(), code);
        }
        assert_eq!(HackrfError::check(0), Ok(()));
        assert_eq!(HackrfError::check(1), Ok(()));
        assert_eq!(HackrfError::check(-6), Err(HackrfError::Busy));
    }

    #[test]
    fn soapy_codes() {
        assert_eq!(Error::Timeout.soapy_code(), -1);
        assert_eq!(Error::Overflow.soapy_code(), -4);
        assert_eq!(Error::NotSupported.soapy_code(), -5);
        assert_eq!(Error::Underflow.soapy_code(), -7);
        assert_eq!(Error::StreamError("x".into()).soapy_code(), -2);
    }

    #[test]
    fn display() {
        let e = Error::hackrf("hackrf_set_freq", HackrfError::InvalidParam);
        assert_eq!(
            e.to_string(),
            "hackrf_set_freq() returned invalid parameter(s) (-2)"
        );
    }
}
