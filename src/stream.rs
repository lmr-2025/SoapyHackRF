//! Streaming: ring-buffered RX/TX, the libhackrf callbacks, half-duplex
//! switching and burst transmission (the C++ `HackRF_Streaming.cpp`).
//!
//! [`RxStream`] and [`TxStream`] are created with [`HackRf::rx_stream`] /
//! [`HackRf::tx_stream`]. Reading an inactive RX stream or writing an
//! inactive TX stream activates it automatically, switching the radio out of
//! the other direction first (waiting for queued TX data to be sent).
//!
//! # Bursts
//!
//! A burst ends either with [`TxStream::activate_burst`] (declaring the
//! number of samples up front, as `activateStream(END_BURST, numElems)` did)
//! or by passing [`StreamFlags::END_BURST`] to [`TxStream::write`]. In both
//! cases the final partial buffer is transmitted with its exact length, the
//! TX callback keeps the stream alive until that buffer has been handed to
//! libhackrf, and the libhackrf flush callback (when supported) tells the
//! driver when the hardware has actually sent everything, so switching to RX
//! afterwards loses no samples.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::backend::{Backend, DeviceHandle, RxHandler, StreamingStatus, TxFill, TxHandler};
use crate::convert::{Sample, SampleBuf, SampleBufMut};
use crate::device::HackRf;
use crate::error::{Error, Result};
use crate::ring::Ring;
use crate::types::*;

/// How long [`HackRf::activate_rx`] waits for a finishing TX burst.
pub const BURST_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the driver waits for the libhackrf flush callback.
pub const FLUSH_TIMEOUT: Duration = Duration::from_secs(1);

/// RX buffer state.
pub struct RxBuf {
    /// The ring, present while an RX stream is open.
    pub ring: Option<Ring>,
    /// Samples were dropped since the last read.
    pub overflow: bool,
    /// Number of transfers delivered by the callback.
    pub transfers: u64,
}

/// TX buffer state.
pub struct TxBuf {
    /// The ring, present while a TX stream is open.
    pub ring: Option<Ring>,
    /// The callback ran dry since the last status read.
    pub underflow: bool,
    /// A burst end was requested.
    pub burst_end: bool,
    /// Samples still to be delivered to libhackrf before the burst is done.
    pub burst_samps: i64,
    /// The callback has delivered the whole burst and stopped streaming.
    pub burst_done: bool,
    /// The flush callback fired.
    pub flushed: bool,
    /// Number of transfers filled by the callback.
    pub transfers: u64,
    /// Number of transfers that were zero filled.
    pub underflows: u64,
}

/// Buffer state shared with the backend callbacks.
pub struct BufState {
    /// RX side.
    pub rx: RxBuf,
    /// TX side.
    pub tx: TxBuf,
}

/// The C++ `_buf_mutex` / `_buf_cond` pair plus the callbacks.
pub struct Shared {
    /// Buffer state.
    pub st: Mutex<BufState>,
    /// Signalled whenever the buffer state changes.
    pub cv: Condvar,
}

impl Shared {
    pub(crate) fn new() -> Shared {
        Shared {
            st: Mutex::new(BufState {
                rx: RxBuf {
                    ring: None,
                    overflow: false,
                    transfers: 0,
                },
                tx: TxBuf {
                    ring: None,
                    underflow: false,
                    burst_end: false,
                    burst_samps: 0,
                    burst_done: false,
                    flushed: false,
                    transfers: 0,
                    underflows: 0,
                },
            }),
            cv: Condvar::new(),
        }
    }

    /// Lock the buffer state (poisoning is ignored: the state is plain data).
    pub fn lock(&self) -> MutexGuard<'_, BufState> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait on the condition variable until `pred` holds or `deadline`
    /// passes. Returns the guard and whether the predicate holds.
    pub fn wait_until<'a, F: FnMut(&mut BufState) -> bool>(
        &'a self,
        mut guard: MutexGuard<'a, BufState>,
        deadline: Instant,
        mut pred: F,
    ) -> (MutexGuard<'a, BufState>, bool) {
        loop {
            if pred(&mut guard) {
                return (guard, true);
            }
            let now = Instant::now();
            if now >= deadline {
                return (guard, false);
            }
            let (g, _) = self
                .cv
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            guard = g;
        }
    }
}

