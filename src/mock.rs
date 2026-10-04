//! A scriptable in-memory [`Backend`] used by the test-suite.
//!
//! It models the parts of libhackrf behaviour the driver depends on:
//! reference-counted init/exit, device enumeration, "busy" devices, gain
//! range validation and masking, the automatic baseband filter reset on
//! sample-rate changes, the streaming state machine (including the old
//! libhackrf behaviour where a stopped device must be re-opened before it can
//! stream again), and callback driven RX/TX transfers that tests pump by
//! hand with [`MockBackend::pump_rx`] / [`MockBackend::pump_tx`].
//!
//! Every call is appended to a log so tests can assert on the exact sequence
//! of hardware operations.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::backend::{
    Backend, DeviceHandle, ListedDevice, RxHandler, StreamingStatus, TxFill, TxHandler,
};
use crate::error::HackrfError;
use crate::types::{
    auto_baseband_filter_bw, format_serial, TransceiverMode, BUF_LEN, RX_LNA_MAX_DB, RX_VGA_MAX_DB,
    TX_VGA_MAX_DB,
};

/// Static description of a simulated device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockDeviceInfo {
    /// Raw board id byte.
    pub board_id: u8,
    /// Firmware version string.
    pub version: String,
    /// MCU part id.
    pub part_id: [u32; 2],
    /// MCU serial number words; the string serial is derived from them.
    pub serial_no: [u32; 4],
    /// Value of si5351c register 0 (`0x51` = internal clock).
    pub si5351c_reg0: u16,
    /// Whether the USB descriptor carries a serial string.
    pub has_usb_serial: bool,
}

impl MockDeviceInfo {
    /// A HackRF One with the given serial words and typical defaults.
    pub fn new(serial_no: [u32; 4]) -> MockDeviceInfo {
        MockDeviceInfo {
            board_id: 2,
            version: "2023.01.1".to_string(),
            part_id: [0xa000cb3c, 0x00514f4e],
            serial_no,
            si5351c_reg0: 0x51,
            has_usb_serial: true,
        }
    }

    /// The 32 character hex serial string.
    pub fn serial(&self) -> String {
        format_serial(self.serial_no)
    }
}

/// One recorded backend call.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    /// `hackrf_init`
    Init,
    /// `hackrf_exit`
    Exit,
    /// `hackrf_device_list`
    DeviceList,
    /// `hackrf_device_list_open(index)`
    OpenListed(usize),
    /// `hackrf_open_by_serial(serial)`
    OpenBySerial(String),
    /// `hackrf_close` of the device with this serial
    Close(String),
    /// `hackrf_board_id_read`
    BoardIdRead,
    /// `hackrf_version_string_read`
    VersionStringRead,
    /// `hackrf_board_partid_serialno_read`
    PartIdSerialRead,
    /// `hackrf_si5351c_read(register)`
    Si5351cRead(u16),
    /// `hackrf_set_antenna_enable`
    SetAntennaEnable(bool),
    /// `hackrf_set_lna_gain`
    SetLnaGain(u32),
    /// `hackrf_set_vga_gain`
    SetVgaGain(u32),
    /// `hackrf_set_txvga_gain`
    SetTxVgaGain(u32),
    /// `hackrf_set_amp_enable`
    SetAmpEnable(bool),
    /// `hackrf_set_freq`
    SetFreq(u64),
    /// `hackrf_set_sample_rate`
    SetSampleRate(f64),
    /// `hackrf_set_baseband_filter_bandwidth`
    SetBasebandFilterBandwidth(u32),
    /// `hackrf_start_rx`
    StartRx,
    /// `hackrf_stop_rx`
    StopRx,
    /// `hackrf_start_tx`
    StartTx,
    /// `hackrf_stop_tx`
    StopTx,
    /// `hackrf_is_streaming`
    IsStreaming,
}

