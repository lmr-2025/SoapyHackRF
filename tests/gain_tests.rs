//! Gain distribution: equivalence with the C++ driver where it was correct,
//! documented divergence where it produced values the hardware rejects.

mod common;

use common::legacy;
use soapyhackrf::gain::*;

#[test]
fn rx_split_is_always_valid_for_the_hardware() {
    for g in -10..=130 {
        let s = distribute_rx_gain(g as f64);
        assert!(
            s.lna_db <= 40 && s.lna_db % 8 == 0,
            "g={g} lna={}",
            s.lna_db
        );
        assert!(
            s.vga_db <= 62 && s.vga_db % 2 == 0,
            "g={g} vga={}",
            s.vga_db
        );
        assert!(s.amp_db == 0 || s.amp_db == 14, "g={g} amp={}", s.amp_db);
    }
}

#[test]
fn rx_split_reaches_every_even_total_exactly() {
    for g in (0..=116).step_by(2) {
        assert_eq!(distribute_rx_gain(g as f64).total(), g, "g={g}");
    }
    for g in (1..=115).step_by(2) {
        assert_eq!(distribute_rx_gain(g as f64).total(), g - 1, "g={g}");
    }
}

#[test]
fn rx_split_is_monotonic_and_clamped() {
    let mut last = 0;
    for g in 0..=116 {
        let t = distribute_rx_gain(g as f64).total();
        assert!(t >= last, "g={g}: {t} < {last}");
        last = t;
    }
    assert_eq!(distribute_rx_gain(116.0).total(), 116);
    assert_eq!(distribute_rx_gain(1000.0), distribute_rx_gain(116.0));
    assert_eq!(distribute_rx_gain(-5.0).total(), 0);
    assert_eq!(distribute_rx_gain(f64::NAN).total(), 0);
    assert_eq!(distribute_rx_gain(f64::INFINITY).total(), 116);
    assert_eq!(distribute_rx_gain(f64::NEG_INFINITY).total(), 0);
    // Fractions truncate toward zero like the C++ `int32_t gain = value`.
    assert_eq!(distribute_rx_gain(23.9), distribute_rx_gain(23.0));
}

#[test]
fn rx_amp_threshold_matches_cpp() {
    assert_eq!(distribute_rx_gain(51.0).amp_db, 0);
    assert_eq!(distribute_rx_gain(52.0).amp_db, 14);
    for g in 0..=116 {
        let legacy_amp = legacy::rx_set_gain(g as f64).map(|(_, _, a)| a as u32);
        assert_eq!(
            Some(distribute_rx_gain(g as f64).amp_db),
            legacy_amp,
            "g={g}"
        );
    }
}

#[test]
fn rx_matches_legacy_wherever_legacy_was_exact() {
    // Where the C++ algorithm happened to produce values on the hardware
    // steps, the new one yields the same total with the same amplifier state.
    let mut exact_cases = 0;
    for g in 0..=116u32 {
        let (lna, vga, amp) = legacy::rx_set_gain(g as f64).unwrap();
        if legacy::rx_exact(lna, vga) {
            exact_cases += 1;
            let s = distribute_rx_gain(g as f64);
            assert_eq!(s.total(), lna + vga + amp as u32, "g={g}");
            assert_eq!(s.amp_db, amp as u32, "g={g}");
        }
    }
    assert!(exact_cases >= 10, "only {exact_cases} exact legacy cases");
}

#[test]
fn legacy_rx_algorithm_produced_invalid_hardware_values() {
    // BUGS.md G1: VGA above 62 dB for high overall gains (libhackrf rejects it).
    let (_, vga, _) = legacy::rx_set_gain(116.0).unwrap();
    assert_eq!(vga, 65);
    let rejected: Vec<u32> = (0..=116)
        .filter(|&g| {
            let (lna, vga, _) = legacy::rx_set_gain(g as f64).unwrap();
            !legacy::hw_accepts_rx(lna, vga)
        })
        .collect();
    assert_eq!(rejected, vec![112, 113, 114, 115, 116]);

    // BUGS.md G2: LNA values off the 8 dB grid; the hardware rounds down
    // silently while getGain() reports the unrounded value.
    let (lna, vga, _) = legacy::rx_set_gain(51.0).unwrap();
    assert_eq!((lna, vga), (35, 16));
    let off_grid = (0..=116)
        .filter(|&g| {
            let (lna, vga, _) = legacy::rx_set_gain(g as f64).unwrap();
            legacy::hw_accepts_rx(lna, vga) && !legacy::rx_exact(lna, vga)
        })
        .count();
    assert!(off_grid > 60, "{off_grid} off-grid cases");

    // BUGS.md G3: nothing happens above 116 dB instead of clamping.
    assert_eq!(legacy::rx_set_gain(117.0), None);
    assert_eq!(distribute_rx_gain(117.0).total(), 116);
}

#[test]
fn tx_matches_legacy_in_range_and_clamps_above() {
    for g in -5..=61 {
        let s = distribute_tx_gain(g as f64);
        let (vga, amp) = legacy::tx_set_gain(g as f64).unwrap();
        assert_eq!((s.vga_db, s.amp_db), (vga, amp as u32), "g={g}");
        assert_eq!(s.total(), g.max(0) as u32);
    }
    assert_eq!(legacy::tx_set_gain(62.0), None);
    assert_eq!(distribute_tx_gain(62.0).total(), 61);
    assert_eq!(distribute_tx_gain(23.0).amp_db, 0);
    assert_eq!(distribute_tx_gain(24.0).amp_db, 14);
    assert_eq!(distribute_tx_gain(24.0).vga_db, 10);
}

#[test]
fn element_quantisation_tracks_libhackrf_masking() {
    for v in 0..=40u32 {
        assert_eq!(quantize_lna(v as f64), v & !0x07);
    }
    for v in 0..=62u32 {
        assert_eq!(quantize_rx_vga(v as f64), v & !0x01);
    }
    for v in 0..=47u32 {
        assert_eq!(quantize_tx_vga(v as f64), v);
    }
    assert_eq!(quantize_lna(41.0), 40);
    assert_eq!(quantize_rx_vga(63.0), 62);
    assert_eq!(quantize_tx_vga(48.0), 47);
    // BUGS.md G5: negative AMP values switched the amplifier *on* in C++.
    assert_eq!(quantize_amp(-1.0), 0);
    assert_eq!(quantize_amp(1.0), 14);
}
