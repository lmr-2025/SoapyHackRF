//! Plain data types shared by the whole crate: constants, directions, stream
//! formats, ranges, argument descriptions and stream flags.
//!
//! The names mirror the SoapySDR vocabulary used by the original C++ driver so
//! that behaviour can be compared one-to-one, while the shapes follow the
//! conventions of Rust SDR frameworks such as `seify`/FutureSDR (`Direction`,
//! `Range { minimum, maximum, step }`, string-keyed `Args`).

use std::collections::BTreeMap;
use std::fmt;

/// Size in bytes of one transfer buffer. Matches libhackrf's
/// `TRANSFER_BUFFER_SIZE` so every USB transfer fills exactly one slot.
pub const BUF_LEN: usize = 262_144;
/// Default number of ring-buffer slots per stream (`buffers` stream argument).
pub const BUF_NUM: usize = 15;
/// Bytes per complex sample in the native CS8 format (I and Q, one byte each).
pub const BYTES_PER_SAMPLE: usize = 2;
/// Maximum transmission unit of a stream in complex samples.
pub const MTU_SAMPLES: usize = BUF_LEN / BYTES_PER_SAMPLE;

/// Maximum RX baseband (VGA) gain in dB.
pub const RX_VGA_MAX_DB: u32 = 62;
/// RX VGA gain step in dB.
pub const RX_VGA_STEP_DB: u32 = 2;
/// Maximum TX IF (VGA) gain in dB.
pub const TX_VGA_MAX_DB: u32 = 47;
/// TX VGA gain step in dB.
pub const TX_VGA_STEP_DB: u32 = 1;
/// Maximum RX IF (LNA) gain in dB.
pub const RX_LNA_MAX_DB: u32 = 40;
/// RX LNA gain step in dB.
pub const RX_LNA_STEP_DB: u32 = 8;
/// Nominal gain of the RF amplifier in dB (it is either on or off).
pub const AMP_MAX_DB: u32 = 14;

/// Upper edge of the advertised tuning range in Hz.
pub const MAX_FREQUENCY_HZ: f64 = 7_250_000_000.0;
/// Lowest advertised sample rate in samples per second.
pub const MIN_SAMPLE_RATE: f64 = 1e6;
/// Highest advertised sample rate in samples per second.
pub const MAX_SAMPLE_RATE: f64 = 20e6;

/// Value returned by `driver_key()`.
pub const DRIVER_KEY: &str = "HackRF";
/// The single antenna port name.
pub const ANTENNA_NAME: &str = "TX/RX";
/// Key of the bias-tee setting.
pub const BIAS_TX_SETTING: &str = "bias_tx";
/// Key of the stream argument selecting the ring-buffer depth.
pub const BUFFERS_STREAM_ARG: &str = "buffers";
/// Name of the RF frequency component.
pub const FREQ_COMPONENT_RF: &str = "RF";
/// Name of the (unsupported, always 0 Hz) baseband frequency component.
pub const FREQ_COMPONENT_BB: &str = "BB";
/// RX RF amplifier gain element.
pub const GAIN_AMP: &str = "AMP";
/// RX IF (LNA) gain element.
pub const GAIN_LNA: &str = "LNA";
/// RX baseband / TX IF (VGA) gain element.
pub const GAIN_VGA: &str = "VGA";

/// Baseband filter bandwidths supported by the MAX2837, in Hz (ascending).
pub const BASEBAND_FILTER_BANDWIDTHS_HZ: [u32; 16] = [
    1_750_000, 2_500_000, 3_500_000, 5_000_000, 5_500_000, 6_000_000, 7_000_000, 8_000_000,
    9_000_000, 10_000_000, 12_000_000, 14_000_000, 15_000_000, 20_000_000, 24_000_000, 28_000_000,
];

/// Port of `hackrf_compute_baseband_filter_bw_round_down_lt`: the widest
/// table entry strictly below `bandwidth_hz`, or the first entry.
pub fn baseband_filter_bw_round_down_lt(bandwidth_hz: u32) -> u32 {
    let table = &BASEBAND_FILTER_BANDWIDTHS_HZ;
    let idx = table
        .iter()
        .position(|&bw| bw >= bandwidth_hz)
        .unwrap_or(table.len());
    table[idx.saturating_sub(1)]
}