impl Call {
    /// Name used for fault injection (`MockBackend::fail_next`).
    pub fn name(&self) -> &'static str {
        match self {
            Call::Init => "init",
            Call::Exit => "exit",
            Call::DeviceList => "device_list",
            Call::OpenListed(_) => "open_listed",
            Call::OpenBySerial(_) => "open_by_serial",
            Call::Close(_) => "close",
            Call::BoardIdRead => "board_id_read",
            Call::VersionStringRead => "version_string_read",
            Call::PartIdSerialRead => "partid_serialno_read",
            Call::Si5351cRead(_) => "si5351c_read",
            Call::SetAntennaEnable(_) => "set_antenna_enable",
            Call::SetLnaGain(_) => "set_lna_gain",
            Call::SetVgaGain(_) => "set_vga_gain",
            Call::SetTxVgaGain(_) => "set_txvga_gain",
            Call::SetAmpEnable(_) => "set_amp_enable",
            Call::SetFreq(_) => "set_freq",
            Call::SetSampleRate(_) => "set_sample_rate",
            Call::SetBasebandFilterBandwidth(_) => "set_baseband_filter_bandwidth",
            Call::StartRx => "start_rx",
            Call::StopRx => "stop_rx",
            Call::StartTx => "start_tx",
            Call::StopTx => "stop_tx",
            Call::IsStreaming => "is_streaming",
        }
    }
}

/// Simulated register state of one device.
#[derive(Clone)]
pub struct HwState {
    /// Tuned frequency in Hz.
    pub freq: u64,
    /// Sample rate.
    pub sample_rate: f64,
    /// Baseband filter bandwidth in Hz.
    pub bandwidth: u32,
    /// LNA gain after libhackrf masking.
    pub lna: u32,
    /// RX VGA gain after libhackrf masking.
    pub vga: u32,
    /// TX VGA gain.
    pub txvga: u32,
    /// RF amplifier enabled.
    pub amp: bool,
    /// Bias tee enabled.
    pub antenna: bool,
    /// Transceiver mode last commanded.
    pub mode: TransceiverMode,
    /// Transfers set up (`transfer_thread_started`).
    pub transfers_setup: bool,
    /// `streaming` flag.
    pub streaming: bool,
    /// `do_exit` flag (old libhackrf: set by stop, cleared by re-open).
    pub do_exit: bool,
    /// Flush callback armed.
    pub flush_pending: bool,
    /// Number of RX transfers pumped.
    pub rx_transfers: u64,
    /// Number of TX transfers pumped.
    pub tx_transfers: u64,
    rx_handler: Option<Arc<dyn RxHandler>>,
    tx_handler: Option<Arc<dyn TxHandler>>,
}

impl std::fmt::Debug for HwState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HwState")
            .field("freq", &self.freq)
            .field("sample_rate", &self.sample_rate)
            .field("bandwidth", &self.bandwidth)
            .field("lna", &self.lna)
            .field("vga", &self.vga)
            .field("txvga", &self.txvga)
            .field("amp", &self.amp)
            .field("antenna", &self.antenna)
            .field("mode", &self.mode)
            .field("transfers_setup", &self.transfers_setup)
            .field("streaming", &self.streaming)
            .field("do_exit", &self.do_exit)
            .field("flush_pending", &self.flush_pending)
            .field("rx_transfers", &self.rx_transfers)
            .field("tx_transfers", &self.tx_transfers)
            .finish()
    }
}

impl Default for HwState {
    fn default() -> HwState {
        HwState {
            freq: 0,
            sample_rate: 0.0,
            bandwidth: 0,
            lna: 0,
            vga: 0,
            txvga: 0,
            amp: false,
            antenna: false,
            mode: TransceiverMode::Off,
            transfers_setup: false,
            streaming: false,
            do_exit: false,
            flush_pending: false,
            rx_transfers: 0,
            tx_transfers: 0,
            rx_handler: None,
            tx_handler: None,
        }
    }
}

struct MockState {
    devices: Vec<MockDeviceInfo>,
    open: BTreeSet<String>,
    hw: BTreeMap<String, HwState>,
    calls: Vec<Call>,
    sessions: usize,
    faults: BTreeMap<&'static str, VecDeque<HackrfError>>,
    legacy_restart_bug: bool,
    tx_flush_supported: bool,
    auto_flush: bool,
    transfer_len: usize,
}

/// The mock backend. Create it with [`MockBackend::new`] and share it as an
/// `Arc`.
pub struct MockBackend {
    state: Mutex<MockState>,
}

