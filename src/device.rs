//! The device object: identification, settings, antenna, gain, frequency,
//! sample rate and bandwidth control (the C++ `HackRF_Settings.cpp`).
//!
//! All state lives behind one mutex (the C++ `_device_mutex`); streaming
//! buffers live in [`crate::stream::Shared`] behind a second one, and the
//! lock order is always device → buffers.
//!
//! # Half-duplex bookkeeping
//!
//! The HackRF has one RF chain shared by RX and TX. Settings are stored per
//! direction and applied to the hardware when they belong to the direction
//! that is currently active (or when the radio is idle). Whenever a stream is
//! activated, `DeviceState::resync` compares the hardware shadow (`current_*`)
//! with that direction's settings and applies the differences.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::backend::{Backend, DeviceHandle, Session};
use crate::convert::NATIVE_FULL_SCALE;
use crate::enumerate::{claim_serial, find_hackrf, release_serial, ARG_SERIAL};
use crate::error::{Error, HackrfError, Result};
use crate::gain::{
    distribute_rx_gain, distribute_tx_gain, quantize_amp, quantize_lna, quantize_rx_vga,
    quantize_tx_vga, RX_GAIN_MAX_DB, TX_GAIN_MAX_DB,
};
use crate::stream::Shared;
use crate::types::*;

/// Per-direction settings and stream bookkeeping.
#[derive(Clone, Debug, PartialEq)]
pub struct DirectionSettings {
    /// LNA gain (RX only).
    pub lna_gain: u32,
    /// VGA gain (RX baseband or TX IF).
    pub vga_gain: u32,
    /// Amplifier gain (0 or 14).
    pub amp_gain: u32,
    /// Requested sample rate, 0 when never set.
    pub samplerate: f64,
    /// Requested baseband filter bandwidth, 0 means automatic.
    pub bandwidth: u32,
    /// Requested RF frequency in Hz (before ppm correction), 0 when never set.
    pub frequency: u64,
    /// Bias tee requested (TX settings only; shared by both directions).
    pub bias: bool,
    /// A stream in this direction is open.
    pub opened: bool,
    /// Format of the open stream.
    pub format: StreamFormat,
    /// Ring depth of the open stream.
    pub buf_num: usize,
}

impl DirectionSettings {
    fn new(lna_gain: u32, vga_gain: u32) -> DirectionSettings {
        DirectionSettings {
            lna_gain,
            vga_gain,
            amp_gain: 0,
            samplerate: 0.0,
            bandwidth: 0,
            frequency: 0,
            bias: false,
            opened: false,
            format: StreamFormat::CS8,
            buf_num: BUF_NUM,
        }
    }
}

/// Everything protected by the device mutex.
pub struct DeviceState<B: Backend> {
    /// The libhackrf handle; `None` only if a re-open failed.
    pub handle: Option<B::Device>,
    /// RX settings.
    pub rx: DirectionSettings,
    /// TX settings.
    pub tx: DirectionSettings,
    /// Frequency the hardware was last tuned to (uncorrected request).
    pub current_frequency: u64,
    /// Frequency correction in parts per million, shared by RX and TX.
    pub frequency_correction_ppm: f64,
    /// Sample rate last written to the hardware.
    pub current_samplerate: f64,
    /// Manual baseband filter last written, 0 if the automatic one is active.
    pub current_bandwidth: u32,
    /// Amplifier state last written (0 or 14).
    pub current_amp: u32,
    /// Whether the hardware is idle, receiving or transmitting.
    pub mode: TransceiverMode,
}

impl<B: Backend> DeviceState<B> {
    /// The open handle or [`Error::DeviceClosed`].
    pub fn handle(&self) -> Result<&B::Device> {
        self.handle.as_ref().ok_or(Error::DeviceClosed)
    }

    /// Settings of one direction.
    pub fn dir(&self, direction: Direction) -> &DirectionSettings {
        match direction {
            Direction::Rx => &self.rx,
            Direction::Tx => &self.tx,
        }
    }

    /// Mutable settings of one direction.
    pub fn dir_mut(&mut self, direction: Direction) -> &mut DirectionSettings {
        match direction {
            Direction::Rx => &mut self.rx,
            Direction::Tx => &mut self.tx,
        }
    }

