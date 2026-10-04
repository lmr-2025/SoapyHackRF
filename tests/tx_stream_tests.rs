//! TX streaming, bursts and half-duplex switching against the mock backend.

mod common;

use std::thread;
use std::time::{Duration, Instant};

use common::{kw, open_single, pattern};
use soapyhackrf::mock::Call;
use soapyhackrf::*;

const SHORT: Duration = Duration::from_millis(20);
const MTU: usize = MTU_SAMPLES;
const BLOCK: usize = BUF_LEN;

#[test]
fn write_activates_and_fills_whole_transfers() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    assert!(!tx.is_active());
    assert_eq!(tx.mtu(), MTU);
    backend.clear_calls();
    let data = pattern(9, MTU);
    assert_eq!(tx.write(&data, StreamFlags::NONE, SHORT).unwrap(), MTU);
    assert!(tx.is_active());
    assert!(backend.calls().contains(&Call::StartTx));
    let (buf, fill) = backend.pump_tx(&serial).unwrap();
    assert_eq!(
        fill,
        soapyhackrf::backend::TxFill {
            valid_len: BLOCK,
            keep_streaming: true
        }
    );
    assert_eq!(buf, data);
    // Nothing queued: zeros and an underflow.
    let (buf, fill) = backend.pump_tx(&serial).unwrap();
    assert!(fill.keep_streaming);
    assert!(buf.iter().all(|&b| b == 0));
    assert_eq!(tx.read_status(SHORT), Ok(StreamEvent::Underflow));
    assert_eq!(tx.read_status(SHORT), Err(Error::Timeout));
    assert_eq!(dev.stream_stats().3, 1);
}

#[test]
fn write_converts_every_format() {
    let (backend, dev, serial) = open_single();
    let native = pattern(3, MTU);

    let mut tx = dev
        .tx_stream(StreamFormat::CS16, &[0], &Kwargs::new())
        .unwrap();
    let cs16: Vec<i16> = native.iter().map(|&v| (v as i16) << 8).collect();
    assert_eq!(tx.write(&cs16, StreamFlags::NONE, SHORT).unwrap(), MTU);
    assert_eq!(backend.pump_tx(&serial).unwrap().0, native);
    drop(tx);

    let mut tx = dev
        .tx_stream(StreamFormat::CF32, &[0], &Kwargs::new())
        .unwrap();
    let cf32: Vec<f32> = native.iter().map(|&v| (v as f64 / 127.0) as f32).collect();
    assert_eq!(tx.write(&cf32, StreamFlags::NONE, SHORT).unwrap(), MTU);
    let got = backend.pump_tx(&serial).unwrap().0;
    assert!(got.iter().zip(&native).all(|(&g, &n)| g == n || n == -128));
    drop(tx);

    let mut tx = dev
        .tx_stream(StreamFormat::CF64, &[0], &Kwargs::new())
        .unwrap();
    let cf64: Vec<f64> = native.iter().map(|&v| v as f64 / 127.0).collect();
    assert_eq!(tx.write(&cf64, StreamFlags::NONE, SHORT).unwrap(), MTU);
    let got = backend.pump_tx(&serial).unwrap().0;
    assert!(got.iter().zip(&native).all(|(&g, &n)| g == n || n == -128));
    drop(tx);

    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    assert_eq!(
        tx.write(&cf32, StreamFlags::NONE, SHORT).err(),
        Some(Error::FormatMismatch {
            expected: StreamFormat::CS8,
            actual: StreamFormat::CF32
        })
    );
}