/// Port of `hackrf_compute_baseband_filter_bw`: the widest table entry that is
/// not above `bandwidth_hz` (an exact match is returned as is), or the first
/// entry.
pub fn baseband_filter_bw(bandwidth_hz: u32) -> u32 {
    let table = &BASEBAND_FILTER_BANDWIDTHS_HZ;
    let idx = table
        .iter()
        .position(|&bw| bw >= bandwidth_hz)
        .unwrap_or(table.len());
    if idx == 0 {
        table[0]
    } else if idx < table.len() && table[idx] == bandwidth_hz {
        table[idx]
    } else {
        table[idx - 1]
    }
}

/// The baseband filter libhackrf selects automatically whenever the sample
/// rate is set: the widest filter not above 75 % of the sample rate.
pub fn auto_baseband_filter_bw(sample_rate: f64) -> u32 {
    let target = (0.75 * sample_rate).max(0.0).min(u32::MAX as f64) as u32;
    baseband_filter_bw(target)
}

/// String key/value arguments (SoapySDR `Kwargs`, seify `Args`).
pub type Kwargs = BTreeMap<String, String>;

/// Build a [`Kwargs`] from string pairs.
pub fn kwargs<I, K, V>(pairs: I) -> Kwargs
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    pairs
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}

/// Stream direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Receive (device → host).
    Rx,
    /// Transmit (host → device).
    Tx,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Direction::Rx => "RX",
            Direction::Tx => "TX",
        })
    }
}

/// Host-side sample format of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamFormat {
    /// Complex signed 8-bit (the native format).
    CS8,
    /// Complex signed 16-bit.
    CS16,
    /// Complex 32-bit float.
    CF32,
    /// Complex 64-bit float.
    CF64,
}

impl StreamFormat {
    /// All supported formats, in the order the C++ driver advertised them.
    pub const ALL: [StreamFormat; 4] = [
        StreamFormat::CS8,
        StreamFormat::CS16,
        StreamFormat::CF32,
        StreamFormat::CF64,
    ];

    /// The SoapySDR format string.
    pub fn name(self) -> &'static str {
        match self {
            StreamFormat::CS8 => "CS8",
            StreamFormat::CS16 => "CS16",
            StreamFormat::CF32 => "CF32",
            StreamFormat::CF64 => "CF64",
        }
    }

    /// Parse a SoapySDR format string.
    pub fn from_name(name: &str) -> Option<StreamFormat> {
        StreamFormat::ALL.iter().copied().find(|f| f.name() == name)
    }

    /// Bytes per scalar element (half a complex sample).
    pub fn bytes_per_element(self) -> usize {
        match self {
            StreamFormat::CS8 => 1,
            StreamFormat::CS16 => 2,
            StreamFormat::CF32 => 4,
            StreamFormat::CF64 => 8,
        }
    }
}

impl fmt::Display for StreamFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which half-duplex mode the hardware is currently in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransceiverMode {
    /// Idle.
    Off,
    /// Receiving.
    Rx,
    /// Transmitting.
    Tx,
}

/// A closed numeric range with an optional step (0 means continuous).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    /// Lowest value.
    pub minimum: f64,
    /// Highest value.
    pub maximum: f64,
    /// Step between valid values, 0 for continuous.
    pub step: f64,
}

impl Range {
    /// Construct a range.
    pub const fn new(minimum: f64, maximum: f64, step: f64) -> Range {
        Range {
            minimum,
            maximum,
            step,
        }
    }

    /// Whether `value` lies inside `[minimum, maximum]` (the step is ignored).
    pub fn contains(&self, value: f64) -> bool {
        value >= self.minimum && value <= self.maximum
    }
}

/// Data type of an argument described by [`ArgInfo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgType {
    /// "true" / "false".
    Bool,
    /// Integer.
    Int,
    /// Floating point.
    Float,
    /// Free text.
    String,
}