impl MockBackend {
    /// A backend with the given simulated devices attached.
    pub fn new(devices: Vec<MockDeviceInfo>) -> Arc<MockBackend> {
        Arc::new(MockBackend {
            state: Mutex::new(MockState {
                devices,
                open: BTreeSet::new(),
                hw: BTreeMap::new(),
                calls: Vec::new(),
                sessions: 0,
                faults: BTreeMap::new(),
                legacy_restart_bug: false,
                tx_flush_supported: true,
                auto_flush: true,
                transfer_len: BUF_LEN,
            }),
        })
    }

    /// A backend with one default device.
    pub fn single() -> (Arc<MockBackend>, MockDeviceInfo) {
        let info = MockDeviceInfo::new([0, 0, 0xa06063c8, 0x2e6b6b1f]);
        (MockBackend::new(vec![info.clone()]), info)
    }

    fn lock(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn record(st: &mut MockState, call: Call) -> Result<(), HackrfError> {
        let name = call.name();
        st.calls.push(call);
        if let Some(q) = st.faults.get_mut(name) {
            if let Some(err) = q.pop_front() {
                return Err(err);
            }
        }
        Ok(())
    }

    /// Make the next call named `call` (see [`Call::name`]) fail with `err`.
    /// Repeated registrations queue up.
    pub fn fail_next(&self, call: &'static str, err: HackrfError) {
        self.lock().faults.entry(call).or_default().push_back(err);
    }

    /// Simulate old libhackrf releases where a device cannot restart
    /// streaming after `stop_rx`/`stop_tx` until it is closed and re-opened
    /// (`hackrf_is_streaming` keeps returning `STREAMING_EXIT_CALLED`).
    pub fn set_legacy_restart_bug(&self, enabled: bool) {
        self.lock().legacy_restart_bug = enabled;
    }

    /// Whether devices report TX flush support (default: true).
    pub fn set_tx_flush_supported(&self, supported: bool) {
        self.lock().tx_flush_supported = supported;
    }

    /// Whether the flush callback fires immediately when the TX handler stops
    /// streaming (default: true). When false, call
    /// [`MockBackend::complete_flush`] by hand.
    pub fn set_auto_flush(&self, auto: bool) {
        self.lock().auto_flush = auto;
    }

    /// Size of simulated transfers (default [`BUF_LEN`]).
    pub fn set_transfer_len(&self, len: usize) {
        self.lock().transfer_len = len;
    }

    /// All recorded calls so far.
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Discard the call log.
    pub fn clear_calls(&self) {
        self.lock().calls.clear();
    }

    /// Recorded calls since the last `clear_calls`, consumed.
    pub fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut self.lock().calls)
    }

    /// Number of live sessions (`init` minus `exit`).
    pub fn sessions(&self) -> usize {
        self.lock().sessions
    }

    /// Serials of currently open devices.
    pub fn open_serials(&self) -> Vec<String> {
        self.lock().open.iter().cloned().collect()
    }

    /// Snapshot of a device's simulated hardware state.
    pub fn hw(&self, serial: &str) -> HwState {
        self.lock().hw.get(serial).cloned().unwrap_or_default()
    }

    /// Attach another device at runtime.
    pub fn add_device(&self, info: MockDeviceInfo) {
        self.lock().devices.push(info);
    }

    /// Deliver one RX transfer to the device's RX handler. Returns the
    /// handler's "keep streaming" answer, or `None` if the device is not
    /// receiving.
    pub fn pump_rx(&self, serial: &str, data: &[i8]) -> Option<bool> {
        let handler = {
            let mut st = self.lock();
            let hw = st.hw.get_mut(serial)?;
            if !hw.streaming || hw.do_exit || hw.mode != TransceiverMode::Rx {
                return None;
            }
            hw.rx_transfers += 1;
            hw.rx_handler.clone()?
        };
        let keep = handler.on_rx(data);
        if !keep {
            if let Some(hw) = self.lock().hw.get_mut(serial) {
                hw.streaming = false;
            }
        }
        Some(keep)
    }

    /// Ask the device's TX handler to fill one transfer. Returns the buffer
    /// (full transfer length, zero padded) and the handler's answer, or
    /// `None` if the device is not transmitting.
    pub fn pump_tx(&self, serial: &str) -> Option<(Vec<i8>, TxFill)> {
        let (handler, len) = {
            let mut st = self.lock();
            let len = st.transfer_len;
            let hw = st.hw.get_mut(serial)?;
            if !hw.streaming || hw.do_exit || hw.mode != TransceiverMode::Tx {
                return None;
            }
            hw.tx_transfers += 1;
            (hw.tx_handler.clone()?, len)
        };
        let mut buf = vec![0i8; len];
        let fill = handler.on_tx(&mut buf);
        if !fill.keep_streaming || fill.valid_len == 0 {
            let flush_now = {
                let mut st = self.lock();
                let auto = st.auto_flush && st.tx_flush_supported;
                let flush_supported = st.tx_flush_supported;
                let hw = st.hw.get_mut(serial)?;
                hw.streaming = false;
                hw.flush_pending = flush_supported;
                auto
            };
            if flush_now {
                self.complete_flush(serial, true);
            }
        }
        Some((buf, fill))
    }

    /// Fire the pending flush callback.
    pub fn complete_flush(&self, serial: &str, success: bool) {
        let handler = {
            let mut st = self.lock();
            let hw = match st.hw.get_mut(serial) {
                Some(hw) => hw,
                None => return,
            };
            if !hw.flush_pending {
                return;
            }
            hw.flush_pending = false;
            hw.tx_handler.clone()
        };
        if let Some(h) = handler {
            h.on_flush(success);
        }
    }

    /// Simulate a USB failure that ends streaming (`streaming = false`).
    pub fn fail_streaming(&self, serial: &str) {
        if let Some(hw) = self.lock().hw.get_mut(serial) {
            hw.streaming = false;
        }
    }

    fn find_device(st: &MockState, serial: &str) -> Option<MockDeviceInfo> {
        st.devices
            .iter()
            .find(|d| d.serial() == serial || d.serial().ends_with(serial))
            .cloned()
    }

    fn open_device(
        self: &Arc<Self>,
        st: &mut MockState,
        info: MockDeviceInfo,
    ) -> Result<MockDevice, HackrfError> {
        let serial = info.serial();
        if st.open.contains(&serial) {
            return Err(HackrfError::Busy);
        }
        st.open.insert(serial.clone());
        // Re-opening resets the streaming machinery (do_exit) but, like real
        // hardware, keeps the last register values.
        let hw = st.hw.entry(serial.clone()).or_default();
        hw.transfers_setup = false;
        hw.streaming = false;
        hw.do_exit = false;
        hw.flush_pending = false;
        hw.rx_handler = None;
        hw.tx_handler = None;
        hw.mode = TransceiverMode::Off;
        Ok(MockDevice {
            backend: Arc::clone(self),
            info,
            serial,
        })
    }

    fn with_hw<T>(
        &self,
        serial: &str,
        call: Call,
        f: impl FnOnce(&mut HwState, &mut MockState) -> Result<T, HackrfError>,
    ) -> Result<T, HackrfError> {
        let mut st = self.lock();
        MockBackend::record(&mut st, call)?;
        let mut hw = st.hw.remove(serial).unwrap_or_default();
        let r = f(&mut hw, &mut st);
        st.hw.insert(serial.to_string(), hw);
        r
    }
}

