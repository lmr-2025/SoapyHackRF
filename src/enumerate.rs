//! Device discovery (the C++ `find_HackRF` registration routine).
//!
//! Every attached HackRF is opened briefly to read its board id, firmware
//! version, part id and serial. Devices that cannot be opened (because this
//! or another process holds them) are reported from a process-wide cache of
//! earlier discoveries, keyed by serial, just like the C++ driver did.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::backend::{Backend, DeviceHandle, Session};
use crate::error::{Error, Result};
use crate::types::{format_part_id, format_serial, trimmed_serial, BoardId, Kwargs};

static CLAIMED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static CACHE: Mutex<BTreeMap<String, Kwargs>> = Mutex::new(BTreeMap::new());

fn claimed() -> MutexGuard<'static, BTreeSet<String>> {
    CLAIMED.lock().unwrap_or_else(|e| e.into_inner())
}

fn cache() -> MutexGuard<'static, BTreeMap<String, Kwargs>> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Serials of devices currently opened by this process through [`crate::HackRf`].
pub fn claimed_serials() -> Vec<String> {
    claimed().iter().cloned().collect()
}

pub(crate) fn claim_serial(serial: &str) {
    claimed().insert(serial.to_string());
}

pub(crate) fn release_serial(serial: &str) {
    claimed().remove(serial);
}

/// Forget cached discovery results (mainly for tests).
pub fn clear_discovery_cache() {
    cache().clear();
}

/// Argument key selecting a device by serial (full or suffix).
pub const ARG_SERIAL: &str = "serial";
/// Argument key selecting a device by enumeration index.
pub const ARG_INDEX: &str = "hackrf";

/// Describe an open device the way discovery reports it.
pub(crate) fn describe_device<D: DeviceHandle>(dev: &D, index: usize) -> Kwargs {
    let board = BoardId::from_u8(dev.board_id().unwrap_or(0xFF));
    let version = dev.version_string().unwrap_or_default();
    let (part_id, serial_no) = dev.board_partid_serialno().unwrap_or(([0; 2], [0; 4]));
    let serial = format_serial(serial_no);
    let mut options = Kwargs::new();
    options.insert("device".into(), board.name().into());
    options.insert("version".into(), version);
    options.insert("part_id".into(), format_part_id(part_id));
    options.insert(
        "label".into(),
        format!("{} #{} {}", board.name(), index, trimmed_serial(&serial)),
    );
    options.insert("serial".into(), serial);
    options
}

/// Discover attached devices, filtered by the optional `serial` and `hackrf`
/// (index) arguments. Returns one [`Kwargs`] per device with the keys
/// `device`, `version`, `part_id`, `serial` and `label`.
pub fn find_hackrf<B: Backend>(backend: &Arc<B>, args: &Kwargs) -> Result<Vec<Kwargs>> {
    let _session =
        Session::new(Arc::clone(backend)).map_err(|e| Error::hackrf("hackrf_init", e))?;

    let want_serial = args.get(ARG_SERIAL);
    let want_index = match args.get(ARG_INDEX) {
        Some(s) => Some(s.trim().parse::<usize>().map_err(|_| {
            Error::InvalidArgument(format!("hackrf index {s:?} is not a non-negative integer"))
        })?),
        None => None,
    };

    let list = backend
        .list_devices()
        .map_err(|e| Error::hackrf("hackrf_device_list", e))?;

    let mut results = Vec::new();
    for entry in &list {
        let dev = match B::open_listed(backend, entry.index) {
            Ok(dev) => dev,
            Err(_) => continue, // busy or gone: reported from the cache below
        };
        let options = describe_device(&dev, entry.index);
        drop(dev);

        let serial = options["serial"].clone();
        let serial_match = want_serial.map_or(true, |s| *s == serial);
        let index_match = want_index.map_or(true, |i| i == entry.index);
        if serial_match && index_match {
            cache().insert(serial, options.clone());
            results.push(options);
        }
    }

    // Devices this process already holds cannot be opened again; report the
    // cached description so they stay visible.
    for serial in claimed_serials() {
        if want_serial.is_some_and(|s| *s != serial) {
            continue;
        }
        if let Some(cached) = cache().get(&serial) {
            results.push(cached.clone());
        }
    }

    Ok(results)
}