/// Description of a settings or stream argument (SoapySDR `ArgInfo`).
#[derive(Clone, Debug, PartialEq)]
pub struct ArgInfo {
    /// Argument key.
    pub key: String,
    /// Default value.
    pub value: String,
    /// Human readable name.
    pub name: String,
    /// Human readable description.
    pub description: String,
    /// Units, if any.
    pub units: String,
    /// Data type.
    pub arg_type: ArgType,
    /// Valid range, if any.
    pub range: Option<Range>,
    /// Valid discrete options, if any.
    pub options: Vec<String>,
}

/// Stream flags (SoapySDR `SOAPY_SDR_*` bit flags).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StreamFlags(pub u32);

impl StreamFlags {
    /// No flags.
    pub const NONE: StreamFlags = StreamFlags(0);
    /// End of a burst: flush partial buffers and stop after the last sample.
    pub const END_BURST: StreamFlags = StreamFlags(1 << 1);
    /// A timestamp accompanies the call (ignored: HackRF has no time source).
    pub const HAS_TIME: StreamFlags = StreamFlags(1 << 2);
    /// The stream terminated prematurely (set on overflow).
    pub const END_ABRUPT: StreamFlags = StreamFlags(1 << 3);
    /// Read only a single packet (ignored).
    pub const ONE_PACKET: StreamFlags = StreamFlags(1 << 4);
    /// More fragments follow (never set).
    pub const MORE_FRAGMENTS: StreamFlags = StreamFlags(1 << 5);
    /// Wait for a trigger (ignored).
    pub const WAIT_TRIGGER: StreamFlags = StreamFlags(1 << 6);