impl Backend for MockBackend {
    type Device = MockDevice;

    fn init(&self) -> Result<(), HackrfError> {
        let mut st = self.lock();
        MockBackend::record(&mut st, Call::Init)?;
        st.sessions += 1;
        Ok(())
    }

    fn exit(&self) -> Result<(), HackrfError> {
        let mut st = self.lock();
        MockBackend::record(&mut st, Call::Exit)?;
        if st.sessions == 0 {
            return Err(HackrfError::Other);
        }
        st.sessions -= 1;
        if !st.open.is_empty() {
            return Err(HackrfError::NotLastDevice);
        }
        Ok(())
    }

    fn list_devices(&self) -> Result<Vec<ListedDevice>, HackrfError> {
        let mut st = self.lock();
        MockBackend::record(&mut st, Call::DeviceList)?;
        Ok(st
            .devices
            .iter()
            .enumerate()
            .map(|(index, d)| ListedDevice {
                index,
                usb_serial: d.has_usb_serial.then(|| d.serial()),
                usb_board_id: 0x6089,
            })
            .collect())
    }

    fn open_listed(self: &Arc<Self>, index: usize) -> Result<MockDevice, HackrfError> {
        let mut st = self.lock();
        MockBackend::record(&mut st, Call::OpenListed(index))?;
        let info = st
            .devices
            .get(index)
            .cloned()
            .ok_or(HackrfError::InvalidParam)?;
        self.open_device(&mut st, info)
    }