impl RxHandler for Shared {
    fn on_rx(&self, data: &[i8]) -> bool {
        let mut st = self.lock();
        let rx = &mut st.rx;
        let ring = match rx.ring.as_mut() {
            Some(r) => r,
            None => return false,
        };
        rx.transfers += 1;
        if data.is_empty() {
            return true;
        }
        if ring.free() == 0 {
            rx.overflow = true;
            if !ring.drop_oldest() {
                // Every slot is held by the reader: drop the incoming data.
                self.cv.notify_all();
                return true;
            }
        }
        let slot = ring.begin_produce().expect("a slot was just freed");
        let n = data.len().min(ring.slot_len());
        // SAFETY: the slot is in the *filling* state and owned by us.
        unsafe { ring.slot_mut(slot)[..n].copy_from_slice(&data[..n]) };
        ring.end_produce(slot, n);
        self.cv.notify_all();
        true
    }
}

impl TxHandler for Shared {
    fn on_tx(&self, buf: &mut [i8]) -> TxFill {
        let mut st = self.lock();
        let tx = &mut st.tx;
        let ring = match tx.ring.as_mut() {
            Some(r) => r,
            None => {
                return TxFill {
                    valid_len: 0,
                    keep_streaming: false,
                }
            }
        };
        tx.transfers += 1;
        if tx.burst_done {
            // Everything was handed over on the previous call; stop now so
            // libhackrf does not resubmit and (if enabled) flushes.
            self.cv.notify_all();
            return TxFill {
                valid_len: 0,
                keep_streaming: false,
            };
        }
        if let Some(slot) = ring.begin_consume() {
            let valid = ring.valid(slot).min(buf.len());
            // SAFETY: the slot is in the *draining* state and owned by us.
            unsafe { buf[..valid].copy_from_slice(&ring.slot_valid(slot)[..valid]) };
            ring.end_consume(slot);
            if tx.burst_end {
                tx.burst_samps -= (valid / BYTES_PER_SAMPLE) as i64;
                if tx.burst_samps <= 0 {
                    tx.burst_done = true;
                }
            }
            self.cv.notify_all();
            TxFill {
                valid_len: valid,
                keep_streaming: true,
            }
        } else if tx.burst_end && tx.burst_samps <= 0 {
            tx.burst_done = true;
            self.cv.notify_all();
            TxFill {
                valid_len: 0,
                keep_streaming: false,
            }
        } else {
            buf.iter_mut().for_each(|b| *b = 0);
            tx.underflow = true;
            tx.underflows += 1;
            self.cv.notify_all();
            TxFill {
                valid_len: buf.len(),
                keep_streaming: true,
            }
        }
    }

    fn on_flush(&self, _success: bool) {
        let mut st = self.lock();
        st.tx.flushed = true;
        self.cv.notify_all();
    }
}

/// Outcome of a successful read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadResult {
    /// Complex samples written into the caller's buffer.
    pub samples: usize,
    /// Flags (never set at present; overflow is reported as [`Error::Overflow`]).
    pub flags: StreamFlags,
}

/// Asynchronous stream events reported by [`TxStream::read_status`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// The transmitter ran out of samples and sent zeros.
    Underflow,
}

fn parse_buffers_arg(args: &Kwargs) -> Result<usize> {
    match args.get(BUFFERS_STREAM_ARG) {
        None => Ok(BUF_NUM),
        Some(s) => match s.trim().parse::<i64>() {
            Ok(n) if n > 0 => Ok(n as usize),
            Ok(_) => Ok(BUF_NUM),
            Err(_) => Err(Error::InvalidArgument(format!(
                "buffers={s:?} is not an integer"
            ))),
        },
    }
}

fn check_channels(channels: &[usize]) -> Result<()> {
    if channels.len() > 1 || channels.first().is_some_and(|&c| c != 0) {
        return Err(Error::InvalidArgument(
            "setupStream invalid channel selection".into(),
        ));
    }
    Ok(())
}

