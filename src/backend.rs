//! The hardware abstraction: the subset of libhackrf the driver uses.
//!
//! [`Backend`] creates device handles, [`DeviceHandle`] exposes the per-device
//! calls. The real implementation lives in [`crate::libhackrf`] (behind the
//! `libhackrf` feature); [`crate::mock::MockBackend`] is a scriptable stand-in
//! that the test-suite uses to verify the driver logic without hardware.
//!
//! Streaming is callback based, exactly like libhackrf: the driver hands the
//! backend an [`RxHandler`] or [`TxHandler`] and the backend invokes it from
//! its own transfer thread for every USB transfer.

use std::sync::Arc;

use crate::error::HackrfError;

/// Receives one RX transfer. Return `true` to keep streaming.
pub trait RxHandler: Send + Sync {
    /// `data` holds `valid_length` interleaved I/Q bytes.
    fn on_rx(&self, data: &[i8]) -> bool;
}

/// Result of filling one TX transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxFill {
    /// Number of valid bytes written into the buffer (libhackrf pads to a
    /// 512-byte boundary and transmits only this much).
    pub valid_len: usize,
    /// `false` stops streaming; the buffer is then *not* transmitted.
    pub keep_streaming: bool,
}

/// Fills TX transfers.
pub trait TxHandler: Send + Sync {
    /// Fill `buf` (its length is the transfer size) and say how much is valid.
    fn on_tx(&self, buf: &mut [i8]) -> TxFill;
    /// Called once the device has transmitted everything that was queued
    /// after the handler stopped streaming (`hackrf_enable_tx_flush`).
    fn on_flush(&self, success: bool);
}

/// Result of `hackrf_is_streaming`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamingStatus {
    /// `HACKRF_TRUE`: transfers are flowing.
    Streaming,
    /// `HACKRF_ERROR_STREAMING_THREAD_ERR`: no transfer thread.
    ThreadError,
    /// `HACKRF_ERROR_STREAMING_STOPPED`: the stream ended (callback stopped
    /// it, a transfer failed, or stop was called).
    Stopped,
    /// `HACKRF_ERROR_STREAMING_EXIT_CALLED`: the device must be re-opened
    /// before it can stream again (older libhackrf versions).
    ExitCalled,
}

impl StreamingStatus {
    /// Decode the raw return value of `hackrf_is_streaming`.
    pub fn from_code(code: i32) -> StreamingStatus {
        match code {
            1 => StreamingStatus::Streaming,
            -1002 => StreamingStatus::ThreadError,
            -1003 => StreamingStatus::Stopped,
            _ => StreamingStatus::ExitCalled,
        }
    }
}

/// One entry of `hackrf_device_list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedDevice {
    /// Index to pass to [`Backend::open_listed`].
    pub index: usize,
    /// USB serial string, if the descriptor had one.
    pub usb_serial: Option<String>,
    /// USB board id (`enum hackrf_usb_board_id`).
    pub usb_board_id: u16,
}

/// An open HackRF device. Dropping the handle closes it.
pub trait DeviceHandle: Send + 'static {
    /// `hackrf_board_id_read`
    fn board_id(&self) -> Result<u8, HackrfError>;
    /// `hackrf_version_string_read`
    fn version_string(&self) -> Result<String, HackrfError>;
    /// `hackrf_board_partid_serialno_read`: `(part_id, serial_no)`.
    fn board_partid_serialno(&self) -> Result<([u32; 2], [u32; 4]), HackrfError>;
    /// `hackrf_si5351c_read`
    fn si5351c_read(&self, register: u16) -> Result<u16, HackrfError>;
    /// `hackrf_set_antenna_enable` (bias tee).
    fn set_antenna_enable(&self, enable: bool) -> Result<(), HackrfError>;
    /// `hackrf_set_lna_gain` (0–40 dB, 8 dB steps).
    fn set_lna_gain(&self, db: u32) -> Result<(), HackrfError>;
    /// `hackrf_set_vga_gain` (0–62 dB, 2 dB steps).
    fn set_vga_gain(&self, db: u32) -> Result<(), HackrfError>;
    /// `hackrf_set_txvga_gain` (0–47 dB).
    fn set_txvga_gain(&self, db: u32) -> Result<(), HackrfError>;
    /// `hackrf_set_amp_enable`
    fn set_amp_enable(&self, enable: bool) -> Result<(), HackrfError>;
    /// `hackrf_set_freq`
    fn set_freq(&self, hz: u64) -> Result<(), HackrfError>;
    /// `hackrf_set_sample_rate` (also resets the baseband filter).
    fn set_sample_rate(&self, rate: f64) -> Result<(), HackrfError>;
    /// `hackrf_set_baseband_filter_bandwidth`
    fn set_baseband_filter_bandwidth(&self, hz: u32) -> Result<(), HackrfError>;
    /// `hackrf_start_rx`
    fn start_rx(&self, handler: Arc<dyn RxHandler>) -> Result<(), HackrfError>;
    /// `hackrf_stop_rx`
    fn stop_rx(&self) -> Result<(), HackrfError>;
    /// `hackrf_start_tx` (with `hackrf_enable_tx_flush` when supported).
    fn start_tx(&self, handler: Arc<dyn TxHandler>) -> Result<(), HackrfError>;
    /// `hackrf_stop_tx`
    fn stop_tx(&self) -> Result<(), HackrfError>;
    /// `hackrf_is_streaming`
    fn is_streaming(&self) -> StreamingStatus;
    /// Whether [`TxHandler::on_flush`] is delivered after the handler stops
    /// streaming.
    fn supports_tx_flush(&self) -> bool {
        false
    }
}

/// Factory for device handles plus library init/exit.
pub trait Backend: Send + Sync + 'static {
    /// Device handle type.
    type Device: DeviceHandle;
    /// `hackrf_init` (reference counted by the implementation).
    fn init(&self) -> Result<(), HackrfError>;
    /// `hackrf_exit` (reference counted by the implementation).
    fn exit(&self) -> Result<(), HackrfError>;
    /// `hackrf_device_list`
    fn list_devices(&self) -> Result<Vec<ListedDevice>, HackrfError>;
    /// `hackrf_device_list_open`
    fn open_listed(self: &Arc<Self>, index: usize) -> Result<Self::Device, HackrfError>;
    /// `hackrf_open_by_serial`
    fn open_by_serial(self: &Arc<Self>, serial: &str) -> Result<Self::Device, HackrfError>;
}

/// RAII guard pairing [`Backend::init`] with [`Backend::exit`]
/// (the C++ `SoapyHackRFSession`).
pub struct Session<B: Backend> {
    backend: Arc<B>,
}

impl<B: Backend> Session<B> {
    /// Initialise the library (or bump its reference count).
    pub fn new(backend: Arc<B>) -> Result<Session<B>, HackrfError> {
        backend.init()?;
        Ok(Session { backend })
    }

    /// The backend this session belongs to.
    pub fn backend(&self) -> &Arc<B> {
        &self.backend
    }
}

impl<B: Backend> Drop for Session<B> {
    fn drop(&mut self) {
        // Errors cannot be propagated from Drop; the C++ driver only logged.
        let _ = self.backend.exit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_status_codes() {
        assert_eq!(StreamingStatus::from_code(1), StreamingStatus::Streaming);
        assert_eq!(
            StreamingStatus::from_code(-1002),
            StreamingStatus::ThreadError
        );
        assert_eq!(StreamingStatus::from_code(-1003), StreamingStatus::Stopped);
        assert_eq!(
            StreamingStatus::from_code(-1004),
            StreamingStatus::ExitCalled
        );
    }
}
