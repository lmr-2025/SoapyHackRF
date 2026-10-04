//! Gain quantisation and the overall-gain distribution algorithm.
//!
//! The HackRF has three RX gain stages (RF amplifier: 0/14 dB, LNA: 0–40 dB in
//! 8 dB steps, VGA: 0–62 dB in 2 dB steps) and two TX stages (RF amplifier and
//! a 0–47 dB VGA in 1 dB steps). `set_gain(direction, value)` has to split one
//! number across those stages.
//!
//! The C++ driver's algorithm produced values the hardware rejects or silently
//! rounds (see `BUGS.md`, items G1–G4). This module keeps its structure (the
//! same amplifier switch-on threshold, VGA ≈ one third of the remaining gain)
//! but only ever yields values that are valid for libhackrf, and always
//! reaches the requested total exactly whenever it is representable.

use crate::types::{
    AMP_MAX_DB, RX_LNA_MAX_DB, RX_LNA_STEP_DB, RX_VGA_MAX_DB, RX_VGA_STEP_DB, TX_VGA_MAX_DB,
};

/// Highest overall RX gain (LNA + VGA + amplifier).
pub const RX_GAIN_MAX_DB: u32 = RX_LNA_MAX_DB + RX_VGA_MAX_DB + AMP_MAX_DB;
/// Highest overall TX gain (VGA + amplifier).
pub const TX_GAIN_MAX_DB: u32 = TX_VGA_MAX_DB + AMP_MAX_DB;
/// Overall RX gains above this value switch the RF amplifier on
/// (`RX_LNA_MAX_DB / 2 + RX_VGA_MAX_DB / 2`, as in the C++ driver).
pub const RX_AMP_THRESHOLD_DB: u32 = RX_LNA_MAX_DB / 2 + RX_VGA_MAX_DB / 2;
/// Overall TX gains above this value switch the RF amplifier on
/// (`TX_VGA_MAX_DB / 2`, as in the C++ driver).
pub const TX_AMP_THRESHOLD_DB: u32 = TX_VGA_MAX_DB / 2;

/// How an overall RX gain is split across the three RX stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RxGainSplit {
    /// LNA (IF) gain in dB, a multiple of 8 in `0..=40`.
    pub lna_db: u32,
    /// VGA (baseband) gain in dB, a multiple of 2 in `0..=62`.
    pub vga_db: u32,
    /// RF amplifier gain in dB, 0 or 14.
    pub amp_db: u32,
}

impl RxGainSplit {
    /// Sum of all stages.
    pub fn total(&self) -> u32 {
        self.lna_db + self.vga_db + self.amp_db
    }
}

/// How an overall TX gain is split across the two TX stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TxGainSplit {
    /// VGA (IF) gain in dB in `0..=47`.
    pub vga_db: u32,
    /// RF amplifier gain in dB, 0 or 14.
    pub amp_db: u32,
}

impl TxGainSplit {
    /// Sum of all stages.
    pub fn total(&self) -> u32 {
        self.vga_db + self.amp_db
    }
}

/// Truncate a dB value toward zero into `0..=max`, as the C++ driver's
/// `int32_t gain = value` did for positive values (negative values and NaN
/// become 0 instead of wrapping).
fn clamp_db(db: f64, max: u32) -> u32 {
    if db.is_nan() || db <= 0.0 {
        0
    } else if db >= max as f64 {
        max
    } else {
        db as u32
    }
}

/// Quantise an RX LNA gain: clamp into `0..=40` and round down to a multiple
/// of 8 (what libhackrf itself does with `value &= ~0x07`).
pub fn quantize_lna(db: f64) -> u32 {
    clamp_db(db, RX_LNA_MAX_DB) / RX_LNA_STEP_DB * RX_LNA_STEP_DB
}

/// Quantise an RX VGA gain: clamp into `0..=62` and round down to a multiple
/// of 2 (what libhackrf itself does with `value &= ~0x01`).
pub fn quantize_rx_vga(db: f64) -> u32 {
    clamp_db(db, RX_VGA_MAX_DB) / RX_VGA_STEP_DB * RX_VGA_STEP_DB
}