impl<B: Backend> HackRf<B> {
    /// Open an RX stream (`setupStream(SOAPY_SDR_RX, ...)`).
    pub fn rx_stream(
        &self,
        format: StreamFormat,
        channels: &[usize],
        args: &Kwargs,
    ) -> Result<RxStream<'_, B>> {
        check_channels(channels)?;
        let buf_num = parse_buffers_arg(args)?;
        let mut st = self.lock();
        if st.rx.opened {
            return Err(Error::StreamAlreadyOpen(Direction::Rx));
        }
        {
            let mut bs = self.shared.lock();
            bs.rx.ring = Some(Ring::new(buf_num, BUF_LEN));
            bs.rx.overflow = false;
        }
        st.rx.opened = true;
        st.rx.format = format;
        st.rx.buf_num = buf_num;
        Ok(RxStream {
            dev: self,
            format,
            buf_num,
            remainder: None,
            open: true,
        })
    }

    /// Open a TX stream (`setupStream(SOAPY_SDR_TX, ...)`).
    pub fn tx_stream(
        &self,
        format: StreamFormat,
        channels: &[usize],
        args: &Kwargs,
    ) -> Result<TxStream<'_, B>> {
        check_channels(channels)?;
        let buf_num = parse_buffers_arg(args)?;
        let mut st = self.lock();
        if st.tx.opened {
            return Err(Error::StreamAlreadyOpen(Direction::Tx));
        }
        {
            let mut bs = self.shared.lock();
            bs.tx.ring = Some(Ring::new(buf_num, BUF_LEN));
            bs.tx.underflow = false;
            bs.tx.burst_end = false;
            bs.tx.burst_samps = 0;
            bs.tx.burst_done = false;
            bs.tx.flushed = false;
        }
        st.tx.opened = true;
        st.tx.format = format;
        st.tx.buf_num = buf_num;
        Ok(TxStream {
            dev: self,
            format,
            buf_num,
            remainder: None,
            burst_target: None,
            written: 0,
            open: true,
        })
    }

    fn start_streaming(
        &self,
        direction: Direction,
        st: &mut crate::device::DeviceState<B>,
    ) -> Result<()> {
        let handler: Arc<Shared> = Arc::clone(&self.shared);
        let start = |h: &B::Device| match direction {
            Direction::Rx => h
                .start_rx(handler.clone())
                .map_err(|e| Error::hackrf("hackrf_start_rx", e)),
            Direction::Tx => h
                .start_tx(handler.clone())
                .map_err(|e| Error::hackrf("hackrf_start_tx", e)),
        };
        start(st.handle()?)?;
        let mut status = st.handle()?.is_streaming();
        if status == StreamingStatus::ExitCalled {
            // Old libhackrf: a stopped handle cannot restart; re-open and
            // re-apply this direction's settings.
            self.reopen(st)?;
            st.reapply_all(direction)?;
            start(st.handle()?)?;
            status = st.handle()?.is_streaming();
        }
        if status != StreamingStatus::Streaming {
            return Err(Error::StreamError(format!(
                "activate {direction} stream failed: {status:?}"
            )));
        }
        Ok(())
    }

    /// Switch the radio to RX (`activateStream` on the RX stream).
    pub fn activate_rx(&self) -> Result<()> {
        let mut st = self.lock();
        if st.mode == TransceiverMode::Rx {
            return Ok(());
        }
        if st.mode == TransceiverMode::Tx {
            self.finish_tx_burst(&mut st);
            st.handle()?
                .stop_tx()
                .map_err(|e| Error::hackrf("hackrf_stop_tx", e))?;
            st.mode = TransceiverMode::Off;
        }
        st.resync(Direction::Rx)?;
        {
            let mut bs = self.shared.lock();
            if let Some(r) = bs.rx.ring.as_mut() {
                r.reset();
            }
            bs.rx.overflow = false;
        }
        self.start_streaming(Direction::Rx, &mut st)?;
        st.mode = TransceiverMode::Rx;
        Ok(())
    }

    /// If a burst is ending, wait until the callback has handed everything
    /// to libhackrf and the hardware has flushed it.
    fn finish_tx_burst(&self, st: &mut crate::device::DeviceState<B>) {
        let (burst_end, supports_flush) = {
            let bs = self.shared.lock();
            let flush = st.handle.as_ref().is_some_and(|h| h.supports_tx_flush());
            (bs.tx.burst_end, flush)
        };
        if !burst_end {
            return;
        }
        let deadline = Instant::now() + BURST_DRAIN_TIMEOUT;
        {
            let bs = self.shared.lock();
            let _ = self.shared.wait_until(bs, deadline, |b| b.tx.burst_done);
        }
        if let Some(h) = st.handle.as_ref() {
            while h.is_streaming() == StreamingStatus::Streaming && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if supports_flush {
            let bs = self.shared.lock();
            let _ = self
                .shared
                .wait_until(bs, Instant::now() + FLUSH_TIMEOUT, |b| b.tx.flushed);
        }
    }

    /// Switch the radio to TX (`activateStream` on the TX stream). With
    /// `burst_samples > 0` the stream stops by itself after that many
    /// samples have been transmitted.
    pub fn activate_tx(&self, burst_samples: usize) -> Result<()> {
        let mut st = self.lock();
        {
            let mut bs = self.shared.lock();
            let finished = st.mode == TransceiverMode::Tx && bs.tx.burst_done;
            if burst_samples > 0 {
                bs.tx.burst_end = true;
                bs.tx.burst_samps = burst_samples as i64;
                bs.tx.burst_done = false;
                bs.tx.flushed = false;
            } else if finished {
                bs.tx.burst_end = false;
                bs.tx.burst_samps = 0;
                bs.tx.burst_done = false;
                bs.tx.flushed = false;
            }
            if st.mode == TransceiverMode::Tx {
                if !finished {
                    return Ok(());
                }
                // The previous burst finished and stopped the stream: restart.
                drop(bs);
                st.handle()?
                    .stop_tx()
                    .map_err(|e| Error::hackrf("hackrf_stop_tx", e))?;
                st.mode = TransceiverMode::Off;
            }
        }
        if st.mode == TransceiverMode::Rx {
            st.handle()?
                .stop_rx()
                .map_err(|e| Error::hackrf("hackrf_stop_rx", e))?;
            st.mode = TransceiverMode::Off;
        }
        st.resync(Direction::Tx)?;
        {
            let mut bs = self.shared.lock();
            bs.tx.underflow = false;
            bs.tx.flushed = false;
        }
        self.start_streaming(Direction::Tx, &mut st)?;
        st.mode = TransceiverMode::Tx;
        Ok(())
    }

    /// Stop receiving (`deactivateStream` on the RX stream).
    pub fn deactivate_rx(&self) -> Result<()> {
        let mut st = self.lock();
        if st.mode == TransceiverMode::Rx {
            st.handle()?
                .stop_rx()
                .map_err(|e| Error::hackrf("hackrf_stop_rx", e))?;
            st.mode = TransceiverMode::Off;
        }
        Ok(())
    }

    /// Stop transmitting (`deactivateStream` on the TX stream). Queued
    /// samples that were not yet sent are kept for the next activation.
    pub fn deactivate_tx(&self) -> Result<()> {
        let mut st = self.lock();
        if st.mode == TransceiverMode::Tx {
            st.handle()?
                .stop_tx()
                .map_err(|e| Error::hackrf("hackrf_stop_tx", e))?;
            st.mode = TransceiverMode::Off;
        }
        let mut bs = self.shared.lock();
        bs.tx.burst_end = false;
        bs.tx.burst_samps = 0;
        bs.tx.burst_done = false;
        bs.tx.flushed = false;
        Ok(())
    }

    /// Statistics: `(rx_transfers, rx_overflows, tx_transfers, tx_underflows)`.
    pub fn stream_stats(&self) -> (u64, u64, u64, u64) {
        let bs = self.shared.lock();
        (
            bs.rx.transfers,
            bs.rx.ring.as_ref().map_or(0, |r| r.overflows()),
            bs.tx.transfers,
            bs.tx.underflows,
        )
    }
}