#[test]
fn partial_writes_accumulate_until_a_transfer_is_full() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.activate().unwrap();
    let data = pattern(0, MTU + 500);
    let chunk = 1000;
    let mut off = 0;
    let mut writes = 0;
    while off < MTU {
        let end = (off + chunk).min(MTU);
        let n = tx
            .write(&data[2 * off..2 * end], StreamFlags::NONE, SHORT)
            .unwrap();
        assert_eq!(n, end - off);
        off = end;
        writes += 1;
        if off < MTU {
            let (_, fill) = backend.pump_tx(&serial).unwrap();
            assert_eq!(fill.valid_len, BLOCK);
            assert_eq!(
                dev.stream_stats().3 as usize,
                writes,
                "underflow until full"
            );
        }
    }
    let (buf, _) = backend.pump_tx(&serial).unwrap();
    assert_eq!(buf, &data[..2 * MTU]);
    // The 500 extra samples sit in the next buffer.
    assert_eq!(
        tx.write(&data[2 * MTU..], StreamFlags::NONE, SHORT)
            .unwrap(),
        500
    );
    assert!(
        backend.pump_tx(&serial).unwrap().0.iter().all(|&b| b == 0),
        "not full yet"
    );
    // Writes larger than the MTU are capped.
    let big = pattern(0, 3 * MTU);
    assert_eq!(tx.write(&big, StreamFlags::NONE, SHORT).unwrap(), MTU);
}

#[test]
fn end_burst_flushes_the_partial_buffer_and_stops() {
    // BUGS.md T1/T2: the C++ driver never transmitted a partially filled
    // final buffer and dropped the buffer in which it returned -1.
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    let data = pattern(11, 1000);
    assert_eq!(
        tx.write(&data, StreamFlags::END_BURST, SHORT).unwrap(),
        1000
    );
    assert!(!tx.burst_done());
    let (buf, fill) = backend.pump_tx(&serial).unwrap();
    assert_eq!(fill.valid_len, 2000);
    assert!(
        fill.keep_streaming,
        "the buffer with the last samples is transmitted"
    );
    assert_eq!(&buf[..2000], &data[..]);
    assert!(buf[2000..].iter().all(|&b| b == 0));
    let (_, fill) = backend.pump_tx(&serial).unwrap();
    assert_eq!(fill.valid_len, 0);
    assert!(!fill.keep_streaming, "then the stream stops");
    assert!(tx.burst_done());
    assert!(!backend.hw(&serial).streaming);
    assert_eq!(backend.pump_tx(&serial), None);
    assert_eq!(dev.stream_stats().3, 0, "no underflow during the burst");
}

#[test]
fn declared_burst_of_an_exact_mtu_multiple_terminates() {
    // BUGS.md T3: with numElems a multiple of the MTU the C++ callback never
    // saw burst_samps go negative and streamed zeros forever.
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Off);
    // BUGS.md T4: bursts were only set up when switching from RX.
    tx.activate_burst(2 * MTU).unwrap();
    let a = pattern(0, MTU);
    let b = pattern(1, MTU);
    assert_eq!(tx.write(&a, StreamFlags::NONE, SHORT).unwrap(), MTU);
    assert_eq!(tx.write(&b, StreamFlags::NONE, SHORT).unwrap(), MTU);
    assert_eq!(backend.pump_tx(&serial).unwrap().0, a);
    let (buf, fill) = backend.pump_tx(&serial).unwrap();
    assert_eq!(buf, b);
    assert!(fill.keep_streaming);
    let (_, fill) = backend.pump_tx(&serial).unwrap();
    assert!(!fill.keep_streaming);
    assert!(tx.burst_done());
    assert_eq!(backend.hw(&serial).tx_transfers, 3);
}

#[test]
fn declared_burst_with_a_partial_last_buffer() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    let total = MTU + MTU / 2;
    tx.activate_burst(total).unwrap();
    let data = pattern(0, total);
    let mut off = 0;
    while off < total {
        let end = (off + 50_000).min(total);
        off += tx
            .write(&data[2 * off..2 * end], StreamFlags::NONE, SHORT)
            .unwrap();
    }
    assert_eq!(backend.pump_tx(&serial).unwrap().0, &data[..2 * MTU]);
    let (buf, fill) = backend.pump_tx(&serial).unwrap();
    assert_eq!(fill.valid_len, MTU, "half an MTU of samples = MTU bytes");
    assert_eq!(&buf[..MTU], &data[2 * MTU..]);
    assert!(fill.keep_streaming);
    assert!(!backend.pump_tx(&serial).unwrap().1.keep_streaming);
    assert!(tx.burst_done());
}