    fn open_by_serial(self: &Arc<Self>, serial: &str) -> Result<MockDevice, HackrfError> {
        let mut st = self.lock();
        MockBackend::record(&mut st, Call::OpenBySerial(serial.to_string()))?;
        let info = MockBackend::find_device(&st, serial).ok_or(HackrfError::NotFound)?;
        self.open_device(&mut st, info)
    }
}

/// Handle to a simulated device.
pub struct MockDevice {
    backend: Arc<MockBackend>,
    info: MockDeviceInfo,
    serial: String,
}

impl MockDevice {
    /// The device's serial string.
    pub fn serial(&self) -> &str {
        &self.serial
    }
}

impl Drop for MockDevice {
    fn drop(&mut self) {
        let mut st = self.backend.lock();
        st.calls.push(Call::Close(self.serial.clone()));
        st.open.remove(&self.serial);
        if let Some(hw) = st.hw.get_mut(&self.serial) {
            hw.streaming = false;
            hw.transfers_setup = false;
            hw.mode = TransceiverMode::Off;
            hw.rx_handler = None;
            hw.tx_handler = None;
            hw.flush_pending = false;
            // libhackrf's hackrf_close also switches the bias tee off.
            hw.antenna = false;
        }
    }
}

impl DeviceHandle for MockDevice {
    fn board_id(&self) -> Result<u8, HackrfError> {
        let info = self.info.clone();
        self.backend
            .with_hw(&self.serial, Call::BoardIdRead, |_, _| Ok(info.board_id))
    }

    fn version_string(&self) -> Result<String, HackrfError> {
        let info = self.info.clone();
        self.backend
            .with_hw(&self.serial, Call::VersionStringRead, |_, _| {
                Ok(info.version)
            })
    }

    fn board_partid_serialno(&self) -> Result<([u32; 2], [u32; 4]), HackrfError> {
        let info = self.info.clone();
        self.backend
            .with_hw(&self.serial, Call::PartIdSerialRead, |_, _| {
                Ok((info.part_id, info.serial_no))
            })
    }

    fn si5351c_read(&self, register: u16) -> Result<u16, HackrfError> {
        let info = self.info.clone();
        self.backend
            .with_hw(&self.serial, Call::Si5351cRead(register), |_, _| {
                Ok(if register == 0 { info.si5351c_reg0 } else { 0 })
            })
    }

    fn set_antenna_enable(&self, enable: bool) -> Result<(), HackrfError> {
        self.backend
            .with_hw(&self.serial, Call::SetAntennaEnable(enable), |hw, _| {
                hw.antenna = enable;
                Ok(())
            })
    }

    fn set_lna_gain(&self, db: u32) -> Result<(), HackrfError> {
        self.backend
            .with_hw(&self.serial, Call::SetLnaGain(db), |hw, _| {
                if db > RX_LNA_MAX_DB {
                    return Err(HackrfError::InvalidParam);
                }
                hw.lna = db & !0x07;
                Ok(())
            })
    }

    fn set_vga_gain(&self, db: u32) -> Result<(), HackrfError> {
        self.backend
            .with_hw(&self.serial, Call::SetVgaGain(db), |hw, _| {
                if db > RX_VGA_MAX_DB {
                    return Err(HackrfError::InvalidParam);
                }
                hw.vga = db & !0x01;
                Ok(())
            })
    }

    fn set_txvga_gain(&self, db: u32) -> Result<(), HackrfError> {
        self.backend
            .with_hw(&self.serial, Call::SetTxVgaGain(db), |hw, _| {
                if db > TX_VGA_MAX_DB {
                    return Err(HackrfError::InvalidParam);
                }
                hw.txvga = db;
                Ok(())
            })
    }

