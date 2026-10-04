//! Sample format conversion: byte-exact with the C++ readbuf/writebuf for
//! in-range values, saturating where C++ was undefined.

mod common;

use common::legacy;
use soapyhackrf::convert::*;
use soapyhackrf::StreamFormat;

fn all_i8() -> impl Iterator<Item = i8> {
    (-128..=127).map(|v| v as i8)
}

#[test]
fn cs8_is_identity() {
    for v in all_i8() {
        assert_eq!(i8::from_native(v), v);
        assert_eq!(v.to_native(), v);
    }
}

#[test]
fn cs16_matches_legacy_shift() {
    for v in all_i8() {
        assert_eq!(i16::from_native(v), legacy::cs8_to_cs16(v));
        assert_eq!(i16::from_native(v).to_native(), v);
    }
    for v in [i16::MIN, -256, -255, -1, 0, 1, 255, 256, i16::MAX] {
        assert_eq!(v.to_native(), legacy::cs16_to_cs8(v));
    }
    assert_eq!(i16::MAX.to_native(), 127);
    assert_eq!(i16::MIN.to_native(), -128);
    assert_eq!(255i16.to_native(), 0);
    assert_eq!((-1i16).to_native(), -1);
}

#[test]
fn cf32_matches_legacy_scaling() {
    for v in all_i8() {
        assert_eq!(f32::from_native(v), legacy::cs8_to_cf32(v));
        assert_eq!(f64::from_native(v), legacy::cs8_to_cf64(v));
    }
    assert_eq!(f32::from_native(127), 1.0);
    assert_eq!(f32::from_native(-127), -1.0);
    assert!(f32::from_native(-128) < -1.0);
    // Wherever the scaled value is already an integer, rounding and the C++
    // truncation agree.
    let mut agreed = 0;
    for k in -1270..=1270 {
        let v = k as f32 / 1270.0;
        if (v as f64 * 127.0).fract() == 0.0 {
            agreed += 1;
            assert_eq!(v.to_native(), legacy::cf32_to_cs8_in_range(v), "{v}");
            assert_eq!(
                (v as f64).to_native(),
                legacy::cf32_to_cs8_in_range(v),
                "{v}"
            );
        }
    }
    assert!(agreed >= 3, "{agreed}");
    assert_eq!(1.0f32.to_native(), 127);
    assert_eq!((-1.0f32).to_native(), -127);
}

#[test]
fn float_to_native_rounds_where_legacy_truncated() {
    // BUGS.md C3: `(int8_t)(x * 127.0)` truncates toward zero, so e.g. 0.999
    // became 126 and the round trip CS8 -> CF32 -> CS8 lost one LSB. The port
    // rounds to the nearest value.
    assert_eq!(legacy::cf32_to_cs8_in_range(0.999), 126);
    assert_eq!(0.999f32.to_native(), 127);
    assert_eq!(legacy::cf32_to_cs8_in_range(-0.999), -126);
    assert_eq!((-0.999f32).to_native(), -127);
    assert_eq!(legacy::cf32_to_cs8_in_range(0.5), 63);
    assert_eq!(0.5f32.to_native(), 64, "63.5 rounds half away from zero");
    assert_eq!(0.004f32.to_native(), 1);
    assert_eq!(legacy::cf32_to_cs8_in_range(0.004), 0);
    // Legacy round trip was lossy for almost every value.
    let lossy = (-127..=127i8)
        .filter(|&v| legacy::cf32_to_cs8_in_range(legacy::cs8_to_cf32(v)) != v)
        .count();
    assert!(
        lossy > 100,
        "{lossy} of 255 values changed in the C++ round trip"
    );
}

#[test]
fn float_round_trip_is_lossless_in_range() {
    for v in (-127..=127).map(|v| v as i8) {
        assert_eq!(f32::from_native(v).to_native(), v, "{v}");
        assert_eq!(f64::from_native(v).to_native(), v, "{v}");
    }
}