    /// Whether a setting for `direction` must be written to the hardware now
    /// (the radio is idle or already working in that direction) or only
    /// remembered until the next activation.
    pub fn applies_now(&self, direction: Direction) -> bool {
        matches!(
            (self.mode, direction),
            (TransceiverMode::Off, _)
                | (TransceiverMode::Rx, Direction::Rx)
                | (TransceiverMode::Tx, Direction::Tx)
        )
    }

    /// Tune to `frequency` with the ppm correction applied, using the same
    /// convention as gr-osmosdr: `real = freq * (1 + ppm * 1e-6)`.
    pub fn tune(&mut self, frequency: u64) -> Result<()> {
        let corrected = corrected_frequency(frequency, self.frequency_correction_ppm);
        self.handle()?
            .set_freq(corrected)
            .map_err(|e| Error::hackrf("hackrf_set_freq", e))?;
        self.current_frequency = frequency;
        Ok(())
    }

    /// Write the amplifier state.
    pub fn apply_amp(&mut self, amp_gain: u32) -> Result<()> {
        self.handle()?
            .set_amp_enable(amp_gain > 0)
            .map_err(|e| Error::hackrf("hackrf_set_amp_enable", e))?;
        self.current_amp = amp_gain;
        Ok(())
    }

    /// Write the sample rate; libhackrf resets the baseband filter to its
    /// automatic value as a side effect, so a manual filter is re-applied.
    pub fn apply_samplerate(&mut self, rate: f64, manual_bandwidth: u32) -> Result<()> {
        self.handle()?
            .set_sample_rate(rate)
            .map_err(|e| Error::hackrf("hackrf_set_sample_rate", e))?;
        self.current_samplerate = rate;
        self.current_bandwidth = 0;
        if manual_bandwidth > 0 {
            self.apply_bandwidth(manual_bandwidth)?;
        }
        Ok(())
    }

    /// Write a baseband filter bandwidth; 0 restores the automatic filter for
    /// the current sample rate.
    pub fn apply_bandwidth(&mut self, bandwidth: u32) -> Result<()> {
        let hw = if bandwidth > 0 {
            bandwidth
        } else if self.current_samplerate > 0.0 {
            auto_baseband_filter_bw(self.current_samplerate)
        } else {
            self.current_bandwidth = 0;
            return Ok(());
        };
        self.handle()?
            .set_baseband_filter_bandwidth(hw)
            .map_err(|e| Error::hackrf("hackrf_set_baseband_filter_bandwidth", e))?;
        self.current_bandwidth = bandwidth;
        Ok(())
    }

    /// Bring the hardware shadow in line with `direction`'s settings.
    pub fn resync(&mut self, direction: Direction) -> Result<()> {
        let s = self.dir(direction).clone();
        if s.samplerate > 0.0 && self.current_samplerate != s.samplerate {
            self.apply_samplerate(s.samplerate, s.bandwidth)?;
        }
        if s.frequency != 0 && self.current_frequency != s.frequency {
            self.tune(s.frequency)?;
        }
        if self.current_amp != s.amp_gain {
            self.apply_amp(s.amp_gain)?;
        }
        if self.current_bandwidth != s.bandwidth {
            self.apply_bandwidth(s.bandwidth)?;
        }
        if self.tx.bias {
            // The firmware drops the bias tee whenever the radio goes idle.
            self.handle()?
                .set_antenna_enable(true)
                .map_err(|e| Error::hackrf("hackrf_set_antenna_enable", e))?;
        }
        Ok(())
    }