    fn set_amp_enable(&self, enable: bool) -> Result<(), HackrfError> {
        self.backend
            .with_hw(&self.serial, Call::SetAmpEnable(enable), |hw, _| {
                hw.amp = enable;
                Ok(())
            })
    }

    fn set_freq(&self, hz: u64) -> Result<(), HackrfError> {
        self.backend
            .with_hw(&self.serial, Call::SetFreq(hz), |hw, _| {
                if hz > 7_250_000_000 {
                    return Err(HackrfError::InvalidParam);
                }
                hw.freq = hz;
                Ok(())
            })
    }

    fn set_sample_rate(&self, rate: f64) -> Result<(), HackrfError> {
        self.backend
            .with_hw(&self.serial, Call::SetSampleRate(rate), |hw, _| {
                if rate.is_nan() || rate <= 0.0 || rate > 4.0e9 {
                    return Err(HackrfError::InvalidParam);
                }
                hw.sample_rate = rate;
                hw.bandwidth = auto_baseband_filter_bw(rate);
                Ok(())
            })
    }

    fn set_baseband_filter_bandwidth(&self, hz: u32) -> Result<(), HackrfError> {
        self.backend.with_hw(
            &self.serial,
            Call::SetBasebandFilterBandwidth(hz),
            |hw, _| {
                hw.bandwidth = hz;
                Ok(())
            },
        )
    }

    fn start_rx(&self, handler: Arc<dyn RxHandler>) -> Result<(), HackrfError> {
        self.backend.with_hw(&self.serial, Call::StartRx, |hw, st| {
            if hw.transfers_setup && !hw.do_exit && hw.streaming {
                return Err(HackrfError::Busy);
            }
            hw.mode = TransceiverMode::Rx;
            hw.rx_handler = Some(handler);
            hw.transfers_setup = true;
            hw.streaming = true;
            let _ = st;
            Ok(())
        })
    }

    fn stop_rx(&self) -> Result<(), HackrfError> {
        self.backend.with_hw(&self.serial, Call::StopRx, |hw, st| {
            hw.streaming = false;
            hw.mode = TransceiverMode::Off;
            hw.rx_handler = None;
            if st.legacy_restart_bug {
                hw.do_exit = true;
            }
            Ok(())
        })
    }

    fn start_tx(&self, handler: Arc<dyn TxHandler>) -> Result<(), HackrfError> {
        self.backend.with_hw(&self.serial, Call::StartTx, |hw, st| {
            if hw.transfers_setup && !hw.do_exit && hw.streaming {
                return Err(HackrfError::Busy);
            }
            hw.mode = TransceiverMode::Tx;
            hw.tx_handler = Some(handler);
            hw.transfers_setup = true;
            hw.streaming = true;
            hw.flush_pending = false;
            let _ = st;
            Ok(())
        })
    }

    fn stop_tx(&self) -> Result<(), HackrfError> {
        self.backend.with_hw(&self.serial, Call::StopTx, |hw, st| {
            hw.streaming = false;
            hw.mode = TransceiverMode::Off;
            hw.tx_handler = None;
            hw.flush_pending = false;
            // Returning to idle switches the bias tee off (firmware
            // behaviour documented in hackrf.h).
            hw.antenna = false;
            if st.legacy_restart_bug {
                hw.do_exit = true;
            }
            Ok(())
        })
    }

    fn is_streaming(&self) -> StreamingStatus {
        self.backend
            .with_hw(&self.serial, Call::IsStreaming, |hw, _| {
                Ok(if hw.transfers_setup && hw.streaming && !hw.do_exit {
                    StreamingStatus::Streaming
                } else if !hw.transfers_setup {
                    StreamingStatus::ThreadError
                } else if !hw.streaming {
                    StreamingStatus::Stopped
                } else {
                    StreamingStatus::ExitCalled
                })
            })
            .unwrap_or(StreamingStatus::ThreadError)
    }

    fn supports_tx_flush(&self) -> bool {
        self.backend.lock().tx_flush_supported
    }
}
