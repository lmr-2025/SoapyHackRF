//! Device discovery and opening.

mod common;

use std::sync::Arc;

use common::{fresh_info, kw};
use soapyhackrf::mock::{Call, MockBackend, MockDeviceInfo};
use soapyhackrf::{claimed_serials, find_hackrf, Error, HackRf, HackrfError, Kwargs};

/// Only the results belonging to this test's devices (other tests in the
/// same process may hold devices open, and those are reported from the
/// process-wide cache exactly like the C++ driver did).
fn mine(results: Vec<Kwargs>, infos: &[MockDeviceInfo]) -> Vec<Kwargs> {
    results
        .into_iter()
        .filter(|r| infos.iter().any(|i| i.serial() == r["serial"]))
        .collect()
}

#[test]
fn lists_devices_with_the_cpp_keys_and_label() {
    let a = fresh_info();
    let mut b = fresh_info();
    b.board_id = 4;
    b.version = "2021.03.1".into();
    b.part_id = [0x12345678, 0x9abcdef0];
    let backend = MockBackend::new(vec![a.clone(), b.clone()]);
    let found = mine(
        find_hackrf(&backend, &Kwargs::new()).unwrap(),
        &[a.clone(), b.clone()],
    );
    assert_eq!(found.len(), 2);
    assert_eq!(found[0]["device"], "HackRF One");
    assert_eq!(found[0]["version"], "2023.01.1");
    assert_eq!(found[0]["part_id"], "a000cb3c00514f4e");
    assert_eq!(found[0]["serial"], a.serial());
    assert_eq!(
        found[0]["label"],
        format!("HackRF One #0 {}", a.serial().trim_start_matches('0'))
    );
    assert_eq!(found[1]["device"], "HackRF One");
    assert_eq!(found[1]["version"], "2021.03.1");
    assert_eq!(found[1]["part_id"], "123456789abcdef0");
    assert!(found[1]["label"].starts_with("HackRF One #1 "));
    assert_eq!(backend.sessions(), 0, "discovery releases its session");
    assert!(
        backend.open_serials().is_empty(),
        "discovery closes every device"
    );
    let calls = backend.calls();
    assert_eq!(calls[0], Call::Init);
    assert_eq!(calls[1], Call::DeviceList);
    assert_eq!(calls[2], Call::OpenListed(0));
    assert_eq!(calls.last(), Some(&Call::Exit));
}

