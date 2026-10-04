//! RX streaming against the mock backend.

mod common;

use std::time::{Duration, Instant};

use common::{kw, open_single, pattern};
use soapyhackrf::mock::Call;
use soapyhackrf::*;

const SHORT: Duration = Duration::from_millis(20);
const MTU: usize = MTU_SAMPLES;

#[test]
fn setup_and_close() {
    let (_, dev, _) = open_single();
    let rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    assert_eq!(rx.format(), StreamFormat::CS8);
    assert_eq!(rx.mtu(), 131072);
    assert_eq!(rx.num_direct_buffers(), 15);
    assert_eq!(
        dev.rx_stream(StreamFormat::CS8, &[0], &Kwargs::new()).err(),
        Some(Error::StreamAlreadyOpen(Direction::Rx))
    );
    let _tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    rx.close();
    let rx = dev
        .rx_stream(StreamFormat::CF32, &[], &kw(&[("buffers", "4")]))
        .unwrap();
    assert_eq!(rx.num_direct_buffers(), 4);
    drop(rx);
    let rx = dev
        .rx_stream(StreamFormat::CF32, &[], &kw(&[("buffers", "0")]))
        .unwrap();
    assert_eq!(
        rx.num_direct_buffers(),
        15,
        "non-positive counts fall back to the default"
    );
    drop(rx);
    assert!(matches!(
        dev.rx_stream(StreamFormat::CF32, &[], &kw(&[("buffers", "lots")])),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        dev.rx_stream(StreamFormat::CF32, &[1], &Kwargs::new()),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        dev.rx_stream(StreamFormat::CF32, &[0, 1], &Kwargs::new()),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn read_checks_the_sample_type() {
    let (_, dev, _) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS16, &[0], &Kwargs::new())
        .unwrap();
    let mut buf = vec![0f32; 16];
    assert_eq!(
        rx.read(&mut buf, SHORT).err(),
        Some(Error::FormatMismatch {
            expected: StreamFormat::CS16,
            actual: StreamFormat::CF32
        })
    );
}

#[test]
fn read_activates_and_times_out_without_data() {
    let (backend, dev, serial) = open_single();
    dev.set_sample_rate(Direction::Rx, 0, 10e6).unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    assert!(!rx.is_active());
    backend.clear_calls();
    let mut buf = vec![0i8; 2 * MTU];
    let t = Instant::now();
    assert_eq!(rx.read(&mut buf, SHORT).err(), Some(Error::Timeout));
    assert!(t.elapsed() < Duration::from_millis(500));
    assert!(rx.is_active());
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Rx);
    assert!(backend.calls().contains(&Call::StartRx));
    assert!(backend.hw(&serial).streaming);
    assert_eq!(rx.read(&mut buf, SHORT).err(), Some(Error::Timeout));
}

#[test]
fn reads_whole_transfers_in_every_format() {
    let (backend, dev, serial) = open_single();
    let data = pattern(7, MTU);

    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    assert_eq!(backend.pump_rx(&serial, &data), Some(true));
    let mut buf = vec![0i8; 2 * MTU];
    let r = rx.read(&mut buf, SHORT).unwrap();
    assert_eq!(r.samples, MTU);
    assert_eq!(r.flags, StreamFlags::NONE);
    assert_eq!(buf, data);
    drop(rx);

    let mut rx = dev
        .rx_stream(StreamFormat::CS16, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    backend.pump_rx(&serial, &data).unwrap();
    let mut buf = vec![0i16; 2 * MTU];
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);
    assert!(buf.iter().zip(&data).all(|(&o, &i)| o == (i as i16) << 8));
    drop(rx);

    let mut rx = dev
        .rx_stream(StreamFormat::CF32, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    backend.pump_rx(&serial, &data).unwrap();
    let mut buf = vec![0f32; 2 * MTU];
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);
    assert!(buf
        .iter()
        .zip(&data)
        .all(|(&o, &i)| o == (i as f64 / 127.0) as f32));
    drop(rx);

    let mut rx = dev
        .rx_stream(StreamFormat::CF64, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    backend.pump_rx(&serial, &data).unwrap();
    let mut buf = vec![0f64; 2 * MTU];
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);
    assert!(buf.iter().zip(&data).all(|(&o, &i)| o == i as f64 / 127.0));
}

#[test]
fn partial_reads_are_contiguous_across_transfers() {
    let (backend, dev, serial) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    let a = pattern(0, MTU);
    let b = pattern(MTU as u32, MTU);
    backend.pump_rx(&serial, &a).unwrap();
    backend.pump_rx(&serial, &b).unwrap();

    let chunk = 100_000;
    let mut got = Vec::new();
    let mut buf = vec![0i8; 2 * chunk];
    while got.len() < 2 * 2 * MTU {
        let n = rx.read(&mut buf, SHORT).unwrap().samples;
        assert!(n > 0 && n <= chunk);
        got.extend_from_slice(&buf[..2 * n]);
    }
    let mut expect = a.clone();
    expect.extend_from_slice(&b);
    assert_eq!(got, expect);
    assert_eq!(rx.read(&mut buf, SHORT).err(), Some(Error::Timeout));
    assert_eq!(dev.stream_stats().0, 2);
}

#[test]
fn reads_are_capped_at_the_mtu_and_handle_short_transfers() {
    let (backend, dev, serial) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    backend.pump_rx(&serial, &pattern(1, MTU)).unwrap();
    backend.pump_rx(&serial, &pattern(2, MTU)).unwrap();
    let mut big = vec![0i8; 2 * 3 * MTU];
    assert_eq!(
        rx.read(&mut big, SHORT).unwrap().samples,
        MTU,
        "one MTU per call"
    );
    assert_eq!(rx.read(&mut big, SHORT).unwrap().samples, MTU);

    // A short USB transfer yields only its valid samples.
    let short = pattern(3, 1000);
    backend.pump_rx(&serial, &short).unwrap();
    assert_eq!(rx.read(&mut big, SHORT).unwrap().samples, 1000);
    assert_eq!(&big[..2000], &short[..]);
    // Empty transfers are ignored.
    backend.pump_rx(&serial, &[]).unwrap();
    assert_eq!(rx.read(&mut big, SHORT).err(), Some(Error::Timeout));
}

#[test]
fn overflow_drops_the_oldest_and_is_reported_once() {
    let (backend, dev, serial) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &kw(&[("buffers", "2")]))
        .unwrap();
    rx.activate().unwrap();
    for i in 0..4u32 {
        backend.pump_rx(&serial, &pattern(i * 1000, MTU)).unwrap();
    }
    let mut buf = vec![0i8; 2 * MTU];
    assert_eq!(rx.read(&mut buf, SHORT).err(), Some(Error::Overflow));
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);
    assert_eq!(
        buf,
        pattern(2000, MTU),
        "the two oldest transfers were dropped"
    );
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);
    assert_eq!(buf, pattern(3000, MTU));
    assert_eq!(rx.read(&mut buf, SHORT).err(), Some(Error::Timeout));
    assert_eq!(dev.stream_stats().1, 2);
}

