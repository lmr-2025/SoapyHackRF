//! Settings API against the mock backend.

mod common;

use common::{kw, open_single};
use soapyhackrf::mock::Call;
use soapyhackrf::*;

const RX: Direction = Direction::Rx;
const TX: Direction = Direction::Tx;

#[test]
fn identification() {
    let (backend, dev, serial) = open_single();
    assert_eq!(dev.driver_key(), "HackRF");
    assert_eq!(dev.hardware_key().unwrap(), "HackRF One");
    let info = dev.hardware_info().unwrap();
    assert_eq!(info["version"], "2023.01.1");
    assert_eq!(info["part id"], "a000cb3c00514f4e");
    assert_eq!(info["serial"], serial);
    assert_eq!(info["clock source"], "internal");
    assert_eq!(dev.num_channels(RX), 1);
    assert_eq!(dev.num_channels(TX), 1);
    assert!(!dev.full_duplex(RX, 0).unwrap());
    assert_eq!(dev.full_duplex(RX, 1), Err(Error::InvalidChannel(1)));
    assert!(backend.calls().contains(&Call::Si5351cRead(0)));
}

#[test]
fn external_clock_is_detected() {
    let mut info = common::fresh_info();
    info.si5351c_reg0 = 0x00;
    let serial = info.serial();
    let backend = soapyhackrf::mock::MockBackend::new(vec![info]);
    let dev = HackRf::open(backend, &kw(&[("serial", &serial)])).unwrap();
    assert_eq!(dev.hardware_info().unwrap()["clock source"], "external");
}

#[test]
fn hardware_read_errors_propagate() {
    let (backend, dev, _) = open_single();
    backend.fail_next("board_id_read", HackrfError::LibUsb);
    assert_eq!(
        dev.hardware_key(),
        Err(Error::hackrf("hackrf_board_id_read", HackrfError::LibUsb))
    );
    assert_eq!(dev.hardware_key().unwrap(), "HackRF One");
}

#[test]
fn stream_info() {
    let (_, dev, _) = open_single();
    assert_eq!(
        dev.stream_formats(RX, 0).unwrap(),
        vec![
            StreamFormat::CS8,
            StreamFormat::CS16,
            StreamFormat::CF32,
            StreamFormat::CF64
        ]
    );
    assert_eq!(
        dev.native_stream_format(TX, 0).unwrap(),
        (StreamFormat::CS8, 128.0)
    );
    let args = dev.stream_args_info(RX, 0).unwrap();
    assert_eq!(args.len(), 1);
    assert_eq!(args[0].key, "buffers");
    assert_eq!(args[0].value, "15");
    assert_eq!(args[0].arg_type, ArgType::Int);
}

#[test]
fn antenna_api() {
    let (_, dev, _) = open_single();
    assert_eq!(dev.list_antennas(RX, 0).unwrap(), vec!["TX/RX"]);
    assert_eq!(dev.antenna(TX, 0).unwrap(), "TX/RX");
    assert_eq!(dev.set_antenna(RX, 0, "TX/RX"), Ok(()));
    assert_eq!(
        dev.set_antenna(RX, 0, "RX2"),
        Err(Error::UnknownAntenna("RX2".into()))
    );
}

