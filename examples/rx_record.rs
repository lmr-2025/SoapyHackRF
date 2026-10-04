//! Record CS8 samples to a file: `rx_record <freq_hz> <rate> <seconds> <out.cs8>`.

use std::io::Write;
use std::time::{Duration, Instant};

use soapyhackrf::{Direction, Error, HackRfDevice, Kwargs, StreamFormat};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: rx_record <freq_hz> <sample_rate> <seconds> <out.cs8>");
        std::process::exit(2);
    }
    let freq: f64 = args[1].parse()?;
    let rate: f64 = args[2].parse()?;
    let seconds: f64 = args[3].parse()?;
    let mut out = std::fs::File::create(&args[4])?;

    let dev = HackRfDevice::open_default(&Kwargs::new())?;
    dev.set_sample_rate(Direction::Rx, 0, rate)?;
    dev.set_frequency(Direction::Rx, 0, "RF", freq, &Kwargs::new())?;
    dev.set_gain_element(Direction::Rx, 0, "LNA", 16.0)?;
    dev.set_gain_element(Direction::Rx, 0, "VGA", 20.0)?;

    let mut rx = dev.rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())?;
    rx.activate()?;
    let mut buf = vec![0i8; 2 * rx.mtu()];
    let start = Instant::now();
    let mut total = 0usize;
    let mut overflows = 0usize;
    while start.elapsed().as_secs_f64() < seconds {
        match rx.read(&mut buf, Duration::from_millis(500)) {
            Ok(r) => {
                let bytes: &[u8] =
                    unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, r.samples * 2) };
                out.write_all(bytes)?;
                total += r.samples;
            }
            Err(Error::Overflow) => overflows += 1,
            Err(Error::Timeout) => eprintln!("timeout"),
            Err(e) => return Err(e.into()),
        }
    }
    rx.deactivate()?;
    println!("wrote {total} samples, {overflows} overflows");
    Ok(())
}