#[test]
fn out_of_range_floats_saturate_instead_of_wrapping() {
    // BUGS.md C2: `(int8_t)(x * 127.0)` is undefined for |x| > 1 in C++.
    assert_eq!(2.0f32.to_native(), 127);
    assert_eq!((-2.0f32).to_native(), -128);
    assert_eq!(f32::INFINITY.to_native(), 127);
    assert_eq!(f32::NEG_INFINITY.to_native(), -128);
    assert_eq!(f32::NAN.to_native(), 0);
    assert_eq!(1e300f64.to_native(), 127);
    assert_eq!(f64::NAN.to_native(), 0);
}

#[test]
fn full_scale_constants_document_the_legacy_inconsistency() {
    // BUGS.md C1: conversion divides by 127 while the advertised full scale is 128.
    assert_eq!(CONVERSION_SCALE, 127.0);
    assert_eq!(NATIVE_FULL_SCALE, 128.0);
}

#[test]
fn bulk_conversion_preserves_interleaving() {
    let native: Vec<i8> = vec![1, -2, 3, -4, 127, -128];
    let mut cs16 = vec![0i16; 6];
    native_to_samples(&native, &mut cs16);
    assert_eq!(cs16, vec![256, -512, 768, -1024, 32512, -32768]);
    let mut back = vec![0i8; 6];
    samples_to_native(&cs16, &mut back);
    assert_eq!(back, native);

    let mut cf32 = vec![0f32; 6];
    native_to_samples(&native, &mut cf32);
    assert_eq!(cf32[4], 1.0);
    let mut back = vec![0i8; 6];
    samples_to_native(&cf32, &mut back);
    assert_eq!(back, native);
}

#[test]
#[should_panic(expected = "length mismatch")]
fn bulk_conversion_checks_lengths() {
    let mut dst = vec![0f32; 3];
    native_to_samples(&[1i8, 2], &mut dst);
}

#[test]
fn dynamic_buffers_convert_at_offsets() {
    let mut out = vec![0f32; 8];
    let mut view = SampleBufMut::from(out.as_mut_slice());
    assert_eq!(view.format(), StreamFormat::CF32);
    assert_eq!(view.num_samples(), 4);
    view.write_from_native(1, &[127, -127, 0, 127]);
    assert_eq!(out, vec![0.0, 0.0, 1.0, -1.0, 0.0, 1.0, 0.0, 0.0]);

    let input = vec![0i16, 0, 256, -256, 32767, -32768];
    let view = SampleBuf::from(input.as_slice());
    assert_eq!(view.format(), StreamFormat::CS16);
    assert_eq!(view.num_samples(), 3);
    let mut native = [0i8; 4];
    view.read_to_native(1, &mut native);
    assert_eq!(native, [1, -1, 127, -128]);
}

#[test]
fn dynamic_buffers_report_formats() {
    let mut a = [0i8; 2];
    let mut b = [0i16; 2];
    let mut c = [0f32; 2];
    let mut d = [0f64; 3];
    assert_eq!(SampleBufMut::from(&mut a[..]).format(), StreamFormat::CS8);
    assert_eq!(SampleBufMut::from(&mut b[..]).format(), StreamFormat::CS16);
    assert_eq!(SampleBufMut::from(&mut c[..]).format(), StreamFormat::CF32);
    let v = SampleBufMut::from(&mut d[..]);
    assert_eq!(v.format(), StreamFormat::CF64);
    assert_eq!(v.len_elements(), 3);
    assert_eq!(
        v.num_samples(),
        1,
        "odd element counts hold a whole sample less"
    );
    assert_eq!(SampleBuf::from(&a[..]).format(), StreamFormat::CS8);
    assert_eq!(SampleBuf::from(&b[..]).format(), StreamFormat::CS16);
    assert_eq!(SampleBuf::from(&c[..]).format(), StreamFormat::CF32);
    assert_eq!(SampleBuf::from(&d[..]).format(), StreamFormat::CF64);
}
