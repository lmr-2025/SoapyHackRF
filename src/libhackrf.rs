//! The real backend: a thin safe wrapper over libhackrf (`ffi`).
//!
//! `hackrf_init`/`hackrf_exit` are reference counted process-wide, as the C++
//! `SoapyHackRFSession` did. Each [`LibDevice`] owns one `hackrf_device*`;
//! the RX/TX handlers are boxed and their address is passed as the libhackrf
//! context pointer, and they are kept alive until the matching stop call has
//! returned (libhackrf waits for all transfers to finish before returning
//! from `hackrf_stop_rx`/`hackrf_stop_tx`).

use std::ffi::{CStr, CString};
use std::os::raw::{c_int, c_void};
use std::sync::{Arc, Mutex, OnceLock};

use crate::backend::{Backend, DeviceHandle, ListedDevice, RxHandler, StreamingStatus, TxHandler};
use crate::error::HackrfError;
use crate::ffi;

static SESSIONS: Mutex<usize> = Mutex::new(0);
static SHARED: OnceLock<Arc<LibHackrf>> = OnceLock::new();

fn check(code: c_int) -> Result<(), HackrfError> {
    HackrfError::check(code)
}

/// The libhackrf backend (a zero-sized handle; use [`LibHackrf::shared`]).
#[derive(Debug, Default)]
pub struct LibHackrf;

impl LibHackrf {
    /// The process-wide backend instance.
    pub fn shared() -> Arc<LibHackrf> {
        Arc::clone(SHARED.get_or_init(|| Arc::new(LibHackrf)))
    }

    /// Version string of the linked libhackrf.
    pub fn library_version() -> String {
        // SAFETY: returns a static string.
        unsafe { CStr::from_ptr(ffi::hackrf_library_version()) }
            .to_string_lossy()
            .into_owned()
    }

    /// Release string of the linked libhackrf.
    pub fn library_release() -> String {
        // SAFETY: returns a static string.
        unsafe { CStr::from_ptr(ffi::hackrf_library_release()) }
            .to_string_lossy()
            .into_owned()
    }

