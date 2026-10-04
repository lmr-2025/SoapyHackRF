//! Producer/consumer threads driving the streams the way libhackrf's transfer
//! thread and an application thread would.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use common::{kw, open_single};
use soapyhackrf::*;

const MTU: usize = MTU_SAMPLES;

fn counter_block(start: u32) -> Vec<i8> {
    (0..MTU / 2)
        .flat_map(|i| (start + i as u32).to_le_bytes())
        .map(|b| b as i8)
        .collect()
}

fn counters(bytes: &[i8]) -> impl Iterator<Item = u32> + '_ {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0] as u8, c[1] as u8, c[2] as u8, c[3] as u8]))
}

#[test]
fn rx_data_is_contiguous_between_overflows() {
    let (backend, dev, serial) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &kw(&[("buffers", "6")]))
        .unwrap();
    rx.activate().unwrap();
    let transfers = 200u32;
    let per_block = (MTU / 2) as u32;
    let done = Arc::new(AtomicBool::new(false));
    let producer = {
        let backend = Arc::clone(&backend);
        let serial = serial.clone();
        let done = Arc::clone(&done);
        thread::spawn(move || {
            for i in 0..transfers {
                assert_eq!(
                    backend.pump_rx(&serial, &counter_block(i * per_block)),
                    Some(true)
                );
                if i % 7 == 0 {
                    thread::sleep(Duration::from_micros(200));
                }
            }
            done.store(true, Ordering::SeqCst);
        })
    };

    let mut expect: Option<u32> = None;
    let mut discontinuities = 0;
    let mut overflows = 0;
    let mut samples = 0usize;
    // An even number of samples keeps every read 4-byte aligned for `counters`.
    let mut buf = vec![0i8; 2 * 12_346];
    loop {
        match rx.read(&mut buf, Duration::from_millis(200)) {
            Ok(r) => {
                samples += r.samples;
                let n = r.samples * 2;
                let mut first = true;
                for c in counters(&buf[..n]) {
                    if let Some(e) = expect {
                        if c != e {
                            assert!(first, "discontinuity inside one read at {c} (expected {e})");
                            discontinuities += 1;
                        }
                    }
                    first = false;
                    expect = Some(c + 1);
                }
            }
            Err(Error::Overflow) => overflows += 1,
            Err(Error::Timeout) => {
                if done.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(e) => panic!("{e}"),
        }
    }
    producer.join().unwrap();
    assert!(samples > 0);
    assert!(
        discontinuities <= overflows,
        "{discontinuities} jumps but only {overflows} overflow reports"
    );
    assert_eq!(dev.stream_stats().0, transfers as u64);
}

#[test]
fn tx_data_arrives_in_order_and_complete() {
    let (backend, dev, serial) = open_single();
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &kw(&[("buffers", "4")]))
        .unwrap();
    tx.activate().unwrap();
    let blocks = 60u32;
    let per_block = (MTU / 2) as u32;
    let consumer = {
        let backend = Arc::clone(&backend);
        let serial = serial.clone();
        thread::spawn(move || {
            let mut got = Vec::new();
            let mut zero_blocks = 0;
            while got.len() < (blocks * per_block) as usize {
                let (buf, fill) = backend.pump_tx(&serial).expect("transmitting");
                assert!(fill.keep_streaming);
                if buf.iter().all(|&b| b == 0) {
                    zero_blocks += 1;
                    thread::sleep(Duration::from_micros(100));
                    continue;
                }
                assert_eq!(fill.valid_len, buf.len());
                got.extend(counters(&buf));
            }
            (got, zero_blocks)
        })
    };
    // Odd sized writes so that buffers are filled across several calls.
    let stream: Vec<i8> = (0..blocks)
        .flat_map(|b| counter_block(b * per_block + 1))
        .collect();
    let mut off = 0;
    while off < stream.len() {
        let end = (off + 2 * 30_001).min(stream.len());
        let n = tx
            .write(&stream[off..end], StreamFlags::NONE, Duration::from_secs(2))
            .unwrap();
        off += 2 * n;
    }
    let (got, zero_blocks) = consumer.join().unwrap();
    let expect: Vec<u32> = (1..=blocks * per_block).collect();
    assert_eq!(got, expect);
    let (_, _, tx_transfers, underflows) = dev.stream_stats();
    assert_eq!(underflows, zero_blocks);
    assert_eq!(tx_transfers, blocks as u64 + zero_blocks);
}

#[test]
fn settings_can_be_changed_while_streaming() {
    let (backend, dev, serial) = open_single();
    let dev = Arc::new(dev);
    let mut rx = dev
        .rx_stream(StreamFormat::CF32, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let tuner = {
        let dev = Arc::clone(&dev);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut f = 100e6;
            while !stop.load(Ordering::SeqCst) {
                dev.set_frequency(Direction::Rx, 0, "RF", f, &Kwargs::new())
                    .unwrap();
                dev.set_gain_element(Direction::Rx, 0, "VGA", 20.0).unwrap();
                f = if f > 6e9 { 100e6 } else { f + 1e6 };
            }
        })
    };
    let mut buf = vec![0f32; 2 * MTU];
    for i in 0..50u32 {
        backend.pump_rx(&serial, &counter_block(i)).unwrap();
        assert_eq!(
            rx.read(&mut buf, Duration::from_millis(500))
                .unwrap()
                .samples,
            MTU
        );
    }
    stop.store(true, Ordering::SeqCst);
    tuner.join().unwrap();
    assert!(backend.hw(&serial).freq >= 100_000_000);
}
