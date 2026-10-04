//! Shared helpers for the integration tests, including a literal transcription
//! of the C++ driver's arithmetic ("legacy oracle") used to prove equivalence
//! where the old code was right and to document where it was wrong.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use soapyhackrf::mock::{MockBackend, MockDeviceInfo};
use soapyhackrf::{HackRf, Kwargs};

static NEXT_SERIAL: AtomicU32 = AtomicU32::new(0x1000);

/// A device description with a serial no other test in this process uses
/// (the discovery cache and the claimed-serial set are process wide).
pub fn fresh_info() -> MockDeviceInfo {
    MockDeviceInfo::new([0, 0, 0xa06063c8, NEXT_SERIAL.fetch_add(1, Ordering::SeqCst)])
}

/// A backend with one fresh device, opened.
pub fn open_single() -> (Arc<MockBackend>, HackRf<MockBackend>, String) {
    let info = fresh_info();
    let serial = info.serial();
    let backend = MockBackend::new(vec![info]);
    let dev =
        HackRf::open(Arc::clone(&backend), &kw(&[("serial", serial.as_str())])).expect("open");
    backend.clear_calls();
    (backend, dev, serial)
}

/// Build `Kwargs` from pairs.
pub fn kw(pairs: &[(&str, &str)]) -> Kwargs {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Interleaved I/Q test pattern of `samples` complex samples: a 16-bit
/// counter starting at `seed`, I = low byte, Q = high byte (as i8).
pub fn pattern(seed: u32, samples: usize) -> Vec<i8> {
    (0..samples)
        .flat_map(|i| {
            let c = (seed as usize + i) as u16;
            [(c & 0xff) as u8 as i8, (c >> 8) as u8 as i8]
        })
        .collect()
}

/// Literal transcription of the C++ arithmetic.
pub mod legacy {
    /// `SoapyHackRF::setGain(SOAPY_SDR_RX, value)` as written in
    /// HackRF_Settings.cpp, including integer truncation and unsigned wraparound.
    /// Returns `None` where the C++ code left every field untouched.
    pub fn rx_set_gain(value: f64) -> Option<(u32, u32, u8)> {
        let gain: i32 = value as i32; // int32_t gain = value;
        let lna: u32;
        let vga: u32;
        let amp: u8;
        if gain <= 0 {
            lna = 0;
            vga = 0;
            amp = 0;
        } else if gain <= 40 / 2 + 62 / 2 {
            vga = ((gain / 3) & !0x1) as u32;
            lna = (gain as u32).wrapping_sub(vga);
            amp = 0;
        } else if gain <= 40 / 2 + 62 / 2 + 14 {
            amp = 14;
            vga = (((gain - amp as i32) / 3) & !0x1) as u32;
            lna = ((gain - amp as i32) as u32).wrapping_sub(vga);
        } else if gain <= 40 + 62 + 14 {
            amp = 14;
            vga = ((gain - amp as i32) as f64 * 40.0 / 62.0) as u32;
            lna = ((gain - amp as i32) as u32).wrapping_sub(vga);
        } else {
            return None;
        }
        Some((lna, vga, amp))
    }

    /// `SoapyHackRF::setGain(SOAPY_SDR_TX, value)`.
    pub fn tx_set_gain(value: f64) -> Option<(u32, u8)> {
        let gain: i32 = value as i32;
        if gain <= 0 {
            Some((0, 0))
        } else if gain <= 47 / 2 {
            Some((gain as u32, 0))
        } else if gain <= 47 + 14 {
            Some(((gain - 14) as u32, 14))
        } else {
            None
        }
    }

    /// What libhackrf accepts (no `HACKRF_ERROR_INVALID_PARAM`).
    pub fn hw_accepts_rx(lna: u32, vga: u32) -> bool {
        lna <= 40 && vga <= 62
    }

    /// Whether the values are on the hardware steps, i.e. the hardware gain
    /// equals what the driver would report.
    pub fn rx_exact(lna: u32, vga: u32) -> bool {
        hw_accepts_rx(lna, vga) && lna % 8 == 0 && vga % 2 == 0
    }

    /// `readbuf()` for one element, CS8 -> CS16.
    pub fn cs8_to_cs16(v: i8) -> i16 {
        ((v as i32) << 8) as i16
    }

    /// `readbuf()` for one element, CS8 -> CF32: `(float)(src/127.0)`.
    pub fn cs8_to_cf32(v: i8) -> f32 {
        (v as f64 / 127.0) as f32
    }

    /// `readbuf()` for one element, CS8 -> CF64.
    pub fn cs8_to_cf64(v: i8) -> f64 {
        v as f64 / 127.0
    }

    /// `writebuf()` CS16 -> CS8: `(int8_t)(v >> 8)`.
    pub fn cs16_to_cs8(v: i16) -> i8 {
        (v >> 8) as i8
    }

    /// `writebuf()` CF32 -> CS8 for in-range input: `(int8_t)(v * 127.0)`.
    pub fn cf32_to_cs8_in_range(v: f32) -> i8 {
        let scaled = v as f64 * 127.0;
        assert!(
            scaled > -129.0 && scaled < 128.0,
            "out of range is UB in C++"
        );
        scaled as i8
    }
}