#[test]
fn a_new_burst_after_a_finished_one_restarts_the_stream() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.write(&pattern(0, 10), StreamFlags::END_BURST, SHORT)
        .unwrap();
    backend.pump_tx(&serial).unwrap();
    backend.pump_tx(&serial).unwrap();
    assert!(tx.burst_done());
    backend.clear_calls();
    tx.write(&pattern(1, 10), StreamFlags::END_BURST, SHORT)
        .unwrap();
    assert_eq!(backend.calls()[..2], [Call::StopTx, Call::StartTx]);
    let (buf, fill) = backend.pump_tx(&serial).unwrap();
    assert_eq!(fill.valid_len, 20);
    assert_eq!(&buf[..20], &pattern(1, 10)[..]);
    assert!(!backend.pump_tx(&serial).unwrap().1.keep_streaming);
}

#[test]
fn switching_to_rx_waits_for_the_burst_and_the_flush() {
    let (backend, dev, serial) = open_single();
    backend.set_auto_flush(false);
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.write(&pattern(0, MTU), StreamFlags::END_BURST, SHORT)
        .unwrap();

    let pumper = {
        let backend = std::sync::Arc::clone(&backend);
        let serial = serial.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            backend.pump_tx(&serial).unwrap(); // the data
            thread::sleep(Duration::from_millis(50));
            backend.pump_tx(&serial).unwrap(); // stop
            thread::sleep(Duration::from_millis(50));
            backend.complete_flush(&serial, true);
        })
    };
    let t = Instant::now();
    rx.activate().unwrap();
    let elapsed = t.elapsed();
    pumper.join().unwrap();
    assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(900), "{elapsed:?}");
    let calls = backend.calls();
    let stop = calls.iter().rposition(|c| *c == Call::StopTx).unwrap();
    let start = calls.iter().rposition(|c| *c == Call::StartRx).unwrap();
    assert!(stop < start);
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Rx);
    assert_eq!(
        backend.hw(&serial).tx_transfers,
        2,
        "nothing was cancelled early"
    );
}

#[test]
fn switching_to_rx_without_flush_support_still_works() {
    let (backend, dev, serial) = open_single();
    backend.set_tx_flush_supported(false);
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.write(&pattern(0, 100), StreamFlags::END_BURST, SHORT)
        .unwrap();
    backend.pump_tx(&serial).unwrap();
    backend.pump_tx(&serial).unwrap();
    let t = Instant::now();
    rx.activate().unwrap();
    assert!(t.elapsed() < Duration::from_millis(500));
    assert!(rx.is_active());
}

#[test]
fn switching_resyncs_each_directions_settings() {
    let (backend, dev, serial) = open_single();
    dev.set_frequency(Direction::Rx, 0, "RF", 100e6, &Kwargs::new())
        .unwrap();
    dev.set_frequency(Direction::Tx, 0, "RF", 200e6, &Kwargs::new())
        .unwrap();
    dev.set_sample_rate(Direction::Rx, 0, 8e6).unwrap();
    dev.set_sample_rate(Direction::Tx, 0, 10e6).unwrap();
    dev.set_bandwidth(Direction::Rx, 0, 3.5e6).unwrap();
    dev.set_gain_element(Direction::Rx, 0, "AMP", 14.0).unwrap();
    dev.set_gain_element(Direction::Tx, 0, "AMP", 0.0).unwrap();
    // Idle: the last write wins on the hardware (as in C++).
    let hw = backend.hw(&serial);
    assert_eq!(
        (hw.freq, hw.sample_rate, hw.amp),
        (200_000_000, 10e6, false)
    );

    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    // BUGS.md H2: the C++ driver only re-synced when coming from the other
    // mode, so activating RX from idle here would have run at 200 MHz.
    rx.activate().unwrap();
    let hw = backend.hw(&serial);
    assert_eq!(
        (hw.freq, hw.sample_rate, hw.bandwidth, hw.amp),
        (100_000_000, 8e6, 3_500_000, true)
    );

    tx.activate().unwrap();
    let hw = backend.hw(&serial);
    assert_eq!(hw.mode, TransceiverMode::Tx);
    assert_eq!(
        (hw.freq, hw.sample_rate, hw.amp),
        (200_000_000, 10e6, false)
    );
    assert_eq!(hw.bandwidth, 7_000_000, "TX uses the automatic filter");

    rx.activate().unwrap();
    let hw = backend.hw(&serial);
    assert_eq!(
        (hw.freq, hw.sample_rate, hw.bandwidth, hw.amp),
        (100_000_000, 8e6, 3_500_000, true)
    );
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Rx);
}