/// A slot the user side currently holds.
struct Held {
    slot: usize,
    ptr: *mut i8,
    /// Capacity in complex samples (TX) or valid samples (RX).
    samples: usize,
    /// Samples already consumed (RX) or filled (TX).
    offset: usize,
}

/// An open RX stream. Dropping it closes the stream.
pub struct RxStream<'a, B: Backend> {
    dev: &'a HackRf<B>,
    format: StreamFormat,
    buf_num: usize,
    remainder: Option<Held>,
    open: bool,
}

// SAFETY: the raw pointer in `remainder` addresses a ring slot owned by the
// device and only ever accessed through this stream object.
unsafe impl<'a, B: Backend> Send for RxStream<'a, B> {}

/// A ring slot borrowed from an [`RxStream`] through the direct access API.
/// Dropping it releases the slot.
pub struct RxBuffer<'s, 'a, B: Backend> {
    stream: &'s mut RxStream<'a, B>,
    slot: usize,
    data: &'s [i8],
}

impl<'s, 'a, B: Backend> RxBuffer<'s, 'a, B> {
    /// The valid interleaved I/Q bytes of the transfer.
    pub fn data(&self) -> &[i8] {
        self.data
    }

    /// Number of complex samples.
    pub fn samples(&self) -> usize {
        self.data.len() / BYTES_PER_SAMPLE
    }

    /// Ring slot index (`handle` in the SoapySDR API).
    pub fn handle(&self) -> usize {
        self.slot
    }
}

impl<'s, 'a, B: Backend> Drop for RxBuffer<'s, 'a, B> {
    fn drop(&mut self) {
        self.stream.release_slot(self.slot);
    }
}

impl<'a, B: Backend> RxStream<'a, B> {
    /// Format chosen at setup.
    pub fn format(&self) -> StreamFormat {
        self.format
    }

