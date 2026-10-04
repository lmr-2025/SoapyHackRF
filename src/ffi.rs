//! Raw `extern "C"` declarations for the subset of libhackrf the driver uses.
//!
//! Hand written from `hackrf.h` (libhackrf 2023.01.1) to avoid a build-time
//! dependency on bindgen. Layouts follow the C definitions exactly.

#![allow(non_camel_case_types, missing_docs)]

use std::os::raw::{c_char, c_int, c_void};

/// Opaque device handle.
#[repr(C)]
pub struct hackrf_device {
    _private: [u8; 0],
}

/// `hackrf_transfer`
#[repr(C)]
pub struct hackrf_transfer {
    pub device: *mut hackrf_device,
    pub buffer: *mut u8,
    pub buffer_length: c_int,
    pub valid_length: c_int,
    pub rx_ctx: *mut c_void,
    pub tx_ctx: *mut c_void,
}

/// `read_partid_serialno_t`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct read_partid_serialno_t {
    pub part_id: [u32; 2],
    pub serial_no: [u32; 4],
}

/// `hackrf_device_list_t`
#[repr(C)]
pub struct hackrf_device_list_t {
    pub serial_numbers: *mut *mut c_char,
    pub usb_board_ids: *mut c_int,
    pub usb_device_index: *mut c_int,
    pub devicecount: c_int,
    pub usb_devices: *mut *mut c_void,
    pub usb_devicecount: c_int,
}

pub type hackrf_sample_block_cb_fn = unsafe extern "C" fn(transfer: *mut hackrf_transfer) -> c_int;
pub type hackrf_flush_cb_fn = unsafe extern "C" fn(flush_ctx: *mut c_void, success: c_int);

pub const HACKRF_SUCCESS: c_int = 0;
pub const HACKRF_TRUE: c_int = 1;

#[link(name = "hackrf")]
extern "C" {
    pub fn hackrf_init() -> c_int;
    pub fn hackrf_exit() -> c_int;
    pub fn hackrf_device_list() -> *mut hackrf_device_list_t;
    pub fn hackrf_device_list_open(
        list: *mut hackrf_device_list_t,
        idx: c_int,
        device: *mut *mut hackrf_device,
    ) -> c_int;
    pub fn hackrf_device_list_free(list: *mut hackrf_device_list_t);
    pub fn hackrf_open_by_serial(
        desired_serial_number: *const c_char,
        device: *mut *mut hackrf_device,
    ) -> c_int;
    pub fn hackrf_close(device: *mut hackrf_device) -> c_int;
    pub fn hackrf_start_rx(
        device: *mut hackrf_device,
        callback: hackrf_sample_block_cb_fn,
        rx_ctx: *mut c_void,
    ) -> c_int;
    pub fn hackrf_stop_rx(device: *mut hackrf_device) -> c_int;
    pub fn hackrf_start_tx(
        device: *mut hackrf_device,
        callback: hackrf_sample_block_cb_fn,
        tx_ctx: *mut c_void,
    ) -> c_int;
    pub fn hackrf_enable_tx_flush(
        device: *mut hackrf_device,
        callback: hackrf_flush_cb_fn,
        flush_ctx: *mut c_void,
    ) -> c_int;
    pub fn hackrf_stop_tx(device: *mut hackrf_device) -> c_int;
    pub fn hackrf_is_streaming(device: *mut hackrf_device) -> c_int;
    pub fn hackrf_si5351c_read(
        device: *mut hackrf_device,
        register_number: u16,
        value: *mut u16,
    ) -> c_int;
    pub fn hackrf_set_baseband_filter_bandwidth(
        device: *mut hackrf_device,
        bandwidth_hz: u32,
    ) -> c_int;
    pub fn hackrf_board_id_read(device: *mut hackrf_device, value: *mut u8) -> c_int;
    pub fn hackrf_version_string_read(
        device: *mut hackrf_device,
        version: *mut c_char,
        length: u8,
    ) -> c_int;
    pub fn hackrf_set_freq(device: *mut hackrf_device, freq_hz: u64) -> c_int;
    pub fn hackrf_set_sample_rate(device: *mut hackrf_device, freq_hz: f64) -> c_int;
    pub fn hackrf_set_amp_enable(device: *mut hackrf_device, value: u8) -> c_int;
    pub fn hackrf_board_partid_serialno_read(
        device: *mut hackrf_device,
        read_partid_serialno: *mut read_partid_serialno_t,
    ) -> c_int;
    pub fn hackrf_set_lna_gain(device: *mut hackrf_device, value: u32) -> c_int;
    pub fn hackrf_set_vga_gain(device: *mut hackrf_device, value: u32) -> c_int;
    pub fn hackrf_set_txvga_gain(device: *mut hackrf_device, value: u32) -> c_int;
    pub fn hackrf_set_antenna_enable(device: *mut hackrf_device, value: u8) -> c_int;
    pub fn hackrf_error_name(errcode: c_int) -> *const c_char;
    pub fn hackrf_board_id_name(board_id: c_int) -> *const c_char;
    pub fn hackrf_library_version() -> *const c_char;
    pub fn hackrf_library_release() -> *const c_char;
}