#[test]
fn tx_reopen_path_uses_the_tx_amp_setting() {
    // BUGS.md S7: the C++ TX re-open path copied `_rx_stream.amp_gain`.
    let (backend, dev, serial) = open_single();
    backend.set_legacy_restart_bug(true);
    dev.set_gain_element(Direction::Rx, 0, "AMP", 14.0).unwrap();
    dev.set_gain_element(Direction::Tx, 0, "AMP", 0.0).unwrap();
    dev.set_gain_element(Direction::Tx, 0, "VGA", 30.0).unwrap();
    dev.set_frequency(Direction::Tx, 0, "RF", 433e6, &Kwargs::new())
        .unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    assert!(backend.hw(&serial).amp);
    backend.clear_calls();
    tx.activate().unwrap();
    let calls = backend.calls();
    assert!(
        calls.contains(&Call::OpenBySerial(serial.clone())),
        "{calls:?}"
    );
    let hw = backend.hw(&serial);
    assert!(!hw.amp);
    assert_eq!(hw.txvga, 30);
    assert_eq!(hw.freq, 433_000_000);
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Tx);
}

#[test]
fn write_times_out_when_the_ring_is_full() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &kw(&[("buffers", "2")]))
        .unwrap();
    let data = pattern(0, MTU);
    assert_eq!(tx.write(&data, StreamFlags::NONE, SHORT).unwrap(), MTU);
    assert_eq!(tx.write(&data, StreamFlags::NONE, SHORT).unwrap(), MTU);
    let t = Instant::now();
    assert_eq!(
        tx.write(&data, StreamFlags::NONE, SHORT).err(),
        Some(Error::Timeout)
    );
    assert!(t.elapsed() < Duration::from_millis(500));
    backend.pump_tx(&serial).unwrap();
    assert_eq!(tx.write(&data, StreamFlags::NONE, SHORT).unwrap(), MTU);
    // A partial write followed by a full ring returns what it took.
    assert_eq!(
        tx.write(&data[..2 * 100], StreamFlags::NONE, SHORT).err(),
        Some(Error::Timeout)
    );
    backend.pump_tx(&serial).unwrap();
    assert_eq!(
        tx.write(&data[..2 * 100], StreamFlags::NONE, SHORT)
            .unwrap(),
        100
    );
}

#[test]
fn direct_access_submit_and_cancel() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &kw(&[("buffers", "2")]))
        .unwrap();
    tx.activate().unwrap();
    {
        let mut b = tx.acquire(SHORT).unwrap();
        assert_eq!(b.capacity(), MTU);
        assert_eq!(b.handle(), 0);
        b.data()[..4].copy_from_slice(&[1, 2, 3, 4]);
        b.submit(2, StreamFlags::NONE);
    }
    {
        let _unused = tx.acquire(SHORT).unwrap();
        // dropped without submit
    }
    let (buf, fill) = backend.pump_tx(&serial).unwrap();
    assert_eq!(fill.valid_len, 4);
    assert_eq!(&buf[..4], &[1, 2, 3, 4]);
    assert!(
        backend.pump_tx(&serial).unwrap().0.iter().all(|&b| b == 0),
        "cancelled slot not sent"
    );
    // Two outstanding fills are possible one after the other.
    for i in 0..2 {
        let mut b = tx.acquire(SHORT).unwrap();
        b.data()[0] = i;
        b.submit(
            1,
            if i == 1 {
                StreamFlags::END_BURST
            } else {
                StreamFlags::NONE
            },
        );
    }
    assert_eq!(tx.acquire(SHORT).err(), Some(Error::Timeout), "ring full");
    assert_eq!(backend.pump_tx(&serial).unwrap().0[0], 0);
    assert_eq!(backend.pump_tx(&serial).unwrap().0[0], 1);
    assert!(!backend.pump_tx(&serial).unwrap().1.keep_streaming);
}

