//! Transmit a burst containing a CW tone: `tx_tone <freq_hz> <rate> <seconds>`.
//! Only run this with a dummy load or inside a shielded environment and with
//! the appropriate licence.

use std::time::Duration;

use soapyhackrf::{Direction, HackRfDevice, Kwargs, StreamFlags, StreamFormat};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: tx_tone <freq_hz> <sample_rate> <seconds>");
        std::process::exit(2);
    }
    let freq: f64 = args[1].parse()?;
    let rate: f64 = args[2].parse()?;
    let seconds: f64 = args[3].parse()?;

    let dev = HackRfDevice::open_default(&Kwargs::new())?;
    dev.set_sample_rate(Direction::Tx, 0, rate)?;
    dev.set_frequency(Direction::Tx, 0, "RF", freq, &Kwargs::new())?;
    dev.set_gain_element(Direction::Tx, 0, "VGA", 10.0)?;

    let total = (rate * seconds) as usize;
    let tone_hz = 100e3;
    let mut tx = dev.tx_stream(StreamFormat::CF32, &[0], &Kwargs::new())?;
    tx.activate_burst(total)?;

    let chunk = tx.mtu();
    let mut buf = vec![0f32; 2 * chunk];
    let mut sent = 0usize;
    while sent < total {
        let n = chunk.min(total - sent);
        for i in 0..n {
            let t = (sent + i) as f64 / rate;
            let phase = 2.0 * std::f64::consts::PI * tone_hz * t;
            buf[2 * i] = (0.5 * phase.cos()) as f32;
            buf[2 * i + 1] = (0.5 * phase.sin()) as f32;
        }
        let flags = if sent + n == total {
            StreamFlags::END_BURST
        } else {
            StreamFlags::NONE
        };
        let mut off = 0;
        while off < n {
            off += tx.write(&buf[2 * off..2 * n], flags, Duration::from_secs(1))?;
        }
        sent += n;
    }
    while !tx.burst_done() {
        std::thread::sleep(Duration::from_millis(10));
    }
    tx.deactivate()?;
    println!("sent {sent} samples");
    Ok(())
}