/// Quantise a TX VGA gain: clamp into `0..=47`, 1 dB steps.
pub fn quantize_tx_vga(db: f64) -> u32 {
    clamp_db(db, TX_VGA_MAX_DB)
}

/// Quantise an amplifier gain: anything above 0 dB switches the amplifier on
/// (14 dB), everything else (including negative values) switches it off.
pub fn quantize_amp(db: f64) -> u32 {
    if db > 0.0 {
        AMP_MAX_DB
    } else {
        0
    }
}

/// Split an overall RX gain across amplifier, LNA and VGA.
///
/// * values ≤ 0 (or NaN) give 0 dB everywhere, values ≥ 116 give the maximum;
/// * the amplifier is switched on above [`RX_AMP_THRESHOLD_DB`] (51 dB);
/// * the remainder is split roughly 2:1 between LNA and VGA, rounded to the
///   hardware steps, so every even total is reached exactly and every odd
///   total is reached to within 1 dB (the stages only have even steps).
pub fn distribute_rx_gain(db: f64) -> RxGainSplit {
    let gain = clamp_db(db, RX_GAIN_MAX_DB);
    let amp_db = if gain > RX_AMP_THRESHOLD_DB {
        AMP_MAX_DB
    } else {
        0
    };
    let remaining = gain - amp_db;
    let lna_db = ((remaining * 2 / 3) / RX_LNA_STEP_DB * RX_LNA_STEP_DB).min(RX_LNA_MAX_DB);
    let vga_db = ((remaining - lna_db) / RX_VGA_STEP_DB * RX_VGA_STEP_DB).min(RX_VGA_MAX_DB);
    RxGainSplit {
        lna_db,
        vga_db,
        amp_db,
    }
}

/// Split an overall TX gain across amplifier and VGA.
///
/// * values ≤ 0 (or NaN) give 0 dB, values ≥ 61 give the maximum;
/// * the amplifier is switched on above [`TX_AMP_THRESHOLD_DB`] (23 dB) and
///   the VGA carries the rest, so every integer total is reached exactly.
pub fn distribute_tx_gain(db: f64) -> TxGainSplit {
    let gain = clamp_db(db, TX_GAIN_MAX_DB);
    if gain > TX_AMP_THRESHOLD_DB {
        TxGainSplit {
            vga_db: gain - AMP_MAX_DB,
            amp_db: AMP_MAX_DB,
        }
    } else {
        TxGainSplit {
            vga_db: gain,
            amp_db: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_match_cpp_driver() {
        assert_eq!(RX_AMP_THRESHOLD_DB, 51);
        assert_eq!(TX_AMP_THRESHOLD_DB, 23);
        assert_eq!(RX_GAIN_MAX_DB, 116);
        assert_eq!(TX_GAIN_MAX_DB, 61);
    }

    #[test]
    fn quantisation() {
        assert_eq!(quantize_lna(-3.0), 0);
        assert_eq!(quantize_lna(7.9), 0);
        assert_eq!(quantize_lna(8.0), 8);
        assert_eq!(quantize_lna(35.0), 32);
        assert_eq!(quantize_lna(1000.0), 40);
        assert_eq!(quantize_lna(f64::NAN), 0);
        assert_eq!(quantize_rx_vga(1.0), 0);
        assert_eq!(quantize_rx_vga(3.0), 2);
        assert_eq!(quantize_rx_vga(63.0), 62);
        assert_eq!(quantize_tx_vga(46.7), 46);
        assert_eq!(quantize_tx_vga(99.0), 47);
        assert_eq!(quantize_amp(0.0), 0);
        assert_eq!(quantize_amp(-1.0), 0);
        assert_eq!(quantize_amp(0.5), 14);
        assert_eq!(quantize_amp(14.0), 14);
    }
}