#[test]
fn deactivate_keeps_queued_samples() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    let data = pattern(4, MTU);
    tx.write(&data, StreamFlags::NONE, SHORT).unwrap();
    tx.deactivate().unwrap();
    assert_eq!(backend.calls().last(), Some(&Call::StopTx));
    assert_eq!(backend.pump_tx(&serial), None);
    tx.deactivate().unwrap();
    tx.activate().unwrap();
    assert_eq!(backend.pump_tx(&serial).unwrap().0, data);
}

#[test]
fn rx_read_waits_for_queued_tx_data_before_switching() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.write(&pattern(0, MTU), StreamFlags::NONE, SHORT)
        .unwrap();
    let mut buf = vec![0i8; 2 * MTU];
    assert_eq!(rx.read(&mut buf, SHORT).err(), Some(Error::Timeout));
    assert_eq!(
        dev.transceiver_mode(),
        TransceiverMode::Tx,
        "not switched while TX data is queued"
    );
    backend.pump_tx(&serial).unwrap();
    assert_eq!(rx.read(&mut buf, SHORT).err(), Some(Error::Timeout));
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Rx);
    backend.pump_rx(&serial, &pattern(1, MTU)).unwrap();
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);
}

#[test]
fn start_failure_leaves_the_radio_idle() {
    let (backend, dev, _) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    backend.fail_next("start_tx", HackrfError::LibUsb);
    assert_eq!(
        tx.write(&pattern(0, 10), StreamFlags::NONE, SHORT),
        Err(Error::hackrf("hackrf_start_tx", HackrfError::LibUsb))
    );
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Off);
}

#[test]
fn closing_discards_unsent_samples() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.write(&pattern(0, 10), StreamFlags::NONE, SHORT).unwrap();
    tx.close();
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Off);
    assert_eq!(backend.pump_tx(&serial), None);
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.activate().unwrap();
    assert!(backend.pump_tx(&serial).unwrap().0.iter().all(|&b| b == 0));
}

#[test]
fn activate_burst_after_a_finished_burst_restarts_the_stream() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.activate_burst(10).unwrap();
    tx.write(&pattern(0, 10), StreamFlags::NONE, SHORT).unwrap();
    backend.pump_tx(&serial).unwrap();
    assert!(!backend.pump_tx(&serial).unwrap().1.keep_streaming);
    assert!(tx.burst_done());
    backend.clear_calls();
    tx.activate_burst(5).unwrap();
    assert_eq!(backend.calls()[..2], [Call::StopTx, Call::StartTx]);
    assert!(!tx.burst_done());
    tx.write(&pattern(1, 5), StreamFlags::NONE, SHORT).unwrap();
    let (buf, fill) = backend.pump_tx(&serial).unwrap();
    assert_eq!(fill.valid_len, 10);
    assert_eq!(&buf[..10], &pattern(1, 5)[..]);
    assert!(!backend.pump_tx(&serial).unwrap().1.keep_streaming);
}

#[test]
fn end_burst_does_not_undo_a_burst_the_callback_already_finished() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.activate_burst(MTU).unwrap();
    // Fill the whole burst through the direct-access API so the callback can
    // run before the stream object learns the burst target was reached.
    {
        let mut b = tx.acquire(SHORT).unwrap();
        b.data()[0] = 7;
        b.submit(MTU, StreamFlags::NONE);
    }
    backend.pump_tx(&serial).unwrap();
    assert!(!backend.pump_tx(&serial).unwrap().1.keep_streaming);
    assert!(tx.burst_done());
    // A late END_BURST with nothing queued must leave the finished state.
    assert_eq!(
        tx.write(&pattern(0, 0), StreamFlags::END_BURST, SHORT)
            .unwrap(),
        0
    );
    assert!(tx.burst_done());
}

#[test]
fn rx_read_after_tx_deactivate_switches_despite_queued_samples() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.write(&pattern(0, MTU), StreamFlags::NONE, SHORT)
        .unwrap();
    tx.deactivate().unwrap();
    let mut buf = vec![0i8; 2 * MTU];
    assert_eq!(rx.read(&mut buf, SHORT).err(), Some(Error::Timeout));
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Rx);
    backend.pump_rx(&serial, &pattern(1, MTU)).unwrap();
    assert_eq!(rx.read(&mut buf, SHORT).unwrap().samples, MTU);
}