#[test]
fn bias_tee_setting() {
    let (backend, dev, serial) = open_single();
    let info = dev.setting_info();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].key, "bias_tx");
    assert_eq!(info[0].arg_type, ArgType::Bool);
    assert_eq!(dev.read_setting("bias_tx").unwrap(), "false");

    dev.write_setting("bias_tx", "true").unwrap();
    assert_eq!(dev.read_setting("bias_tx").unwrap(), "true");
    assert!(backend.hw(&serial).antenna);
    dev.write_setting("bias_tx", "0").unwrap();
    assert_eq!(dev.read_setting("bias_tx").unwrap(), "false");
    assert!(!backend.hw(&serial).antenna);
    dev.write_setting("bias_tx", " TRUE ").unwrap();
    assert!(backend.hw(&serial).antenna);

    assert!(matches!(
        dev.write_setting("bias_tx", "maybe"),
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(
        dev.write_setting("agc", "true"),
        Err(Error::UnknownSetting("agc".into()))
    );
    assert_eq!(
        dev.read_setting("agc"),
        Err(Error::UnknownSetting("agc".into()))
    );

    backend.fail_next("set_antenna_enable", HackrfError::UsbApiVersion);
    assert_eq!(
        dev.write_setting("bias_tx", "false"),
        Err(Error::hackrf(
            "hackrf_set_antenna_enable",
            HackrfError::UsbApiVersion
        ))
    );
}

#[test]
fn frequency_api() {
    let (backend, dev, serial) = open_single();
    assert_eq!(dev.list_frequencies(RX, 0).unwrap(), vec!["RF"]);
    assert_eq!(
        dev.frequency_range(RX, 0, "RF").unwrap(),
        vec![Range::new(0.0, 7.25e9, 0.0)]
    );
    assert_eq!(
        dev.frequency_range(RX, 0, "BB").unwrap(),
        vec![Range::new(0.0, 0.0, 0.0)]
    );
    assert!(dev.frequency_args_info(RX, 0).unwrap().is_empty());
    assert_eq!(dev.frequency(RX, 0, "RF").unwrap(), 0.0);

    dev.set_frequency(RX, 0, "RF", 100e6, &Kwargs::new())
        .unwrap();
    assert_eq!(backend.hw(&serial).freq, 100_000_000);
    assert_eq!(dev.frequency(RX, 0, "RF").unwrap(), 100e6);
    assert_eq!(dev.frequency(TX, 0, "RF").unwrap(), 0.0, "per direction");

    dev.set_frequency(RX, 0, "BB", 1e6, &Kwargs::new()).unwrap();
    assert_eq!(dev.frequency(RX, 0, "BB").unwrap(), 0.0);
    assert_eq!(
        dev.set_frequency(RX, 0, "LO", 1e6, &Kwargs::new()),
        Err(Error::UnknownFrequencyName("LO".into()))
    );
    assert_eq!(
        dev.frequency(RX, 0, "LO"),
        Err(Error::UnknownFrequencyName("LO".into()))
    );
    assert_eq!(
        dev.frequency_range(RX, 0, "LO"),
        Err(Error::UnknownFrequencyName("LO".into()))
    );
    for bad in [-1.0, f64::NAN, 7.26e9, f64::INFINITY] {
        assert!(
            matches!(
                dev.set_frequency(RX, 0, "RF", bad, &Kwargs::new()),
                Err(Error::InvalidArgument(_))
            ),
            "{bad}"
        );
    }
    assert_eq!(
        dev.frequency(RX, 0, "RF").unwrap(),
        100e6,
        "bad values leave state alone"
    );

    backend.fail_next("set_freq", HackrfError::LibUsb);
    assert_eq!(
        dev.set_frequency(RX, 0, "RF", 200e6, &Kwargs::new()),
        Err(Error::hackrf("hackrf_set_freq", HackrfError::LibUsb))
    );
}

#[test]
fn frequency_correction_retunes() {
    let (backend, dev, serial) = open_single();
    assert!(dev.has_frequency_correction(RX, 0).unwrap());
    assert!(!dev.has_dc_offset_mode(RX, 0).unwrap());
    assert_eq!(dev.frequency_correction(RX, 0).unwrap(), 0.0);

    dev.set_frequency_correction(RX, 0, 10.0).unwrap();
    assert!(
        !backend
            .calls()
            .iter()
            .any(|c| matches!(c, Call::SetFreq(_))),
        "nothing to retune before a frequency was set"
    );
    dev.set_frequency(RX, 0, "RF", 100e6, &Kwargs::new())
        .unwrap();
    assert_eq!(
        backend.hw(&serial).freq,
        100_001_000,
        "gr-osmosdr convention"
    );
    assert_eq!(
        dev.frequency(RX, 0, "RF").unwrap(),
        100e6,
        "reports the uncorrected request"
    );

    dev.set_frequency_correction(TX, 0, -20.0).unwrap();
    assert_eq!(
        backend.hw(&serial).freq,
        99_998_000,
        "shared by both directions"
    );
    assert_eq!(dev.frequency_correction(RX, 0).unwrap(), -20.0);
    assert!(matches!(
        dev.set_frequency_correction(RX, 0, f64::NAN),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn gain_elements_and_ranges() {
    let (_, dev, _) = open_single();
    assert_eq!(dev.list_gains(RX, 0).unwrap(), vec!["LNA", "AMP", "VGA"]);
    assert_eq!(dev.list_gains(TX, 0).unwrap(), vec!["VGA", "AMP"]);
    assert_eq!(
        dev.gain_element_range(RX, 0, "AMP").unwrap(),
        Range::new(0.0, 14.0, 14.0)
    );
    assert_eq!(
        dev.gain_element_range(RX, 0, "LNA").unwrap(),
        Range::new(0.0, 40.0, 8.0)
    );
    assert_eq!(
        dev.gain_element_range(RX, 0, "VGA").unwrap(),
        Range::new(0.0, 62.0, 2.0)
    );
    assert_eq!(
        dev.gain_element_range(TX, 0, "VGA").unwrap(),
        Range::new(0.0, 47.0, 1.0)
    );
    assert_eq!(
        dev.gain_element_range(TX, 0, "LNA"),
        Err(Error::UnknownGainName("LNA".into()))
    );
    assert_eq!(dev.gain_range(RX, 0).unwrap().maximum, 116.0);
    assert_eq!(dev.gain_range(TX, 0).unwrap().maximum, 61.0);
    assert!(!dev.gain_mode(RX, 0).unwrap());
    assert_eq!(dev.set_gain_mode(RX, 0, false), Ok(()));
    assert_eq!(dev.set_gain_mode(RX, 0, true), Err(Error::NotSupported));
    // Defaults from the C++ constructor.
    assert_eq!(dev.gain_element(RX, 0, "LNA").unwrap(), 16.0);
    assert_eq!(dev.gain_element(RX, 0, "VGA").unwrap(), 16.0);
    assert_eq!(dev.gain_element(RX, 0, "AMP").unwrap(), 0.0);
    assert_eq!(dev.gain_element(TX, 0, "VGA").unwrap(), 0.0);
    assert_eq!(dev.gain(RX, 0).unwrap(), 32.0);
}

#[test]
fn set_gain_element_quantises_and_applies() {
    let (backend, dev, serial) = open_single();
    dev.set_gain_element(RX, 0, "LNA", 35.0).unwrap();
    assert_eq!(
        dev.gain_element(RX, 0, "LNA").unwrap(),
        32.0,
        "reports the hardware value"
    );
    assert_eq!(backend.hw(&serial).lna, 32);
    dev.set_gain_element(RX, 0, "VGA", 63.0).unwrap();
    assert_eq!(dev.gain_element(RX, 0, "VGA").unwrap(), 62.0);
    assert_eq!(backend.hw(&serial).vga, 62);
    dev.set_gain_element(TX, 0, "VGA", 46.6).unwrap();
    assert_eq!(dev.gain_element(TX, 0, "VGA").unwrap(), 46.0);
    assert_eq!(backend.hw(&serial).txvga, 46);
    dev.set_gain_element(RX, 0, "AMP", 1.0).unwrap();
    assert_eq!(dev.gain_element(RX, 0, "AMP").unwrap(), 14.0);
    assert!(backend.hw(&serial).amp);
    dev.set_gain_element(RX, 0, "AMP", -3.0).unwrap();
    assert_eq!(dev.gain_element(RX, 0, "AMP").unwrap(), 0.0, "BUGS.md G5");
    assert!(!backend.hw(&serial).amp);
    assert_eq!(
        dev.set_gain_element(TX, 0, "LNA", 8.0),
        Err(Error::UnknownGainName("LNA".into()))
    );
    assert_eq!(
        dev.gain_element(TX, 0, "LNA"),
        Err(Error::UnknownGainName("LNA".into()))
    );
    assert!(
        !backend
            .calls()
            .iter()
            .any(|c| matches!(c, Call::SetLnaGain(v) if *v > 40)),
        "never sends values libhackrf rejects"
    );
}

#[test]
fn set_overall_gain_distributes() {
    let (backend, dev, serial) = open_single();
    dev.set_gain(RX, 0, 40.0).unwrap();
    let hw = backend.hw(&serial);
    assert_eq!(hw.lna + hw.vga, 40);
    assert!(!hw.amp);
    assert_eq!(dev.gain(RX, 0).unwrap(), 40.0);
    assert_eq!(dev.gain_element(RX, 0, "LNA").unwrap(), hw.lna as f64);
    assert_eq!(dev.gain_element(RX, 0, "VGA").unwrap(), hw.vga as f64);

    dev.set_gain(RX, 0, 116.0).unwrap();
    let hw = backend.hw(&serial);
    assert_eq!((hw.lna, hw.vga, hw.amp), (40, 62, true));
    assert_eq!(dev.gain(RX, 0).unwrap(), 116.0);

    dev.set_gain(TX, 0, 30.0).unwrap();
    let hw = backend.hw(&serial);
    assert_eq!((hw.txvga, hw.amp), (16, true));
    assert_eq!(dev.gain(TX, 0).unwrap(), 30.0);
    dev.set_gain(TX, 0, 0.0).unwrap();
    assert!(!backend.hw(&serial).amp);
    assert_eq!(
        dev.gain_element(RX, 0, "AMP").unwrap(),
        14.0,
        "RX keeps its own amp setting"
    );

    backend.fail_next("set_vga_gain", HackrfError::LibUsb);
    assert_eq!(
        dev.set_gain(RX, 0, 20.0),
        Err(Error::hackrf("hackrf_set_vga_gain", HackrfError::LibUsb))
    );
    assert_eq!(
        backend.hw(&serial).lna,
        8,
        "the other stages were still written"
    );
}

#[test]
fn sample_rate_api() {
    let (backend, dev, serial) = open_single();
    assert_eq!(dev.sample_rate(RX, 0).unwrap(), 0.0);
    assert_eq!(dev.list_sample_rates(RX, 0).unwrap().len(), 20);
    assert_eq!(dev.list_sample_rates(RX, 0).unwrap()[0], 1e6);
    assert_eq!(dev.list_sample_rates(RX, 0).unwrap()[19], 20e6);
    assert_eq!(
        dev.sample_rate_range(RX, 0).unwrap(),
        Range::new(1e6, 20e6, 0.0)
    );

    dev.set_sample_rate(RX, 0, 10e6).unwrap();
    assert_eq!(dev.sample_rate(RX, 0).unwrap(), 10e6);
    assert_eq!(dev.sample_rate(TX, 0).unwrap(), 0.0);
    let hw = backend.hw(&serial);
    assert_eq!(hw.sample_rate, 10e6);
    assert_eq!(
        hw.bandwidth, 7_000_000,
        "libhackrf picks the automatic filter"
    );
    assert_eq!(
        dev.bandwidth(RX, 0).unwrap(),
        7e6,
        "reported instead of 0 (BUGS.md B2)"
    );

    for bad in [0.0, -1.0, f64::NAN] {
        assert!(matches!(
            dev.set_sample_rate(RX, 0, bad),
            Err(Error::InvalidArgument(_))
        ));
    }
    backend.fail_next("set_sample_rate", HackrfError::LibUsb);
    assert_eq!(
        dev.set_sample_rate(RX, 0, 8e6),
        Err(Error::hackrf("hackrf_set_sample_rate", HackrfError::LibUsb))
    );
    assert_eq!(
        dev.sample_rate(RX, 0).unwrap(),
        10e6,
        "failed writes do not change state"
    );
}

#[test]
fn manual_bandwidth_survives_sample_rate_changes() {
    // BUGS.md B1: libhackrf resets the filter on every sample-rate change and
    // the C++ driver never re-applied the user's choice.
    let (backend, dev, serial) = open_single();
    assert_eq!(dev.list_bandwidths(RX, 0).unwrap().len(), 16);
    assert_eq!(dev.list_bandwidths(RX, 0).unwrap()[0], 1.75e6);
    assert_eq!(
        dev.bandwidth_range(RX, 0).unwrap(),
        Range::new(1.75e6, 28e6, 0.0)
    );
    assert_eq!(dev.bandwidth(RX, 0).unwrap(), 0.0);

    dev.set_bandwidth(RX, 0, 2.5e6).unwrap();
    assert_eq!(backend.hw(&serial).bandwidth, 2_500_000);
    assert_eq!(dev.bandwidth(RX, 0).unwrap(), 2.5e6);

    backend.clear_calls();
    dev.set_sample_rate(RX, 0, 10e6).unwrap();
    assert_eq!(
        backend.calls(),
        vec![
            Call::SetSampleRate(10e6),
            Call::SetBasebandFilterBandwidth(2_500_000)
        ]
    );
    assert_eq!(backend.hw(&serial).bandwidth, 2_500_000);

    dev.set_bandwidth(RX, 0, 0.0).unwrap();
    assert_eq!(
        backend.hw(&serial).bandwidth,
        7_000_000,
        "automatic filter restored"
    );
    assert_eq!(dev.bandwidth(RX, 0).unwrap(), 7e6);

    assert!(matches!(
        dev.set_bandwidth(RX, 0, -1.0),
        Err(Error::InvalidArgument(_))
    ));
    backend.fail_next("set_baseband_filter_bandwidth", HackrfError::InvalidParam);
    assert!(dev.set_bandwidth(RX, 0, 5e6).is_err());
    assert_eq!(
        dev.bandwidth(RX, 0).unwrap(),
        7e6,
        "failed writes do not change state"
    );
}

#[test]
fn settings_for_the_inactive_direction_are_deferred() {
    // BUGS.md H1: the C++ driver retuned the hardware immediately for either
    // direction, disturbing the one that was streaming.
    let (backend, dev, serial) = open_single();
    dev.set_frequency(RX, 0, "RF", 100e6, &Kwargs::new())
        .unwrap();
    dev.set_sample_rate(RX, 0, 8e6).unwrap();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    assert_eq!(dev.transceiver_mode(), TransceiverMode::Rx);
    backend.clear_calls();

    dev.set_frequency(TX, 0, "RF", 200e6, &Kwargs::new())
        .unwrap();
    dev.set_sample_rate(TX, 0, 10e6).unwrap();
    dev.set_bandwidth(TX, 0, 5e6).unwrap();
    dev.set_gain_element(TX, 0, "AMP", 14.0).unwrap();
    dev.set_gain_element(TX, 0, "VGA", 20.0).unwrap();
    assert_eq!(
        backend.calls(),
        vec![Call::SetTxVgaGain(20)],
        "only the independent TX VGA register is written while receiving"
    );
    assert_eq!(backend.hw(&serial).freq, 100_000_000);
    assert_eq!(dev.frequency(TX, 0, "RF").unwrap(), 200e6);
    assert_eq!(dev.bandwidth(TX, 0).unwrap(), 5e6);

    // RX settings still apply immediately.
    dev.set_frequency(RX, 0, "RF", 101e6, &Kwargs::new())
        .unwrap();
    assert_eq!(backend.hw(&serial).freq, 101_000_000);

    drop(rx);
    // Switching to TX applies the deferred settings.
    let mut tx = dev
        .tx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    tx.activate().unwrap();
    let hw = backend.hw(&serial);
    assert_eq!(hw.freq, 200_000_000);
    assert_eq!(hw.sample_rate, 10e6);
    assert_eq!(hw.bandwidth, 5_000_000);
    assert!(hw.amp);
    assert_eq!(hw.txvga, 20);
}

#[test]
fn drop_closes_the_device_and_releases_the_claim() {
    let (backend, dev, serial) = open_single();
    assert_eq!(backend.open_serials(), vec![serial.clone()]);
    assert_eq!(backend.sessions(), 1);
    drop(dev);
    assert!(backend.open_serials().is_empty());
    assert_eq!(backend.sessions(), 0);
    assert!(!claimed_serials().contains(&serial));
    let calls = backend.calls();
    let close = calls
        .iter()
        .position(|c| *c == Call::Close(serial.clone()))
        .unwrap();
    let exit = calls.iter().position(|c| *c == Call::Exit).unwrap();
    assert!(close < exit, "device closed before hackrf_exit");
}

#[test]
fn drop_while_streaming_stops_first() {
    let (backend, dev, serial) = open_single();
    let mut rx = dev
        .rx_stream(StreamFormat::CS8, &[0], &Kwargs::new())
        .unwrap();
    rx.activate().unwrap();
    std::mem::forget(rx); // simulate a leaked stream handle
    drop(dev);
    let calls = backend.calls();
    let stop = calls.iter().position(|c| *c == Call::StopRx).unwrap();
    let close = calls
        .iter()
        .position(|c| *c == Call::Close(serial.clone()))
        .unwrap();
    assert!(stop < close);
}