#[test]
fn filters_by_serial_and_index() {
    let a = fresh_info();
    let b = fresh_info();
    let backend = MockBackend::new(vec![a.clone(), b.clone()]);
    let infos = [a.clone(), b.clone()];

    let by_serial = mine(
        find_hackrf(&backend, &kw(&[("serial", &b.serial())])).unwrap(),
        &infos,
    );
    assert_eq!(by_serial.len(), 1);
    assert_eq!(by_serial[0]["serial"], b.serial());

    let by_index = mine(
        find_hackrf(&backend, &kw(&[("hackrf", "0")])).unwrap(),
        &infos,
    );
    assert_eq!(by_index.len(), 1);
    assert_eq!(by_index[0]["serial"], a.serial());

    let both = mine(
        find_hackrf(&backend, &kw(&[("hackrf", "1"), ("serial", &a.serial())])).unwrap(),
        &infos,
    );
    assert!(both.is_empty(), "index and serial must both match");

    let none = mine(
        find_hackrf(&backend, &kw(&[("hackrf", "7")])).unwrap(),
        &infos,
    );
    assert!(none.is_empty());

    assert!(matches!(
        find_hackrf(&backend, &kw(&[("hackrf", "zero")])),
        Err(Error::InvalidArgument(_))
    ));
    assert!(matches!(
        find_hackrf(&backend, &kw(&[("hackrf", "-1")])),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn devices_that_cannot_be_opened_are_skipped() {
    let a = fresh_info();
    let backend = MockBackend::new(vec![a.clone()]);
    backend.fail_next("open_listed", HackrfError::Busy);
    let found = mine(
        find_hackrf(&backend, &Kwargs::new()).unwrap(),
        std::slice::from_ref(&a),
    );
    assert!(found.is_empty());
    let again = mine(find_hackrf(&backend, &Kwargs::new()).unwrap(), &[a]);
    assert_eq!(again.len(), 1);
}

#[test]
fn devices_without_usb_serial_are_still_listed() {
    let mut a = fresh_info();
    a.has_usb_serial = false;
    let backend = MockBackend::new(vec![a.clone()]);
    let found = mine(
        find_hackrf(&backend, &Kwargs::new()).unwrap(),
        std::slice::from_ref(&a),
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["serial"], a.serial());
}

#[test]
fn claimed_devices_are_reported_from_the_cache() {
    let a = fresh_info();
    let backend = MockBackend::new(vec![a.clone()]);
    // Populate the cache.
    assert_eq!(
        mine(
            find_hackrf(&backend, &Kwargs::new()).unwrap(),
            std::slice::from_ref(&a)
        )
        .len(),
        1
    );

    let dev = HackRf::open(Arc::clone(&backend), &kw(&[("serial", &a.serial())])).unwrap();
    assert!(claimed_serials().contains(&a.serial()));
    assert_eq!(backend.sessions(), 1);

    backend.clear_calls();
    let found = mine(
        find_hackrf(&backend, &Kwargs::new()).unwrap(),
        std::slice::from_ref(&a),
    );
    assert_eq!(found.len(), 1, "open device is still visible");
    assert_eq!(found[0]["serial"], a.serial());
    assert!(
        !backend
            .calls()
            .iter()
            .any(|c| matches!(c, Call::BoardIdRead)),
        "the open device was not probed"
    );

    let other = mine(
        find_hackrf(&backend, &kw(&[("serial", "deadbeef")])).unwrap(),
        std::slice::from_ref(&a),
    );
    assert!(other.is_empty(), "cached entries honour the serial filter");

    drop(dev);
    assert!(!claimed_serials().contains(&a.serial()));
    assert_eq!(backend.sessions(), 0);
    assert!(backend.open_serials().is_empty());
}

#[test]
fn open_selects_a_device() {
    let a = fresh_info();
    let b = fresh_info();
    let backend = MockBackend::new(vec![a.clone(), b.clone()]);

    let dev = HackRf::open(Arc::clone(&backend), &Kwargs::new()).unwrap();
    assert_eq!(dev.serial(), a.serial(), "first device without arguments");
    drop(dev);

    let dev = HackRf::open(Arc::clone(&backend), &kw(&[("hackrf", "1")])).unwrap();
    assert_eq!(dev.serial(), b.serial());
    drop(dev);

    let suffix = &b.serial()[24..];
    let dev = HackRf::open(Arc::clone(&backend), &kw(&[("serial", suffix)])).unwrap();
    assert_eq!(
        dev.serial(),
        b.serial(),
        "opened by suffix, reported in full"
    );
    assert!(claimed_serials().contains(&b.serial()));
    let visible = find_hackrf(&backend, &kw(&[("serial", &b.serial())])).unwrap();
    assert_eq!(visible.len(), 1, "an open device stays discoverable");
    drop(dev);

    assert_eq!(
        HackRf::open(
            Arc::clone(&backend),
            &kw(&[("serial", "0000000000000000ffffffffffffffff")])
        )
        .err(),
        Some(Error::OpenFailed(HackrfError::NotFound))
    );
    assert_eq!(
        HackRf::open(Arc::clone(&backend), &kw(&[("hackrf", "5")])).err(),
        Some(Error::NoDeviceMatches)
    );

    let held = HackRf::open(Arc::clone(&backend), &kw(&[("serial", &a.serial())])).unwrap();
    assert_eq!(
        HackRf::open(Arc::clone(&backend), &kw(&[("serial", &a.serial())])).err(),
        Some(Error::OpenFailed(HackrfError::Busy))
    );
    drop(held);

    let empty = MockBackend::new(vec![]);
    assert_eq!(
        HackRf::open(empty, &Kwargs::new()).err(),
        Some(Error::NoDeviceMatches)
    );
}

#[test]
fn init_failure_is_reported() {
    let a = fresh_info();
    let backend = MockBackend::new(vec![a]);
    backend.fail_next("init", HackrfError::LibUsb);
    assert_eq!(
        find_hackrf(&backend, &Kwargs::new()).err(),
        Some(Error::hackrf("hackrf_init", HackrfError::LibUsb))
    );
    assert_eq!(backend.sessions(), 0);
}