    /// Samples per transfer buffer.
    pub fn mtu(&self) -> usize {
        MTU_SAMPLES
    }

    /// Number of ring slots.
    pub fn num_direct_buffers(&self) -> usize {
        self.buf_num
    }

    /// Switch the radio to RX.
    pub fn activate(&mut self) -> Result<()> {
        self.drop_remainder();
        self.dev.activate_rx()
    }

    /// Stop receiving.
    pub fn deactivate(&mut self) -> Result<()> {
        self.dev.deactivate_rx()
    }

    /// Whether the radio is currently receiving.
    pub fn is_active(&self) -> bool {
        self.dev.transceiver_mode() == TransceiverMode::Rx
    }

    fn release_slot(&mut self, slot: usize) {
        let mut bs = self.dev.shared.lock();
        if let Some(r) = bs.rx.ring.as_mut() {
            r.end_consume(slot);
        }
        self.dev.shared.cv.notify_all();
    }

    fn drop_remainder(&mut self) {
        if let Some(h) = self.remainder.take() {
            self.release_slot(h.slot);
        }
    }

    /// Wait for the next filled slot; activates the stream if needed.
    fn acquire_slot(&mut self, timeout: Duration) -> Result<Held> {
        let deadline = Instant::now() + timeout;
        let mode = self.dev.transceiver_mode();
        if mode == TransceiverMode::Tx {
            // Wait for queued TX data to be transmitted before switching.
            // Once a burst has completed the callback no longer drains the
            // ring, so there is nothing to wait for.
            let bs = self.dev.shared.lock();
            let (_g, drained) = self.dev.shared.wait_until(bs, deadline, |b| {
                b.tx.burst_done || b.tx.ring.as_ref().map_or(true, |r| r.filled() == 0)
            });
            if !drained {
                return Err(Error::Timeout);
            }
        }
        if mode != TransceiverMode::Rx {
            self.activate()?;
        }
        let bs = self.dev.shared.lock();
        let (mut bs, ready) = self.dev.shared.wait_until(bs, deadline, |b| {
            b.rx.ring.as_ref().map_or(true, |r| r.filled() > 0) || b.rx.overflow
        });
        if bs.rx.ring.is_none() {
            return Err(Error::StreamError("RX stream is closed".into()));
        }
        if !ready {
            return Err(Error::Timeout);
        }
        if bs.rx.overflow {
            bs.rx.overflow = false;
            return Err(Error::Overflow);
        }
        let ring = bs.rx.ring.as_mut().expect("checked above");
        let slot = ring.begin_consume().expect("predicate guarantees data");
        let (ptr, _) = ring.slot_ptr(slot);
        let samples = ring.valid(slot) / BYTES_PER_SAMPLE;
        Ok(Held {
            slot,
            ptr,
            samples,
            offset: 0,
        })
    }

    /// Read samples into an interleaved I/Q buffer of the stream's format
    /// (`readStream`). At most [`RxStream::mtu`] samples are returned per
    /// call. Returns [`Error::Timeout`] if nothing arrived, and
    /// [`Error::Overflow`] once after samples were dropped.
    pub fn read<T: Sample>(&mut self, buf: &mut [T], timeout: Duration) -> Result<ReadResult> {
        self.read_dyn(SampleBufMut::from(buf), timeout)
    }

    /// Like [`RxStream::read`] for a buffer whose format is chosen at run time.
    pub fn read_dyn(&mut self, mut dst: SampleBufMut<'_>, timeout: Duration) -> Result<ReadResult> {
        if dst.format() != self.format {
            return Err(Error::FormatMismatch {
                expected: self.format,
                actual: dst.format(),
            });
        }
        let want = dst.num_samples().min(MTU_SAMPLES);
        let mut copied = 0;

        if let Some(h) = self.remainder.as_mut() {
            let n = (h.samples - h.offset).min(want);
            // SAFETY: the slot is held by this stream; `offset + n <= samples`.
            let src = unsafe {
                std::slice::from_raw_parts(
                    h.ptr.add(h.offset * BYTES_PER_SAMPLE),
                    n * BYTES_PER_SAMPLE,
                )
            };
            dst.write_from_native(0, src);
            h.offset += n;
            copied = n;
            if h.offset == h.samples {
                self.drop_remainder();
            }
            if copied == want {
                return Ok(ReadResult {
                    samples: copied,
                    flags: StreamFlags::NONE,
                });
            }
        }

        let held = match self.acquire_slot(timeout) {
            Ok(h) => h,
            Err(Error::Timeout) if copied > 0 => {
                return Ok(ReadResult {
                    samples: copied,
                    flags: StreamFlags::NONE,
                })
            }
            Err(Error::Overflow) if copied > 0 => {
                // Deliver what we have; report the overflow on the next call.
                self.dev.shared.lock().rx.overflow = true;
                return Ok(ReadResult {
                    samples: copied,
                    flags: StreamFlags::NONE,
                });
            }
            Err(e) => return Err(e),
        };
        self.remainder = Some(held);
        let h = self.remainder.as_mut().unwrap();
        let n = h.samples.min(want - copied);
        // SAFETY: as above.
        let src = unsafe { std::slice::from_raw_parts(h.ptr, n * BYTES_PER_SAMPLE) };
        dst.write_from_native(copied, src);
        h.offset = n;
        copied += n;
        if h.offset == h.samples {
            self.drop_remainder();
        }
        Ok(ReadResult {
            samples: copied,
            flags: StreamFlags::NONE,
        })
    }