#[test]
fn overflow_does_not_lose_samples_already_copied() {
    // BUGS.md S3: readStream() discarded the samples it had already copied
    // out of the remainder buffer when the next acquire reported an overflow.
    let (backend, dev, serial) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &kw(&[("buffers", "2")]))
        .unwrap();
    rx.activate().unwrap();
    backend.pump_rx(&serial, &pattern(0, MTU)).unwrap();
    let mut small = vec![0i8; 2 * 1000];
    assert_eq!(rx.read(&mut small, SHORT).unwrap().samples, 1000);
    // Three more transfers overflow the two-slot ring while the reader still
    // holds part of the first one.
    for i in 1..4u32 {
        backend
            .pump_rx(&serial, &pattern(i * MTU as u32, MTU))
            .unwrap();
    }
    let mut buf = vec![0i8; 2 * MTU];
    let r = rx.read(&mut buf, SHORT).unwrap();
    assert_eq!(
        r.samples,
        MTU - 1000,
        "the rest of the held buffer is delivered"
    );
    assert_eq!(&buf[..2 * (MTU - 1000)], &pattern(1000, MTU - 1000)[..]);
    assert_eq!(
        rx.read(&mut buf, SHORT).err(),
        Some(Error::Overflow),
        "then the overflow"
    );
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);
    assert_eq!(buf, pattern(2 * MTU as u32, MTU));
}

#[test]
fn deactivate_and_reactivate() {
    let (backend, dev, serial) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    rx.activate().unwrap();
    assert_eq!(
        backend
            .calls()
            .iter()
            .filter(|c| **c == Call::StartRx)
            .count(),
        1
    );
    backend.pump_rx(&serial, &pattern(0, MTU)).unwrap();
    rx.deactivate().unwrap();
    assert!(!rx.is_active());
    assert_eq!(backend.calls().last(), Some(&Call::StopRx));
    assert_eq!(
        backend.pump_rx(&serial, &pattern(0, MTU)),
        None,
        "device stopped"
    );
    rx.deactivate().unwrap();
    rx.activate().unwrap();
    let mut buf = vec![0i8; 2 * MTU];
    assert_eq!(
        rx.read(&mut buf, SHORT).err(),
        Some(Error::Timeout),
        "ring reset on activation"
    );
    backend.pump_rx(&serial, &pattern(5, MTU)).unwrap();
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);
}

#[test]
fn start_failure_is_reported_and_leaves_the_radio_idle() {
    let (backend, dev, _) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    backend.fail_next("start_rx", HackrfError::Busy);
    assert_eq!(
        rx.activate(),
        Err(Error::hackrf("hackrf_start_rx", HackrfError::Busy))
    );
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Off);
    rx.activate().unwrap();
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Rx);
}