    /// Re-apply every setting of `direction` after a fresh open.
    pub fn reapply_all(&mut self, direction: Direction) -> Result<()> {
        let s = self.dir(direction).clone();
        self.current_frequency = 0;
        self.current_samplerate = 0.0;
        self.current_bandwidth = 0;
        self.current_amp = 0;
        if s.frequency != 0 {
            self.tune(s.frequency)?;
        }
        if s.samplerate > 0.0 {
            self.apply_samplerate(s.samplerate, s.bandwidth)?;
        } else if s.bandwidth > 0 {
            self.apply_bandwidth(s.bandwidth)?;
        }
        self.apply_amp(s.amp_gain)?;
        let handle = self.handle()?;
        match direction {
            Direction::Rx => {
                handle
                    .set_lna_gain(s.lna_gain)
                    .map_err(|e| Error::hackrf("hackrf_set_lna_gain", e))?;
                handle
                    .set_vga_gain(s.vga_gain)
                    .map_err(|e| Error::hackrf("hackrf_set_vga_gain", e))?;
            }
            Direction::Tx => {
                handle
                    .set_txvga_gain(s.vga_gain)
                    .map_err(|e| Error::hackrf("hackrf_set_txvga_gain", e))?;
            }
        }
        if self.tx.bias {
            handle
                .set_antenna_enable(true)
                .map_err(|e| Error::hackrf("hackrf_set_antenna_enable", e))?;
        }
        Ok(())
    }
}

/// Apply a ppm correction to a frequency.
pub fn corrected_frequency(frequency: u64, ppm: f64) -> u64 {
    (frequency as f64 * (1.0 + ppm * 1e-6)).max(0.0) as u64
}

/// A HackRF device (the C++ `SoapyHackRF` class).
pub struct HackRf<B: Backend> {
    pub(crate) dev: Mutex<DeviceState<B>>,
    pub(crate) shared: Arc<Shared>,
    pub(crate) backend: Arc<B>,
    serial: String,
    // Declared last so that the device handle is closed before hackrf_exit.
    _session: Session<B>,
}

impl<B: Backend> HackRf<B> {
    /// Open a device. `args` may carry `serial` (full or suffix) and/or
    /// `hackrf` (enumeration index); without `serial` the first device
    /// matching the arguments is used.
    pub fn open(backend: Arc<B>, args: &Kwargs) -> Result<HackRf<B>> {
        let session =
            Session::new(Arc::clone(&backend)).map_err(|e| Error::hackrf("hackrf_init", e))?;
        let serial = match args.get(ARG_SERIAL) {
            Some(serial) => serial.clone(),
            None => find_hackrf(&backend, args)?
                .into_iter()
                .next()
                .and_then(|k| k.get(ARG_SERIAL).cloned())
                .ok_or(Error::NoDeviceMatches)?,
        };
        let handle = B::open_by_serial(&backend, &serial).map_err(Error::OpenFailed)?;
        // libhackrf matches a serial *suffix*; record the full serial so the
        // claim and the discovery cache agree.
        let serial = match handle.board_partid_serialno() {
            Ok((_, serial_no)) => format_serial(serial_no),
            Err(_) => serial,
        };
        claim_serial(&serial);
        Ok(HackRf {
            dev: Mutex::new(DeviceState {
                handle: Some(handle),
                rx: DirectionSettings::new(16, 16),
                tx: DirectionSettings::new(0, 0),
                current_frequency: 0,
                frequency_correction_ppm: 0.0,
                current_samplerate: 0.0,
                current_bandwidth: 0,
                current_amp: 0,
                mode: TransceiverMode::Off,
            }),
            shared: Arc::new(Shared::new()),
            backend,
            serial,
            _session: session,
        })
    }

