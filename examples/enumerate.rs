//! List attached HackRF devices and their hardware info.

use soapyhackrf::{HackRfDevice, Kwargs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "libhackrf {} ({})",
        soapyhackrf::libhackrf::LibHackrf::library_version(),
        soapyhackrf::libhackrf::LibHackrf::library_release()
    );
    let found = HackRfDevice::enumerate_default(&Kwargs::new())?;
    if found.is_empty() {
        println!("no HackRF devices found");
        return Ok(());
    }
    for args in &found {
        println!("{}", args["label"]);
        for (k, v) in args {
            println!("  {k} = {v}");
        }
        let dev = HackRfDevice::open_default(args)?;
        println!("  hardware key: {}", dev.hardware_key()?);
        for (k, v) in dev.hardware_info()? {
            println!("  {k}: {v}");
        }
    }
    Ok(())
}