    /// `hackrf_error_name` for a raw code.
    pub fn error_name(code: i32) -> String {
        // SAFETY: returns a static string for every code.
        unsafe { CStr::from_ptr(ffi::hackrf_error_name(code)) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Backend for LibHackrf {
    type Device = LibDevice;

    fn init(&self) -> Result<(), HackrfError> {
        let mut n = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        if *n == 0 {
            // SAFETY: plain library initialisation.
            check(unsafe { ffi::hackrf_init() })?;
        }
        *n += 1;
        Ok(())
    }

    fn exit(&self) -> Result<(), HackrfError> {
        let mut n = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        if *n == 0 {
            return Err(HackrfError::Other);
        }
        *n -= 1;
        if *n == 0 {
            // SAFETY: every device of this process has been closed first
            // (devices hold a Session).
            check(unsafe { ffi::hackrf_exit() })?;
        }
        Ok(())
    }

    fn list_devices(&self) -> Result<Vec<ListedDevice>, HackrfError> {
        // SAFETY: the list is owned by us until hackrf_device_list_free.
        unsafe {
            let list = ffi::hackrf_device_list();
            if list.is_null() {
                return Err(HackrfError::NoMem);
            }
            let count = (*list).devicecount.max(0) as usize;
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let serial_ptr = *(*list).serial_numbers.add(i);
                let usb_serial = if serial_ptr.is_null() {
                    None
                } else {
                    Some(CStr::from_ptr(serial_ptr).to_string_lossy().into_owned())
                };
                out.push(ListedDevice {
                    index: i,
                    usb_serial,
                    usb_board_id: *(*list).usb_board_ids.add(i) as u16,
                });
            }
            ffi::hackrf_device_list_free(list);
            Ok(out)
        }
    }

    fn open_listed(self: &Arc<Self>, index: usize) -> Result<LibDevice, HackrfError> {
        // SAFETY: as in list_devices; the device pointer is checked.
        unsafe {
            let list = ffi::hackrf_device_list();
            if list.is_null() {
                return Err(HackrfError::NoMem);
            }
            let mut dev: *mut ffi::hackrf_device = std::ptr::null_mut();
            let r = ffi::hackrf_device_list_open(list, index as c_int, &mut dev);
            ffi::hackrf_device_list_free(list);
            check(r)?;
            if dev.is_null() {
                return Err(HackrfError::NotFound);
            }
            Ok(LibDevice::new(dev))
        }
    }

    fn open_by_serial(self: &Arc<Self>, serial: &str) -> Result<LibDevice, HackrfError> {
        let c = CString::new(serial).map_err(|_| HackrfError::InvalidParam)?;
        let mut dev: *mut ffi::hackrf_device = std::ptr::null_mut();
        // SAFETY: valid C string and out-pointer.
        check(unsafe { ffi::hackrf_open_by_serial(c.as_ptr(), &mut dev) })?;
        if dev.is_null() {
            return Err(HackrfError::NotFound);
        }
        Ok(LibDevice::new(dev))
    }
}

/// An open libhackrf device.
pub struct LibDevice {
    dev: *mut ffi::hackrf_device,
    rx: Mutex<Option<Box<Arc<dyn RxHandler>>>>,
    tx: Mutex<Option<Box<Arc<dyn TxHandler>>>>,
}

// SAFETY: libhackrf serialises access to the device internally (libusb is
// thread safe); the handle boxes are protected by mutexes.
unsafe impl Send for LibDevice {}
unsafe impl Sync for LibDevice {}

impl LibDevice {
    fn new(dev: *mut ffi::hackrf_device) -> LibDevice {
        LibDevice {
            dev,
            rx: Mutex::new(None),
            tx: Mutex::new(None),
        }
    }

    /// The raw `hackrf_device*` for calls this crate does not wrap.
    pub fn raw(&self) -> *mut ffi::hackrf_device {
        self.dev
    }
}

impl Drop for LibDevice {
    fn drop(&mut self) {
        // SAFETY: owned handle; hackrf_close stops any streaming first.
        unsafe {
            ffi::hackrf_close(self.dev);
        }
    }
}

unsafe extern "C" fn rx_trampoline(transfer: *mut ffi::hackrf_transfer) -> c_int {
    // SAFETY: libhackrf passes the transfer we registered the context for;
    // the context is a live `Arc<dyn RxHandler>` boxed in `LibDevice::rx`.
    unsafe {
        let t = &*transfer;
        let handler = &*(t.rx_ctx as *const Arc<dyn RxHandler>);
        let len = t.valid_length.max(0) as usize;
        let data = std::slice::from_raw_parts(t.buffer as *const i8, len);
        if handler.on_rx(data) {
            0
        } else {
            -1
        }
    }
}

unsafe extern "C" fn tx_trampoline(transfer: *mut ffi::hackrf_transfer) -> c_int {
    // SAFETY: as in rx_trampoline, with `LibDevice::tx`.
    unsafe {
        let t = &mut *transfer;
        let handler = &*(t.tx_ctx as *const Arc<dyn TxHandler>);
        let len = t.buffer_length.max(0) as usize;
        let buf = std::slice::from_raw_parts_mut(t.buffer as *mut i8, len);
        let fill = handler.on_tx(buf);
        t.valid_length = fill.valid_len.min(len) as c_int;
        if fill.keep_streaming {
            0
        } else {
            -1
        }
    }
}

unsafe extern "C" fn flush_trampoline(ctx: *mut c_void, success: c_int) {
    // SAFETY: `ctx` is the same boxed handler as the TX context.
    unsafe {
        let handler = &*(ctx as *const Arc<dyn TxHandler>);
        handler.on_flush(success != 0);
    }
}

impl DeviceHandle for LibDevice {
    fn board_id(&self) -> Result<u8, HackrfError> {
        let mut id: u8 = 0xFF;
        // SAFETY: valid handle and out-pointer.
        check(unsafe { ffi::hackrf_board_id_read(self.dev, &mut id) })?;
        Ok(id)
    }

    fn version_string(&self) -> Result<String, HackrfError> {
        let mut buf = [0u8; 256];
        // SAFETY: libhackrf writes at most `length - 1` bytes plus a NUL.
        check(unsafe {
            ffi::hackrf_version_string_read(self.dev, buf.as_mut_ptr() as *mut _, 255)
        })?;
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Ok(String::from_utf8_lossy(&buf[..end]).into_owned())
    }

    fn board_partid_serialno(&self) -> Result<([u32; 2], [u32; 4]), HackrfError> {
        let mut r = ffi::read_partid_serialno_t::default();
        // SAFETY: valid handle and out-pointer.
        check(unsafe { ffi::hackrf_board_partid_serialno_read(self.dev, &mut r) })?;
        Ok((r.part_id, r.serial_no))
    }

    fn si5351c_read(&self, register: u16) -> Result<u16, HackrfError> {
        let mut v: u16 = 0;
        // SAFETY: valid handle and out-pointer.
        check(unsafe { ffi::hackrf_si5351c_read(self.dev, register, &mut v) })?;
        Ok(v)
    }

    fn set_antenna_enable(&self, enable: bool) -> Result<(), HackrfError> {
        // SAFETY: valid handle.
        check(unsafe { ffi::hackrf_set_antenna_enable(self.dev, enable as u8) })
    }

    fn set_lna_gain(&self, db: u32) -> Result<(), HackrfError> {
        // SAFETY: valid handle.
        check(unsafe { ffi::hackrf_set_lna_gain(self.dev, db) })
    }

    fn set_vga_gain(&self, db: u32) -> Result<(), HackrfError> {
        // SAFETY: valid handle.
        check(unsafe { ffi::hackrf_set_vga_gain(self.dev, db) })
    }

    fn set_txvga_gain(&self, db: u32) -> Result<(), HackrfError> {
        // SAFETY: valid handle.
        check(unsafe { ffi::hackrf_set_txvga_gain(self.dev, db) })
    }

    fn set_amp_enable(&self, enable: bool) -> Result<(), HackrfError> {
        // SAFETY: valid handle.
        check(unsafe { ffi::hackrf_set_amp_enable(self.dev, enable as u8) })
    }

    fn set_freq(&self, hz: u64) -> Result<(), HackrfError> {
        // SAFETY: valid handle.
        check(unsafe { ffi::hackrf_set_freq(self.dev, hz) })
    }

    fn set_sample_rate(&self, rate: f64) -> Result<(), HackrfError> {
        // SAFETY: valid handle.
        check(unsafe { ffi::hackrf_set_sample_rate(self.dev, rate) })
    }

    fn set_baseband_filter_bandwidth(&self, hz: u32) -> Result<(), HackrfError> {
        // SAFETY: valid handle.
        check(unsafe { ffi::hackrf_set_baseband_filter_bandwidth(self.dev, hz) })
    }

    fn start_rx(&self, handler: Arc<dyn RxHandler>) -> Result<(), HackrfError> {
        let mut slot = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        let boxed = Box::new(handler);
        let ctx = &*boxed as *const Arc<dyn RxHandler> as *mut c_void;
        // Keep the previous box alive until the new one is registered: a
        // stale callback from a stopped stream can no longer run because
        // hackrf_stop_rx waited for all transfers.
        *slot = Some(boxed);
        // SAFETY: `ctx` stays valid while `slot` holds the box.
        check(unsafe { ffi::hackrf_start_rx(self.dev, rx_trampoline, ctx) })
    }

    fn stop_rx(&self) -> Result<(), HackrfError> {
        // SAFETY: valid handle; returns after all transfers finished.
        let r = check(unsafe { ffi::hackrf_stop_rx(self.dev) });
        *self.rx.lock().unwrap_or_else(|e| e.into_inner()) = None;
        r
    }

    fn start_tx(&self, handler: Arc<dyn TxHandler>) -> Result<(), HackrfError> {
        let mut slot = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let boxed = Box::new(handler);
        let ctx = &*boxed as *const Arc<dyn TxHandler> as *mut c_void;
        *slot = Some(boxed);
        // SAFETY: `ctx` stays valid while `slot` holds the box.
        unsafe {
            check(ffi::hackrf_enable_tx_flush(self.dev, flush_trampoline, ctx))?;
            check(ffi::hackrf_start_tx(self.dev, tx_trampoline, ctx))
        }
    }

    fn stop_tx(&self) -> Result<(), HackrfError> {
        // SAFETY: valid handle; returns after all transfers finished.
        let r = check(unsafe { ffi::hackrf_stop_tx(self.dev) });
        *self.tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
        r
    }

    fn is_streaming(&self) -> StreamingStatus {
        // SAFETY: valid handle.
        StreamingStatus::from_code(unsafe { ffi::hackrf_is_streaming(self.dev) })
    }

    fn supports_tx_flush(&self) -> bool {
        true
    }
}