    /// Whether every bit of `other` is set.
    pub const fn contains(self, other: StreamFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// Set the bits of `other`.
    pub fn insert(&mut self, other: StreamFlags) {
        self.0 |= other.0;
    }

    /// Whether no bit is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for StreamFlags {
    type Output = StreamFlags;
    fn bitor(self, rhs: StreamFlags) -> StreamFlags {
        StreamFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for StreamFlags {
    fn bitor_assign(&mut self, rhs: StreamFlags) {
        self.0 |= rhs.0;
    }
}

/// HackRF board identifiers (`enum hackrf_board_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoardId {
    /// Pre-production Jellybean.
    Jellybean,
    /// Jawbreaker beta board.
    Jawbreaker,
    /// HackRF One, revisions before r9.
    HackrfOneOg,
    /// rad1o (CCC special edition).
    Rad1o,
    /// HackRF One r9 and later.
    HackrfOneR9,
    /// Detection ran but failed.
    Unrecognized,
    /// Detection not attempted (`BOARD_ID_INVALID`).
    Undetected,
    /// A value libhackrf does not know.
    Unknown(u8),
}

impl BoardId {
    /// Decode the raw byte returned by `hackrf_board_id_read`.
    pub fn from_u8(id: u8) -> BoardId {
        match id {
            0 => BoardId::Jellybean,
            1 => BoardId::Jawbreaker,
            2 => BoardId::HackrfOneOg,
            3 => BoardId::Rad1o,
            4 => BoardId::HackrfOneR9,
            0xFE => BoardId::Unrecognized,
            0xFF => BoardId::Undetected,
            other => BoardId::Unknown(other),
        }
    }

    /// The raw byte value.
    pub fn as_u8(self) -> u8 {
        match self {
            BoardId::Jellybean => 0,
            BoardId::Jawbreaker => 1,
            BoardId::HackrfOneOg => 2,
            BoardId::Rad1o => 3,
            BoardId::HackrfOneR9 => 4,
            BoardId::Unrecognized => 0xFE,
            BoardId::Undetected => 0xFF,
            BoardId::Unknown(v) => v,
        }
    }

    /// Human readable name, identical to `hackrf_board_id_name()`.
    pub fn name(self) -> &'static str {
        match self {
            BoardId::Jellybean => "Jellybean",
            BoardId::Jawbreaker => "Jawbreaker",
            BoardId::HackrfOneOg | BoardId::HackrfOneR9 => "HackRF One",
            BoardId::Rad1o => "rad1o",
            BoardId::Unrecognized => "unrecognized",
            BoardId::Undetected => "undetected",
            BoardId::Unknown(_) => "unknown",
        }
    }
}

/// Format the MCU part id the way the C++ driver did (`%08x%08x`).
pub fn format_part_id(part_id: [u32; 2]) -> String {
    format!("{:08x}{:08x}", part_id[0], part_id[1])
}

/// Format the MCU serial number the way the C++ driver did (`%08x%08x%08x%08x`).
pub fn format_serial(serial_no: [u32; 4]) -> String {
    format!(
        "{:08x}{:08x}{:08x}{:08x}",
        serial_no[0], serial_no[1], serial_no[2], serial_no[3]
    )
}

/// The serial with leading zeros removed, as used in device labels.
pub fn trimmed_serial(serial: &str) -> &str {
    serial.trim_start_matches('0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_table_round_down_lt() {
        assert_eq!(baseband_filter_bw_round_down_lt(0), 1_750_000);
        assert_eq!(baseband_filter_bw_round_down_lt(1_750_000), 1_750_000);
        assert_eq!(baseband_filter_bw_round_down_lt(1_750_001), 1_750_000);
        assert_eq!(baseband_filter_bw_round_down_lt(2_500_000), 1_750_000);
        assert_eq!(baseband_filter_bw_round_down_lt(7_500_000), 7_000_000);
        assert_eq!(baseband_filter_bw_round_down_lt(28_000_000), 24_000_000);
        assert_eq!(baseband_filter_bw_round_down_lt(u32::MAX), 28_000_000);
    }

    #[test]
    fn filter_table_nearest_not_above() {
        assert_eq!(baseband_filter_bw(0), 1_750_000);
        assert_eq!(baseband_filter_bw(2_500_000), 2_500_000);
        assert_eq!(baseband_filter_bw(2_600_000), 2_500_000);
        assert_eq!(baseband_filter_bw(7_500_000), 7_000_000);
        assert_eq!(baseband_filter_bw(28_000_000), 28_000_000);
        assert_eq!(baseband_filter_bw(u32::MAX), 28_000_000);
    }

    #[test]
    fn auto_filter_is_75_percent_of_rate() {
        assert_eq!(auto_baseband_filter_bw(10e6), 7_000_000);
        assert_eq!(auto_baseband_filter_bw(20e6), 15_000_000);
        assert_eq!(auto_baseband_filter_bw(8e6), 6_000_000);
        assert_eq!(auto_baseband_filter_bw(2e6), 1_750_000);
    }

    #[test]
    fn format_round_trip() {
        for f in StreamFormat::ALL {
            assert_eq!(StreamFormat::from_name(f.name()), Some(f));
        }
        assert_eq!(StreamFormat::from_name("CU8"), None);
    }

    #[test]
    fn flags() {
        let mut f = StreamFlags::NONE;
        assert!(f.is_empty());
        f |= StreamFlags::END_BURST;
        assert!(f.contains(StreamFlags::END_BURST));
        assert!(!f.contains(StreamFlags::END_ABRUPT));
        assert_eq!(StreamFlags::END_BURST.0, 2);
        assert_eq!(StreamFlags::END_ABRUPT.0, 8);
    }

    #[test]
    fn board_names_match_libhackrf() {
        assert_eq!(BoardId::from_u8(2).name(), "HackRF One");
        assert_eq!(BoardId::from_u8(4).name(), "HackRF One");
        assert_eq!(BoardId::from_u8(3).name(), "rad1o");
        assert_eq!(BoardId::from_u8(0xFF), BoardId::Undetected);
        assert_eq!(BoardId::from_u8(0xFF).name(), "undetected");
        assert_eq!(BoardId::from_u8(9).as_u8(), 9);
    }

    #[test]
    fn serial_formatting() {
        let s = format_serial([0, 0, 0xa06063c8, 0x2e6b6b1f]);
        assert_eq!(s, "0000000000000000a06063c82e6b6b1f");
        assert_eq!(trimmed_serial(&s), "a06063c82e6b6b1f");
        assert_eq!(format_part_id([0xa000cb3c, 0x00514f4e]), "a000cb3c00514f4e");
        assert_eq!(trimmed_serial("0000"), "");
    }
}