    /// Direct access: borrow the next filled transfer buffer without copying
    /// (`acquireReadBuffer`). Dropping the returned buffer releases it.
    pub fn acquire(&mut self, timeout: Duration) -> Result<RxBuffer<'_, 'a, B>> {
        self.drop_remainder();
        let h = self.acquire_slot(timeout)?;
        // SAFETY: the slot stays owned by this stream until the RxBuffer is
        // dropped, which happens before `self` can be used again.
        let data = unsafe { std::slice::from_raw_parts(h.ptr, h.samples * BYTES_PER_SAMPLE) };
        Ok(RxBuffer {
            stream: self,
            slot: h.slot,
            data,
        })
    }

    /// Close the stream (`closeStream`); also done on drop.
    pub fn close(mut self) {
        self.close_inner();
    }

    fn close_inner(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        self.drop_remainder();
        let _ = self.dev.deactivate_rx();
        let mut st = self.dev.lock();
        {
            let mut bs = self.dev.shared.lock();
            bs.rx.ring = None;
            bs.rx.overflow = false;
        }
        st.rx.opened = false;
    }
}

impl<'a, B: Backend> Drop for RxStream<'a, B> {
    fn drop(&mut self) {
        self.close_inner();
    }
}

/// An open TX stream. Dropping it closes the stream.
pub struct TxStream<'a, B: Backend> {
    dev: &'a HackRf<B>,
    format: StreamFormat,
    buf_num: usize,
    remainder: Option<Held>,
    burst_target: Option<usize>,
    written: usize,
    open: bool,
}

// SAFETY: see `RxStream`.
unsafe impl<'a, B: Backend> Send for TxStream<'a, B> {}

/// A ring slot borrowed from a [`TxStream`] through the direct access API.
/// Call [`TxBuffer::submit`] to queue it; dropping it unsubmitted returns the
/// slot unused.
pub struct TxBuffer<'s, 'a, B: Backend> {
    stream: &'s mut TxStream<'a, B>,
    slot: usize,
    data: &'s mut [i8],
    submitted: bool,
}

impl<'s, 'a, B: Backend> TxBuffer<'s, 'a, B> {
    /// The whole transfer buffer (interleaved I/Q bytes).
    pub fn data(&mut self) -> &mut [i8] {
        self.data
    }

    /// Capacity in complex samples.
    pub fn capacity(&self) -> usize {
        self.data.len() / BYTES_PER_SAMPLE
    }

    /// Ring slot index (`handle` in the SoapySDR API).
    pub fn handle(&self) -> usize {
        self.slot
    }

    /// Queue the first `samples` complex samples for transmission
    /// (`releaseWriteBuffer`). With [`StreamFlags::END_BURST`] the burst ends
    /// after them.
    pub fn submit(mut self, samples: usize, flags: StreamFlags) {
        let samples = samples.min(self.capacity());
        self.submitted = true;
        let slot = self.slot;
        self.stream.submit_slot(slot, samples);
        if flags.contains(StreamFlags::END_BURST) {
            self.stream.end_burst();
        }
    }
}

impl<'s, 'a, B: Backend> Drop for TxBuffer<'s, 'a, B> {
    fn drop(&mut self) {
        if !self.submitted {
            let mut bs = self.stream.dev.shared.lock();
            if let Some(r) = bs.tx.ring.as_mut() {
                r.cancel_produce(self.slot);
            }
        }
    }
}

impl<'a, B: Backend> TxStream<'a, B> {
    /// Format chosen at setup.
    pub fn format(&self) -> StreamFormat {
        self.format
    }

    /// Samples per transfer buffer.
    pub fn mtu(&self) -> usize {
        MTU_SAMPLES
    }