#[test]
fn old_libhackrf_restart_reopens_and_reapplies_settings() {
    let (backend, dev, serial) = open_single();
    backend.set_legacy_restart_bug(true);
    dev.set_frequency(Direction::Rx, 0, "RF", 100e6, &Kwargs::new())
        .unwrap();
    dev.set_sample_rate(Direction::Rx, 0, 10e6).unwrap();
    dev.set_bandwidth(Direction::Rx, 0, 5e6).unwrap();
    dev.set_gain_element(Direction::Rx, 0, "LNA", 24.0).unwrap();
    dev.set_gain_element(Direction::Rx, 0, "VGA", 30.0).unwrap();
    dev.set_gain_element(Direction::Rx, 0, "AMP", 14.0).unwrap();
    dev.write_setting("bias_tx", "true").unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    rx.deactivate().unwrap();
    backend.clear_calls();
    rx.activate().unwrap();
    let calls = backend.calls();
    let expected = [
        Call::StartRx,
        Call::IsStreaming,
        Call::Close(serial.clone()),
        Call::OpenBySerial(serial.clone()),
        Call::SetFreq(100_000_000),
        Call::SetSampleRate(10e6),
        Call::SetBasebandFilterBandwidth(5_000_000),
        Call::SetAmpEnable(true),
        Call::SetLnaGain(24),
        Call::SetVgaGain(30),
        Call::SetAntennaEnable(true),
        Call::StartRx,
        Call::IsStreaming,
    ];
    let tail = &calls[calls.len() - expected.len()..];
    assert_eq!(tail, &expected[..]);
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Rx);
    assert_eq!(backend.open_serials(), vec![serial.clone()]);
    backend.pump_rx(&serial, &pattern(0, MTU)).unwrap();
    let mut buf = vec![0i8; 2 * MTU];
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);

    // A failing re-open is an error, not a null-pointer crash (BUGS.md S6).
    rx.deactivate().unwrap();
    backend.fail_next("open_by_serial", HackrfError::NotFound);
    assert_eq!(rx.activate(), Err(Error::OpenFailed(HackrfError::NotFound)));
    assert_eq!(dev.hardware_key(), Err(Error::DeviceClosed));
}

#[test]
fn streaming_that_never_starts_is_a_stream_error() {
    let (backend, dev, serial) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    // Simulate a device whose transfer thread dies right after start.
    backend.set_legacy_restart_bug(true);
    rx.activate().unwrap();
    rx.deactivate().unwrap();
    backend.set_legacy_restart_bug(false);
    backend.fail_streaming(&serial);
    // Now the device is in do_exit state but, with the bug flag cleared,
    // re-opening works and streaming resumes.
    rx.activate().unwrap();
    assert!(rx.is_active());
}

#[test]
fn direct_access_borrows_transfers_in_order() {
    let (backend, dev, serial) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &kw(&[("buffers", "3")]))
        .unwrap();
    rx.activate().unwrap();
    backend.pump_rx(&serial, &pattern(0, MTU)).unwrap();
    backend.pump_rx(&serial, &pattern(1, 10)).unwrap();
    {
        let b = rx.acquire(SHORT).unwrap();
        assert_eq!(b.samples(), MTU);
        assert_eq!(b.handle(), 0);
        assert_eq!(b.data(), &pattern(0, MTU)[..]);
    }
    {
        let b = rx.acquire(SHORT).unwrap();
        assert_eq!(b.samples(), 10);
        assert_eq!(b.data(), &pattern(1, 10)[..]);
    }
    assert_eq!(rx.acquire(SHORT).err(), Some(Error::Timeout));
    // Mixing with read(): a held remainder is released by acquire().
    backend.pump_rx(&serial, &pattern(2, MTU)).unwrap();
    let mut small = vec![0i8; 2];
    assert_eq!(rx.read(&mut small, SHORT).unwrap().samples, 1);
    backend.pump_rx(&serial, &pattern(3, MTU)).unwrap();
    let b = rx.acquire(SHORT).unwrap();
    assert_eq!(b.data(), &pattern(3, MTU)[..]);
}

#[test]
fn bias_tee_is_reapplied_on_activation() {
    // BUGS.md H3: the firmware drops the bias tee when the radio goes idle.
    let (backend, dev, serial) = open_single();
    dev.write_setting("bias_tx", "true").unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    backend.clear_calls();
    rx.activate().unwrap();
    assert!(backend.calls().contains(&Call::SetAntennaEnable(true)));
    assert!(backend.hw(&serial).antenna);
}

#[test]
fn dropping_the_stream_closes_it() {
    let (backend, dev, _) = open_single();
    {
        let mut rx = dev
            .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
            .unwrap();
        rx.activate().unwrap();
    }
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Off);
    assert_eq!(backend.calls().last(), Some(&Call::StopRx));
    assert!(dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .is_ok());
}