    /// The device's full 32 character serial.
    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// The backend.
    pub fn backend(&self) -> &Arc<B> {
        &self.backend
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, DeviceState<B>> {
        self.dev.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Current transceiver mode.
    pub fn transceiver_mode(&self) -> TransceiverMode {
        self.lock().mode
    }

    /// Snapshot of the settings of one direction.
    pub fn direction_settings(&self, direction: Direction) -> DirectionSettings {
        self.lock().dir(direction).clone()
    }

    /// Re-open the device by serial (used when old libhackrf versions refuse
    /// to restart streaming on a handle that was stopped).
    pub(crate) fn reopen(&self, st: &mut DeviceState<B>) -> Result<()> {
        st.handle = None;
        st.handle =
            Some(B::open_by_serial(&self.backend, &self.serial).map_err(Error::OpenFailed)?);
        Ok(())
    }

    fn check_channel(channel: usize) -> Result<()> {
        if channel == 0 {
            Ok(())
        } else {
            Err(Error::InvalidChannel(channel))
        }
    }

    /*******************************************************************
     * Identification API
     ******************************************************************/

    /// `"HackRF"`.
    pub fn driver_key(&self) -> String {
        DRIVER_KEY.to_string()
    }

    /// The board name as reported by `hackrf_board_id_name`.
    pub fn hardware_key(&self) -> Result<String> {
        let st = self.lock();
        let id = st
            .handle()?
            .board_id()
            .map_err(|e| Error::hackrf("hackrf_board_id_read", e))?;
        Ok(BoardId::from_u8(id).name().to_string())
    }

    /// Firmware version, part id, serial and clock source.
    pub fn hardware_info(&self) -> Result<Kwargs> {
        let st = self.lock();
        let h = st.handle()?;
        let version = h
            .version_string()
            .map_err(|e| Error::hackrf("hackrf_version_string_read", e))?;
        let (part_id, serial_no) = h
            .board_partid_serialno()
            .map_err(|e| Error::hackrf("hackrf_board_partid_serialno_read", e))?;
        let clock = h
            .si5351c_read(0)
            .map_err(|e| Error::hackrf("hackrf_si5351c_read", e))?;
        let mut info = Kwargs::new();
        info.insert("version".into(), version);
        // Key kept with a space for compatibility with the C++ driver
        // (discovery uses "part_id"; see BUGS.md E3).
        info.insert("part id".into(), format_part_id(part_id));
        info.insert("serial".into(), format_serial(serial_no));
        info.insert(
            "clock source".into(),
            if clock == 0x51 {
                "internal"
            } else {
                "external"
            }
            .into(),
        );
        Ok(info)
    }

    /*******************************************************************
     * Channels API
     ******************************************************************/

    /// Always 1.
    pub fn num_channels(&self, _direction: Direction) -> usize {
        1
    }

    /// Always false: the HackRF is half duplex.
    pub fn full_duplex(&self, _direction: Direction, channel: usize) -> Result<bool> {
        Self::check_channel(channel)?;
        Ok(false)
    }

    /*******************************************************************
     * Stream info API
     ******************************************************************/

    /// CS8, CS16, CF32 and CF64.
    pub fn stream_formats(
        &self,
        _direction: Direction,
        channel: usize,
    ) -> Result<Vec<StreamFormat>> {
        Self::check_channel(channel)?;
        Ok(StreamFormat::ALL.to_vec())
    }

    /// CS8 with a full scale of 128.
    pub fn native_stream_format(
        &self,
        _direction: Direction,
        channel: usize,
    ) -> Result<(StreamFormat, f64)> {
        Self::check_channel(channel)?;
        Ok((StreamFormat::CS8, NATIVE_FULL_SCALE))
    }

    /// The `buffers` stream argument.
    pub fn stream_args_info(&self, _direction: Direction, channel: usize) -> Result<Vec<ArgInfo>> {
        Self::check_channel(channel)?;
        Ok(vec![ArgInfo {
            key: BUFFERS_STREAM_ARG.into(),
            value: BUF_NUM.to_string(),
            name: "Buffer Count".into(),
            description: "Number of transfer buffers in the stream ring.".into(),
            units: "buffers".into(),
            arg_type: ArgType::Int,
            range: None,
            options: Vec::new(),
        }])
    }

    /*******************************************************************
     * Settings API
     ******************************************************************/

    /// The `bias_tx` setting.
    pub fn setting_info(&self) -> Vec<ArgInfo> {
        vec![ArgInfo {
            key: BIAS_TX_SETTING.into(),
            value: "false".into(),
            name: "Antenna Bias".into(),
            description: "Antenna port power control.".into(),
            units: String::new(),
            arg_type: ArgType::Bool,
            range: None,
            options: Vec::new(),
        }]
    }

    /// Write a setting. Only `bias_tx` (`"true"`/`"false"`) exists.
    pub fn write_setting(&self, key: &str, value: &str) -> Result<()> {
        if key != BIAS_TX_SETTING {
            return Err(Error::UnknownSetting(key.to_string()));
        }
        let enable = match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => true,
            "false" | "0" => false,
            other => {
                return Err(Error::InvalidArgument(format!(
                    "bias_tx must be true or false, got {other:?}"
                )))
            }
        };
        let mut st = self.lock();
        st.tx.bias = enable;
        st.handle()?
            .set_antenna_enable(enable)
            .map_err(|e| Error::hackrf("hackrf_set_antenna_enable", e))
    }

    /// Read a setting.
    pub fn read_setting(&self, key: &str) -> Result<String> {
        if key != BIAS_TX_SETTING {
            return Err(Error::UnknownSetting(key.to_string()));
        }
        Ok(if self.lock().tx.bias { "true" } else { "false" }.to_string())
    }

    /*******************************************************************
     * Antenna API
     ******************************************************************/

    /// `["TX/RX"]`.
    pub fn list_antennas(&self, _direction: Direction, channel: usize) -> Result<Vec<String>> {
        Self::check_channel(channel)?;
        Ok(vec![ANTENNA_NAME.to_string()])
    }

    /// Only `"TX/RX"` is accepted.
    pub fn set_antenna(&self, _direction: Direction, channel: usize, name: &str) -> Result<()> {
        Self::check_channel(channel)?;
        if name == ANTENNA_NAME {
            Ok(())
        } else {
            Err(Error::UnknownAntenna(name.to_string()))
        }
    }

    /// `"TX/RX"`.
    pub fn antenna(&self, _direction: Direction, channel: usize) -> Result<String> {
        Self::check_channel(channel)?;
        Ok(ANTENNA_NAME.to_string())
    }

    /*******************************************************************
     * Frontend corrections API
     ******************************************************************/

    /// Always false.
    pub fn has_dc_offset_mode(&self, _direction: Direction, channel: usize) -> Result<bool> {
        Self::check_channel(channel)?;
        Ok(false)
    }

    /// Always true.
    pub fn has_frequency_correction(&self, _direction: Direction, channel: usize) -> Result<bool> {
        Self::check_channel(channel)?;
        Ok(true)
    }

    /// Set the ppm correction (shared by both directions) and retune so it
    /// takes effect immediately.
    pub fn set_frequency_correction(
        &self,
        _direction: Direction,
        channel: usize,
        ppm: f64,
    ) -> Result<()> {
        Self::check_channel(channel)?;
        if !ppm.is_finite() {
            return Err(Error::InvalidArgument(format!("ppm {ppm} is not finite")));
        }
        let mut st = self.lock();
        st.frequency_correction_ppm = ppm;
        if st.current_frequency != 0 {
            let f = st.current_frequency;
            st.tune(f)?;
        }
        Ok(())
    }

    /// The ppm correction.
    pub fn frequency_correction(&self, _direction: Direction, channel: usize) -> Result<f64> {
        Self::check_channel(channel)?;
        Ok(self.lock().frequency_correction_ppm)
    }

    /*******************************************************************
     * Gain API
     ******************************************************************/

    /// RX: `LNA`, `AMP`, `VGA` (the order gr-osmosdr expects); TX: `VGA`, `AMP`.
    pub fn list_gains(&self, direction: Direction, channel: usize) -> Result<Vec<String>> {
        Self::check_channel(channel)?;
        Ok(match direction {
            Direction::Rx => vec![GAIN_LNA.into(), GAIN_AMP.into(), GAIN_VGA.into()],
            Direction::Tx => vec![GAIN_VGA.into(), GAIN_AMP.into()],
        })
    }

    /// There is no AGC: enabling it is [`Error::NotSupported`].
    pub fn set_gain_mode(
        &self,
        _direction: Direction,
        channel: usize,
        automatic: bool,
    ) -> Result<()> {
        Self::check_channel(channel)?;
        if automatic {
            Err(Error::NotSupported)
        } else {
            Ok(())
        }
    }

    /// Always false.
    pub fn gain_mode(&self, _direction: Direction, channel: usize) -> Result<bool> {
        Self::check_channel(channel)?;
        Ok(false)
    }

    /// Distribute an overall gain across the stages (see [`crate::gain`]).
    pub fn set_gain(&self, direction: Direction, channel: usize, value: f64) -> Result<()> {
        Self::check_channel(channel)?;
        let mut st = self.lock();
        let mut first_err: Option<Error> = None;
        let mut note = |r: Result<()>| {
            if let Err(e) = r {
                first_err.get_or_insert(e);
            }
        };
        match direction {
            Direction::Rx => {
                let split = distribute_rx_gain(value);
                st.rx.lna_gain = split.lna_db;
                st.rx.vga_gain = split.vga_db;
                st.rx.amp_gain = split.amp_db;
                note(
                    st.handle()?
                        .set_lna_gain(split.lna_db)
                        .map_err(|e| Error::hackrf("hackrf_set_lna_gain", e)),
                );
                note(
                    st.handle()?
                        .set_vga_gain(split.vga_db)
                        .map_err(|e| Error::hackrf("hackrf_set_vga_gain", e)),
                );
                if st.applies_now(Direction::Rx) {
                    note(st.apply_amp(split.amp_db));
                }
            }
            Direction::Tx => {
                let split = distribute_tx_gain(value);
                st.tx.vga_gain = split.vga_db;
                st.tx.amp_gain = split.amp_db;
                note(
                    st.handle()?
                        .set_txvga_gain(split.vga_db)
                        .map_err(|e| Error::hackrf("hackrf_set_txvga_gain", e)),
                );
                if st.applies_now(Direction::Tx) {
                    note(st.apply_amp(split.amp_db));
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Set one gain element. Values are quantised to the hardware steps
    /// (LNA: 8 dB, RX VGA: 2 dB, TX VGA: 1 dB, AMP: on/off).
    pub fn set_gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        value: f64,
    ) -> Result<()> {
        Self::check_channel(channel)?;
        let mut st = self.lock();
        match (direction, name) {
            (_, GAIN_AMP) => {
                let amp = quantize_amp(value);
                st.dir_mut(direction).amp_gain = amp;
                if st.applies_now(direction) {
                    st.apply_amp(amp)?;
                }
            }
            (Direction::Rx, GAIN_LNA) => {
                let lna = quantize_lna(value);
                st.rx.lna_gain = lna;
                st.handle()?
                    .set_lna_gain(lna)
                    .map_err(|e| Error::hackrf("hackrf_set_lna_gain", e))?;
            }
            (Direction::Rx, GAIN_VGA) => {
                let vga = quantize_rx_vga(value);
                st.rx.vga_gain = vga;
                st.handle()?
                    .set_vga_gain(vga)
                    .map_err(|e| Error::hackrf("hackrf_set_vga_gain", e))?;
            }
            (Direction::Tx, GAIN_VGA) => {
                let vga = quantize_tx_vga(value);
                st.tx.vga_gain = vga;
                st.handle()?
                    .set_txvga_gain(vga)
                    .map_err(|e| Error::hackrf("hackrf_set_txvga_gain", e))?;
            }
            _ => return Err(Error::UnknownGainName(name.to_string())),
        }
        Ok(())
    }

    /// Read one gain element.
    pub fn gain_element(&self, direction: Direction, channel: usize, name: &str) -> Result<f64> {
        Self::check_channel(channel)?;
        let st = self.lock();
        let s = st.dir(direction);
        Ok(match (direction, name) {
            (_, GAIN_AMP) => s.amp_gain,
            (Direction::Rx, GAIN_LNA) => s.lna_gain,
            (Direction::Rx, GAIN_VGA) | (Direction::Tx, GAIN_VGA) => s.vga_gain,
            _ => return Err(Error::UnknownGainName(name.to_string())),
        } as f64)
    }

    /// Sum of all gain elements of a direction.
    pub fn gain(&self, direction: Direction, channel: usize) -> Result<f64> {
        Self::check_channel(channel)?;
        let st = self.lock();
        let s = st.dir(direction);
        Ok((s.amp_gain
            + s.vga_gain
            + if direction == Direction::Rx {
                s.lna_gain
            } else {
                0
            }) as f64)
    }

    /// Range of one gain element.
    pub fn gain_element_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range> {
        Self::check_channel(channel)?;
        Ok(match (direction, name) {
            (_, GAIN_AMP) => Range::new(0.0, AMP_MAX_DB as f64, AMP_MAX_DB as f64),
            (Direction::Rx, GAIN_LNA) => {
                Range::new(0.0, RX_LNA_MAX_DB as f64, RX_LNA_STEP_DB as f64)
            }
            (Direction::Rx, GAIN_VGA) => {
                Range::new(0.0, RX_VGA_MAX_DB as f64, RX_VGA_STEP_DB as f64)
            }
            (Direction::Tx, GAIN_VGA) => {
                Range::new(0.0, TX_VGA_MAX_DB as f64, TX_VGA_STEP_DB as f64)
            }
            _ => return Err(Error::UnknownGainName(name.to_string())),
        })
    }

    /// Range of the overall gain.
    pub fn gain_range(&self, direction: Direction, channel: usize) -> Result<Range> {
        Self::check_channel(channel)?;
        Ok(match direction {
            Direction::Rx => Range::new(0.0, RX_GAIN_MAX_DB as f64, 1.0),
            Direction::Tx => Range::new(0.0, TX_GAIN_MAX_DB as f64, 1.0),
        })
    }

    /*******************************************************************
     * Frequency API
     ******************************************************************/

    /// Tune the `RF` component (`BB` is accepted and ignored).
    pub fn set_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        frequency: f64,
        _args: &Kwargs,
    ) -> Result<()> {
        Self::check_channel(channel)?;
        match name {
            FREQ_COMPONENT_BB => return Ok(()),
            FREQ_COMPONENT_RF => {}
            other => return Err(Error::UnknownFrequencyName(other.to_string())),
        }
        if !frequency.is_finite() || !(0.0..=MAX_FREQUENCY_HZ).contains(&frequency) {
            return Err(Error::InvalidArgument(format!(
                "frequency {frequency} Hz is outside 0..={MAX_FREQUENCY_HZ} Hz"
            )));
        }
        let hz = frequency as u64;
        let mut st = self.lock();
        st.dir_mut(direction).frequency = hz;
        if st.applies_now(direction) {
            st.tune(hz)?;
        }
        Ok(())
    }

    /// The requested (uncorrected) frequency of a component.
    pub fn frequency(&self, direction: Direction, channel: usize, name: &str) -> Result<f64> {
        Self::check_channel(channel)?;
        match name {
            FREQ_COMPONENT_BB => Ok(0.0),
            FREQ_COMPONENT_RF => Ok(self.lock().dir(direction).frequency as f64),
            other => Err(Error::UnknownFrequencyName(other.to_string())),
        }
    }

    /// No tuning arguments.
    pub fn frequency_args_info(
        &self,
        _direction: Direction,
        channel: usize,
    ) -> Result<Vec<ArgInfo>> {
        Self::check_channel(channel)?;
        Ok(Vec::new())
    }

    /// `["RF"]`.
    pub fn list_frequencies(&self, _direction: Direction, channel: usize) -> Result<Vec<String>> {
        Self::check_channel(channel)?;
        Ok(vec![FREQ_COMPONENT_RF.to_string()])
    }

    /// `RF`: 0–7.25 GHz; `BB`: 0.
    pub fn frequency_range(
        &self,
        _direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Vec<Range>> {
        Self::check_channel(channel)?;
        match name {
            FREQ_COMPONENT_BB => Ok(vec![Range::new(0.0, 0.0, 0.0)]),
            FREQ_COMPONENT_RF => Ok(vec![Range::new(0.0, MAX_FREQUENCY_HZ, 0.0)]),
            other => Err(Error::UnknownFrequencyName(other.to_string())),
        }
    }

    /*******************************************************************
     * Sample rate API
     ******************************************************************/

    /// Set the sample rate. A manual baseband filter is re-applied afterwards
    /// because libhackrf resets it.
    pub fn set_sample_rate(&self, direction: Direction, channel: usize, rate: f64) -> Result<()> {
        Self::check_channel(channel)?;
        if !rate.is_finite() || rate <= 0.0 {
            return Err(Error::InvalidArgument(format!(
                "sample rate {rate} must be positive"
            )));
        }
        let mut st = self.lock();
        let previous = st.dir(direction).samplerate;
        st.dir_mut(direction).samplerate = rate;
        if st.applies_now(direction) {
            let bw = st.dir(direction).bandwidth;
            if let Err(e) = st.apply_samplerate(rate, bw) {
                st.dir_mut(direction).samplerate = previous;
                return Err(e);
            }
        }
        Ok(())
    }

    /// The requested sample rate (0 if never set).
    pub fn sample_rate(&self, direction: Direction, channel: usize) -> Result<f64> {
        Self::check_channel(channel)?;
        Ok(self.lock().dir(direction).samplerate)
    }

    /// 1 MHz to 20 MHz in 1 MHz steps.
    pub fn list_sample_rates(&self, _direction: Direction, channel: usize) -> Result<Vec<f64>> {
        Self::check_channel(channel)?;
        Ok((1..=20).map(|m| m as f64 * 1e6).collect())
    }

    /// 1 MHz to 20 MHz, continuous.
    pub fn sample_rate_range(&self, _direction: Direction, channel: usize) -> Result<Range> {
        Self::check_channel(channel)?;
        Ok(Range::new(MIN_SAMPLE_RATE, MAX_SAMPLE_RATE, 0.0))
    }

    /*******************************************************************
     * Bandwidth API
     ******************************************************************/

    /// Set the baseband filter bandwidth; 0 selects the automatic filter
    /// (75 % of the sample rate).
    pub fn set_bandwidth(&self, direction: Direction, channel: usize, bw: f64) -> Result<()> {
        Self::check_channel(channel)?;
        if !bw.is_finite() || bw < 0.0 || bw > u32::MAX as f64 {
            return Err(Error::InvalidArgument(format!("bandwidth {bw} is invalid")));
        }
        let hz = bw as u32;
        let mut st = self.lock();
        let previous = st.dir(direction).bandwidth;
        st.dir_mut(direction).bandwidth = hz;
        if st.applies_now(direction) {
            if let Err(e) = st.apply_bandwidth(hz) {
                st.dir_mut(direction).bandwidth = previous;
                return Err(e);
            }
        }
        Ok(())
    }

    /// The effective bandwidth: the manual value, or the automatic filter for
    /// the direction's sample rate (0 if neither was set).
    pub fn bandwidth(&self, direction: Direction, channel: usize) -> Result<f64> {
        Self::check_channel(channel)?;
        let st = self.lock();
        let s = st.dir(direction);
        Ok(if s.bandwidth > 0 {
            s.bandwidth as f64
        } else if s.samplerate > 0.0 {
            auto_baseband_filter_bw(s.samplerate) as f64
        } else {
            0.0
        })
    }

    /// The MAX2837 filter table.
    pub fn list_bandwidths(&self, _direction: Direction, channel: usize) -> Result<Vec<f64>> {
        Self::check_channel(channel)?;
        Ok(BASEBAND_FILTER_BANDWIDTHS_HZ
            .iter()
            .map(|&b| b as f64)
            .collect())
    }

    /// Lowest to highest filter.
    pub fn bandwidth_range(&self, _direction: Direction, channel: usize) -> Result<Range> {
        Self::check_channel(channel)?;
        Ok(Range::new(
            BASEBAND_FILTER_BANDWIDTHS_HZ[0] as f64,
            BASEBAND_FILTER_BANDWIDTHS_HZ[BASEBAND_FILTER_BANDWIDTHS_HZ.len() - 1] as f64,
            0.0,
        ))
    }
}

impl<B: Backend> Drop for HackRf<B> {
    fn drop(&mut self) {
        release_serial(&self.serial);
        // Stop streaming before the handle closes, so the callbacks never
        // observe a torn-down ring.
        if let Ok(mut st) = self.dev.lock() {
            if let Some(h) = st.handle.as_ref() {
                match st.mode {
                    TransceiverMode::Rx => {
                        let _ = h.stop_rx();
                    }
                    TransceiverMode::Tx => {
                        let _ = h.stop_tx();
                    }
                    TransceiverMode::Off => {}
                }
            }
            st.mode = TransceiverMode::Off;
        }
    }
}

impl From<HackrfError> for Box<Error> {
    fn from(e: HackrfError) -> Box<Error> {
        Box::new(Error::from(e))
    }
}