    /// Number of ring slots.
    pub fn num_direct_buffers(&self) -> usize {
        self.buf_num
    }

    /// Switch the radio to TX (continuous transmission).
    pub fn activate(&mut self) -> Result<()> {
        self.burst_target = None;
        self.written = 0;
        self.dev.activate_tx(0)
    }

    /// Switch the radio to TX for a burst of exactly `num_samples` samples
    /// (`activateStream(END_BURST, 0, numElems)`): once that many samples
    /// have been written and transmitted the stream stops by itself.
    pub fn activate_burst(&mut self, num_samples: usize) -> Result<()> {
        if num_samples == 0 {
            return self.activate();
        }
        self.burst_target = Some(num_samples);
        self.written = 0;
        self.dev.activate_tx(num_samples)
    }

    /// Stop transmitting immediately.
    pub fn deactivate(&mut self) -> Result<()> {
        self.burst_target = None;
        self.dev.deactivate_tx()
    }

    /// Whether the radio is currently transmitting.
    pub fn is_active(&self) -> bool {
        self.dev.transceiver_mode() == TransceiverMode::Tx
    }

    /// Whether a requested burst has been completely handed to the hardware.
    pub fn burst_done(&self) -> bool {
        self.dev.shared.lock().tx.burst_done
    }

    fn submit_slot(&mut self, slot: usize, samples: usize) {
        let mut bs = self.dev.shared.lock();
        if let Some(r) = bs.tx.ring.as_mut() {
            if samples == 0 {
                r.cancel_produce(slot);
            } else {
                r.end_produce(slot, samples * BYTES_PER_SAMPLE);
            }
        }
        self.dev.shared.cv.notify_all();
    }

    /// Flush a partially filled remainder and mark the end of the burst.
    fn end_burst(&mut self) {
        self.burst_target = None;
        let remainder = self.remainder.take();
        // One critical section: the callback must not observe the flushed
        // buffer before the burst accounting covers it.
        let mut bs = self.dev.shared.lock();
        if let Some(r) = bs.tx.ring.as_mut() {
            if let Some(h) = remainder {
                if h.offset == 0 {
                    r.cancel_produce(h.slot);
                } else {
                    r.end_produce(h.slot, h.offset * BYTES_PER_SAMPLE);
                }
            }
        }
        if !bs.tx.burst_done {
            // A declared burst may already have completed (burst_done set by
            // the callback); in that case there is nothing left to arm.
            let queued = bs.tx.ring.as_ref().map_or(0, |r| r.queued_bytes()) / BYTES_PER_SAMPLE;
            bs.tx.burst_end = true;
            bs.tx.burst_samps = queued as i64;
            bs.tx.flushed = false;
        }
        self.dev.shared.cv.notify_all();
    }

    /// Wait for a free slot; activates the stream if needed.
    fn acquire_slot(&mut self, timeout: Duration) -> Result<Held> {
        let deadline = Instant::now() + timeout;
        let needs_activation = self.dev.transceiver_mode() != TransceiverMode::Tx
            || self.dev.shared.lock().tx.burst_done;
        if needs_activation {
            let target = self.burst_target;
            self.written = 0;
            self.dev.activate_tx(target.unwrap_or(0))?;
        }
        let bs = self.dev.shared.lock();
        let (mut bs, ready) = self.dev.shared.wait_until(bs, deadline, |b| {
            b.tx.ring.as_ref().map_or(true, |r| r.free() > 0)
        });
        let ring = match bs.tx.ring.as_mut() {
            Some(r) => r,
            None => return Err(Error::StreamError("TX stream is closed".into())),
        };
        if !ready {
            return Err(Error::Timeout);
        }
        let slot = ring
            .begin_produce()
            .expect("predicate guarantees a free slot");
        let (ptr, len) = ring.slot_ptr(slot);
        Ok(Held {
            slot,
            ptr,
            samples: len / BYTES_PER_SAMPLE,
            offset: 0,
        })
    }

    /// Queue samples from an interleaved I/Q buffer of the stream's format
    /// (`writeStream`). At most [`TxStream::mtu`] samples are consumed per
    /// call. Buffers are handed to the hardware once full, or immediately
    /// when `flags` contains [`StreamFlags::END_BURST`]. Returns
    /// [`Error::Timeout`] if no buffer space became available.
    pub fn write<T: Sample>(
        &mut self,
        buf: &[T],
        flags: StreamFlags,
        timeout: Duration,
    ) -> Result<usize> {
        self.write_dyn(SampleBuf::from(buf), flags, timeout)
    }

