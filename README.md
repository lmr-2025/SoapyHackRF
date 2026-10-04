# Soapy SDR module for Hack RF

This repository holds the original C++ SoapySDR module **and** a Rust port of
it (`Cargo.toml`, `src/`, `tests/`, `examples/`). The port reproduces the
driver's behaviour on a mockable hardware abstraction so that all of it can be
tested without a radio, and fixes the defects listed in [`BUGS.md`](BUGS.md).

## Rust port

### Building and testing

```sh
cargo build                      # library + libhackrf backend (needs libhackrf >= 2022.09)
cargo build --no-default-features # pure-logic build, no C library required
cargo test                       # 100 tests, no hardware needed
cargo run --example enumerate    # lists attached HackRFs through libhackrf
cargo run --example rx_record -- 100e6 10e6 2 out.cs8
```

The crate has no dependencies. The `libhackrf` feature (on by default) links
against the system `libhackrf` through hand-written `extern "C"` declarations
(`src/ffi.rs`); it needs a release with `hackrf_enable_tx_flush`
(2022.09.1 or newer) so that bursts end without losing samples.

### Layout

| Module | Role | C++ counterpart |
|--------|------|-----------------|
| `types` | constants, `Direction`, `StreamFormat`, `Range`, `Kwargs`, flags, MAX2837 filter table | `SoapyHackRF.hpp` macros/enums |
| `error` | `HackrfError` (libhackrf codes) and `Error` | `hackrf_error_name`, exceptions, `SOAPY_SDR_*` codes |
| `gain` | overall gain distribution and element quantisation | `setGain` |
| `convert` | CS8 ↔ CS16/CF32/CF64, `Sample` trait, dynamic buffers | `readbuf` / `writebuf` |
| `ring` | transfer ring with explicit slot ownership | `Stream::buf*` fields |
| `backend` | `Backend`/`DeviceHandle` traits, `RxHandler`/`TxHandler` callbacks, `Session` | libhackrf calls, `SoapyHackRFSession` |
| `mock` | scriptable backend with call log, fault injection, pumpable transfers | – |
| `libhackrf` + `ffi` | the real backend | – |
| `enumerate` | `find_hackrf`, claimed-serial cache | `HackRF_Registration.cpp` |
| `device` | `HackRf`: identification, settings, antenna, gain, frequency, rate, bandwidth | `HackRF_Settings.cpp` |
| `stream` | `RxStream`/`TxStream`, callbacks, half-duplex switching, bursts | `HackRF_Streaming.cpp` |

### API mapping

The method names follow SoapySDR's `Device` API in snake case; every call
returns `Result` instead of logging and throwing. Streams are typed objects
rather than opaque handles, in the spirit of seify/FutureSDR's `RxStreamer`
and `TxStreamer`:

| SoapySDR | Rust |
|----------|------|
| `Device::enumerate(args)` | `find_hackrf(&backend, &args)` / `HackRfDevice::enumerate_default(&args)` |
| `Device::make(args)` | `HackRf::open(backend, &args)` / `HackRfDevice::open_default(&args)` |
| `setupStream(RX, fmt, chans, args)` | `dev.rx_stream(fmt, &chans, &args)` → `RxStream` |
| `setupStream(TX, …)` | `dev.tx_stream(…)` → `TxStream` |
| `activateStream` / `deactivateStream` / `closeStream` | `stream.activate()` / `deactivate()` / `close()` (or drop) |
| `activateStream(tx, END_BURST, 0, numElems)` | `tx.activate_burst(num_samples)` |
| `readStream(buffs, n, flags, timeNs, timeoutUs)` | `rx.read(&mut buf, timeout)` → `ReadResult` |
| `writeStream(buffs, n, flags, …)` | `tx.write(&buf, flags, timeout)` (`StreamFlags::END_BURST` flushes) |
| `readStreamStatus` | `tx.read_status(timeout)` → `StreamEvent::Underflow` |
| `acquireReadBuffer` / `releaseReadBuffer` | `rx.acquire(timeout)` → `RxBuffer` (released on drop) |
| `acquireWriteBuffer` / `releaseWriteBuffer` | `tx.acquire(timeout)` → `TxBuffer::submit(n, flags)` |
| `SOAPY_SDR_TIMEOUT`, `_OVERFLOW`, `_UNDERFLOW` | `Error::Timeout`, `Error::Overflow`, `Error::Underflow` (`Error::soapy_code()`) |
| `setGain(dir, ch, value)` | `dev.set_gain(dir, ch, value)` |
| `setGain(dir, ch, name, value)` / `getGain` | `dev.set_gain_element` / `dev.gain_element` |
| `setFrequency(dir, ch, "RF", f, args)` | `dev.set_frequency(dir, ch, "RF", f, &args)` |
| `writeSetting("bias_tx", "true")` | `dev.write_setting("bias_tx", "true")` |

Sample buffers are interleaved I/Q slices of `i8`, `i16`, `f32` or `f64`; the
element type must match the format chosen at `rx_stream`/`tx_stream`
(`SampleBufMut`/`SampleBuf` exist for formats chosen at run time).

### Design notes

* **Backend trait.** Everything the driver needs from libhackrf is behind
  `backend::Backend` / `backend::DeviceHandle`. `mock::MockBackend` implements
  the same contract with libhackrf's quirks (gain masking, automatic filter
  reset on sample-rate change, the old "re-open after stop" behaviour, the
  TX flush callback) so the test-suite exercises the real control flow.
* **Half duplex.** Settings are stored per direction; those for the inactive
  direction are deferred until the radio switches, and every activation
  re-synchronises the hardware with the target direction's settings.
* **Bursts.** `END_BURST` (via `write` or `activate_burst`) flushes the final
  partial buffer with its exact length, keeps streaming until libhackrf has
  taken it, and waits for libhackrf's flush callback before switching to RX.
* **seify / FutureSDR.** The API shapes (`Direction`, `Range`, string
  `Args`, typed streamers with `mtu`/`activate`/`read`/`write` and
  `end_burst`) mirror seify's so that a `seify` adapter is a thin layer.
  seify itself is not a dependency: its 0.25 API has just been redesigned, it
  pulls in futures/serde/nom/thiserror, and it already ships its own HackRF
  driver on `nusb` – adding it here would not reduce code and would add a
  moving target.

### Not covered by the port

SoapySDR has no C ABI for modules, so this crate cannot register itself as a
SoapySDR module; the C++ sources remain for that. A C ABI over `HackRf` plus a
small C++ shim would be the way to plug the Rust implementation into
SoapySDR.

## C++ module

### Dependencies

* SoapySDR - https://github.com/pothosware/SoapySDR/wiki
* libhackrf - https://github.com/mossmann/hackrf/wiki

### Documentation

* https://github.com/pothosware/SoapyHackRF/wiki

## Licensing information

The MIT License (MIT)

Copyright (c) 2015

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