    /// Like [`TxStream::write`] for a buffer whose format is chosen at run time.
    pub fn write_dyn(
        &mut self,
        src: SampleBuf<'_>,
        flags: StreamFlags,
        timeout: Duration,
    ) -> Result<usize> {
        if src.format() != self.format {
            return Err(Error::FormatMismatch {
                expected: self.format,
                actual: src.format(),
            });
        }
        let end_burst = flags.contains(StreamFlags::END_BURST);
        let want = src.num_samples().min(MTU_SAMPLES);
        let mut copied = 0;
        if want == 0 {
            // Nothing to queue; only the burst flag matters.
            return self.finish_write(0, end_burst);
        }

        if let Some(h) = self.remainder.as_mut() {
            let n = (h.samples - h.offset).min(want);
            // SAFETY: the slot is held by this stream; `offset + n <= samples`.
            let dst = unsafe {
                std::slice::from_raw_parts_mut(
                    h.ptr.add(h.offset * BYTES_PER_SAMPLE),
                    n * BYTES_PER_SAMPLE,
                )
            };
            src.read_to_native(0, dst);
            h.offset += n;
            copied = n;
            if h.offset == h.samples {
                let h = self.remainder.take().unwrap();
                self.submit_slot(h.slot, h.samples);
            }
            if copied == want {
                return self.finish_write(copied, end_burst);
            }
        }

        let held = match self.acquire_slot(timeout) {
            Ok(h) => h,
            Err(Error::Timeout) if copied > 0 => return self.finish_write(copied, end_burst),
            Err(e) => return Err(e),
        };
        self.remainder = Some(held);
        let h = self.remainder.as_mut().unwrap();
        let n = h.samples.min(want - copied);
        // SAFETY: as above.
        let dst = unsafe { std::slice::from_raw_parts_mut(h.ptr, n * BYTES_PER_SAMPLE) };
        src.read_to_native(copied, dst);
        h.offset = n;
        copied += n;
        if h.offset == h.samples {
            let h = self.remainder.take().unwrap();
            self.submit_slot(h.slot, h.samples);
        }
        self.finish_write(copied, end_burst)
    }

    fn finish_write(&mut self, copied: usize, end_burst: bool) -> Result<usize> {
        self.written += copied;
        let target_reached = self.burst_target.is_some_and(|t| self.written >= t);
        if end_burst || target_reached {
            self.end_burst();
        }
        Ok(copied)
    }

    /// Wait for an asynchronous event (`readStreamStatus`): currently only
    /// [`StreamEvent::Underflow`]. Returns [`Error::Timeout`] otherwise.
    pub fn read_status(&mut self, timeout: Duration) -> Result<StreamEvent> {
        let deadline = Instant::now() + timeout;
        let bs = self.dev.shared.lock();
        let (mut bs, hit) = self.dev.shared.wait_until(bs, deadline, |b| b.tx.underflow);
        if hit {
            bs.tx.underflow = false;
            Ok(StreamEvent::Underflow)
        } else {
            Err(Error::Timeout)
        }
    }

    /// Direct access: borrow a free transfer buffer to fill in place
    /// (`acquireWriteBuffer`). A partially written remainder from
    /// [`TxStream::write`] is flushed first.
    pub fn acquire(&mut self, timeout: Duration) -> Result<TxBuffer<'_, 'a, B>> {
        if let Some(h) = self.remainder.take() {
            self.submit_slot(h.slot, h.offset);
        }
        let h = self.acquire_slot(timeout)?;
        // SAFETY: the slot stays owned by this stream until the TxBuffer is
        // dropped or submitted.
        let data = unsafe { std::slice::from_raw_parts_mut(h.ptr, h.samples * BYTES_PER_SAMPLE) };
        Ok(TxBuffer {
            stream: self,
            slot: h.slot,
            data,
            submitted: false,
        })
    }

    /// Close the stream (`closeStream`); also done on drop. Unsent samples
    /// are discarded.
    pub fn close(mut self) {
        self.close_inner();
    }

    fn close_inner(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        if let Some(h) = self.remainder.take() {
            let mut bs = self.dev.shared.lock();
            if let Some(r) = bs.tx.ring.as_mut() {
                r.cancel_produce(h.slot);
            }
        }
        let _ = self.dev.deactivate_tx();
        let mut st = self.dev.lock();
        {
            let mut bs = self.dev.shared.lock();
            bs.tx.ring = None;
            bs.tx.underflow = false;
        }
        st.tx.opened = false;
    }
}

impl<'a, B: Backend> Drop for TxStream<'a, B> {
    fn drop(&mut self) {
        self.close_inner();
    }
}
