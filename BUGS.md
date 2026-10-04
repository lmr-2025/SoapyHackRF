# Defects found in the C++ SoapyHackRF driver

This list was compiled while porting the driver to Rust. Each item names the
C++ location, describes the defect, and states how the Rust port treats it:

* **Fixed** – the port behaves correctly and a test pins the new behaviour.
* **Preserved** – the behaviour is questionable but deliberately kept for
  compatibility; a test documents it.
* **N/A** – the defect cannot occur in the port (different language/design).

The identifiers (G1, S1, …) are referenced from the test-suite and source
comments. Line numbers refer to the C++ sources at commit `763819f`.

## Gain (HackRF_Settings.cpp)

| ID | Location | Defect | Port |
|----|----------|--------|------|
| G1 | `setGain(dir, value)` lines 300–304 | For overall RX gains in the top range the VGA share is computed as `(gain-14)*40/62`, which reaches 65 dB at 116 dB. libhackrf rejects VGA values above 62 (`HACKRF_ERROR_INVALID_PARAM`), so overall gains of 112–116 dB fail (logged only, the caller never learns). | Fixed – `gain::distribute_rx_gain`; `gain_tests::legacy_rx_algorithm_produced_invalid_hardware_values` reproduces the old values. |
| G2 | `setGain(dir, value)` lines 288–304 | LNA values are not multiples of 8 (e.g. 51 dB → LNA 35). libhackrf silently masks to 32 dB, so the hardware gain differs from what `getGain("LNA")` reports; the requested total is reached for only a handful of inputs. | Fixed – every split is on the hardware grid and reaches every even total exactly (`gain_tests::rx_split_reaches_every_even_total_exactly`). |
| G3 | `setGain(dir, value)` lines 283–315 | Values above the maximum (RX > 116, TX > 61) fall through every branch and change nothing, without error. | Fixed – clamped. |
| G4 | `setGain(dir, name, value)` lines 345–376 | Element values are stored unquantised and unclamped: `setGain("LNA", 12)` reports 12 while the hardware uses 8; negative values wrap in the `uint32_t` members and are rejected by libhackrf. | Fixed – `gain::quantize_*`; the stored value equals the hardware value. |
| G5 | `setGain(dir, "AMP", value)` lines 331–333 | `_current_amp = value` stores into a `uint8_t`; negative values wrap to a large positive number and switch the amplifier **on**. | Fixed – `quantize_amp`. |
| G6 | lines 343, 354, 365, 376 | Error log messages pass `uint32_t`/`uint8_t` arguments to `%f` (undefined behaviour, garbage in logs). | N/A – errors are returned as `Result`. |
| G7 | `setGain(dir, value)` lines 306–308, 323–324 | Return codes of three libhackrf calls are OR-ed together (`ret |= …`), producing a meaningless combined code in the log. | N/A – the first error is returned. |

## Sample conversion (HackRF_Streaming.cpp `readbuf` / `writebuf`)

| ID | Location | Defect | Port |
|----|----------|--------|------|
| C1 | lines 100–104 vs 505–540 | `getNativeStreamFormat` advertises a full scale of 128 while the float conversions scale by 127, so applications that convert native CS8 themselves (SoapyRemote) and applications that request CF32 see amplitudes that differ by 0.8 %. | Preserved – `convert::CONVERSION_SCALE` = 127, `convert::NATIVE_FULL_SCALE` = 128; documented by `convert_tests::full_scale_constants_document_the_legacy_inconsistency`. |
| C2 | `writebuf` lines 553–563 | `(int8_t)(x * 127.0)` is undefined behaviour for `|x| > 1`; on x86 it wraps (1.5 → −66). | Fixed – saturating conversion. |
| C3 | `writebuf` lines 553–563 | The cast truncates toward zero, so a CS8 → CF32 → CS8 round trip changes 134 of 255 values by one LSB and small signals are biased toward zero. | Fixed – round to nearest (`convert_tests::float_to_native_rounds_where_legacy_truncated`). |

## RX/TX buffering (HackRF_Streaming.cpp)

| ID | Location | Defect | Port |
|----|----------|--------|------|
| S1 | `hackrf_rx_callback` lines 44–58, `acquireReadBuffer` lines 648–653 | The callback writes to slot `(buf_head + buf_count) % buf_num`, but `acquireReadBuffer` advances `buf_head` without decrementing `buf_count` until release. After every acquire one slot is skipped (and later delivered stale), and when the ring is one short of full the callback overwrites the buffer the reader is still copying out of. | Fixed – `ring::Ring` tracks free/filled/held slots explicitly (`ring_tests::held_slots_are_never_handed_out_again`). |
| S2 | `hackrf_rx_callback` line 47, `acquireReadBuffer` line 656 | `valid_length` is copied but ignored: a short transfer is delivered as a full MTU of samples, the tail being stale bytes. | Fixed – per-slot valid length (`rx_stream_tests::reads_are_capped_at_the_mtu_and_handle_short_transfers`). |
| S3 | `readStream` lines 581–589 | When samples were already copied out of the remainder buffer and the next acquire reports `SOAPY_SDR_OVERFLOW`, the function returns the error and the copied samples are lost. | Fixed – returns the samples, reports the overflow on the next call (`rx_stream_tests::overflow_does_not_lose_samples_already_copied`). |
| S4 | `acquireReadBuffer` lines 640–644, `acquireWriteBuffer` lines 686–690 | `wait_for` without a predicate: a spurious wake-up returns `SOAPY_SDR_TIMEOUT` early. `acquireReadBuffer` also uses the full timeout twice (TX drain, then RX data). | Fixed – deadline based waits. |
| S5 | `Stream::allocate_buffers` lines 131–139 | Buffers come from `malloc` unzeroed and allocation failures are not checked. | Fixed. |
| S6 | `activateStream` lines 328–329, 428–429 | In the re-open path the return value of `hackrf_open_by_serial` is ignored; on failure `_dev` is NULL and every later call dereferences it. | Fixed – `Error::OpenFailed` / `Error::DeviceClosed` (`rx_stream_tests::old_libhackrf_restart_reopens_and_reapplies_settings`). |
| S7 | `activateStream` (TX) line 433 | The TX re-open path applies `_rx_stream.amp_gain` instead of `_tx_stream.amp_gain` (copy/paste). | Fixed (`tx_stream_tests::tx_reopen_path_uses_the_tx_amp_setting`). |
| S8 | `activateStream` (TX) line 406 | Debug message says "Set RX bandwidth" in the TX path. | N/A. |
| S9 | `SoapyHackRF.hpp` `TXStream::bias`, constructor lines 33–57 | `_tx_stream.bias` is never initialised; `readSetting("bias_tx")` and the TX re-open path read an indeterminate value. | Fixed. |
| S10 | `acquireReadBuffer` line 626, `readStreamStatus` line 670 | `_current_mode`, `overflow` and `underflow` are read without holding the mutex that protects them. | Fixed – all state is behind mutexes. |
| S11 | `releaseReadBuffer` line 672 | `buf_count--` without a check; a stray release underflows the `uint32_t`. | N/A – RAII buffer guards. |
| S12 | `activateStream` (RX) lines 270–276 | While a burst finishes the thread spins on `hackrf_is_streaming` with `_device_mutex` held and no upper bound. | Fixed – bounded wait (`stream::BURST_DRAIN_TIMEOUT`). |
| S13 | `readStreamStatus` lines 663–685 | Polls the underflow flag with `sleep_for` instead of waiting on the condition variable. | Fixed. |

## TX bursts (HackRF_Streaming.cpp)

| ID | Location | Defect | Port |
|----|----------|--------|------|
| T1 | `writeStream` lines 597–660 | A partially filled buffer is only submitted when it becomes full. The tail of any transmission whose length is not a multiple of 131072 samples is never sent, and `SOAPY_SDR_END_BURST` passed to `writeStream` is ignored. | Fixed – `END_BURST` flushes the partial buffer with its exact length (`tx_stream_tests::end_burst_flushes_the_partial_buffer_and_stops`). |
| T2 | `hackrf_tx_callback` lines 69–84 | The callback returns −1 in the very call that filled the buffer holding the end of the burst. libhackrf (`hackrf.c:1817`, 2023.01.1) does not submit a transfer whose callback returned non-zero, so the last buffer of every burst is dropped. | Fixed – streaming stops on the following callback, and `hackrf_enable_tx_flush` is used so the hardware has sent everything before the radio is switched. |
| T3 | `hackrf_tx_callback` lines 76–84 | `burst_samps` is checked with `< 0`, and only when a buffer was available. With `numElems` an exact multiple of the MTU the counter reaches 0, never goes negative, and the stream then underflows with zeros forever. | Fixed (`tx_stream_tests::declared_burst_of_an_exact_mtu_multiple_terminates`). |
| T4 | `activateStream` (TX) lines 350–357 | The burst is only set up when `_current_mode == RX`; activating a burst from idle ignores `END_BURST`/`numElems`. | Fixed. |
| T5 | `acquireWriteBuffer` line 698 | `memset(buffs[0], 0, getStreamMTU(stream))` zeroes MTU *bytes*, i.e. only half of the final burst buffer; the other half carries stale samples. | Fixed – exact valid lengths, zeroed slots. |
| T6 | `activateStream` (TX) lines 353–354 | Burst set-up resets `buf_head`/`buf_tail` but not `buf_count`, corrupting the ring if samples were already queued. | Fixed – queued samples survive activation (`tx_stream_tests::deactivate_keeps_queued_samples`). |
| T7 | `activateStream` (RX) lines 270–276 | After the callback stopped the stream, `hackrf_stop_tx` cancels transfers that are still in flight (up to 4 × 256 KiB), losing the end of the burst. | Fixed – flush callback (`tx_stream_tests::switching_to_rx_waits_for_the_burst_and_the_flush`). |
| T8 | `hackrf_tx_callback`, `acquireWriteBuffer` | Once the callback returned −1, `_current_mode` stays `TX` although the device stopped; later writes fill the ring and then time out forever. | Fixed – the next write/activate restarts the stream (`tx_stream_tests::a_new_burst_after_a_finished_one_restarts_the_stream`). |

## Half-duplex bookkeeping and settings (HackRF_Settings.cpp, HackRF_Streaming.cpp)

| ID | Location | Defect | Port |
|----|----------|--------|------|
| H1 | `setFrequency` 389–408, `setSampleRate` 495–518, `setBandwidth` 528–560, `setGain("AMP")` | Settings are written to the hardware immediately whatever direction is active: setting the TX frequency while an RX stream runs retunes the receiver. | Fixed – settings for the inactive direction are stored and applied at the next switch (`device_tests::settings_for_the_inactive_direction_are_deferred`). |
| H2 | `activateStream` lines 240–300, 362–412 | Settings are re-synchronised only when switching *from the other mode*. Set the RX frequency, then the TX frequency, then activate RX from idle: RX runs at the TX frequency. | Fixed – always re-synchronised (`tx_stream_tests::switching_resyncs_each_directions_settings`). |
| H3 | `writeSetting("bias_tx")` line 185 | The firmware switches the bias tee off whenever the radio returns to idle (hackrf.h, "Bias-tee"); the driver only re-applies it in the TX re-open path. | Fixed – re-applied on every activation (`rx_stream_tests::bias_tee_is_reapplied_on_activation`). |
| H4 | `setSampleRate` lines 507–516 | Throws on a hardware error but leaves the stored sample rate updated, so `getSampleRate` disagrees with the hardware. | Fixed – state reverts on error. |
| H5 | `setFrequency` line 393 | `double` → `uint64_t` without validation: negative values are undefined behaviour, values above 7.25 GHz go to the hardware. | Fixed – validated. |
| H6 | `activateStream` lines 240–247 | When the target direction was never configured but the other was, the switch calls `hackrf_set_sample_rate(0)` and tunes to 0 Hz. | Fixed – unset values are skipped. |

## Bandwidth

| ID | Location | Defect | Port |
|----|----------|--------|------|
| B1 | `setBandwidth` lines 528–560, `setSampleRate` | `hackrf_set_sample_rate` resets the baseband filter (documented in hackrf.h); a manual bandwidth set before a sample-rate change is silently lost. `_auto_bandwidth` is written but never read. | Fixed (`device_tests::manual_bandwidth_survives_sample_rate_changes`). |
| B2 | `getBandwidth` lines 563–578 | Returns 0 in automatic mode instead of the filter in use. | Fixed – reports the automatic filter (75 % of the sample rate, rounded to the MAX2837 table). |
| B3 | `setBandwidth(0)` lines 553–556 | Sets the auto flag but never restores the automatic filter on the hardware. | Fixed. |

## Discovery and miscellany

| ID | Location | Defect | Port |
|----|----------|--------|------|
| E1 | `HackRF_Registration.cpp:74` | `std::stoi(args.at("hackrf"))` throws `std::invalid_argument`/`std::out_of_range` out of the discovery function for malformed input. | Fixed – `Error::InvalidArgument`. |
| E2 | `HackRF_Registration.cpp:95–100` | Cached results for claimed devices ignore the `hackrf` index filter. | Preserved. |
| E3 | `HackRF_Settings.cpp:131` vs `HackRF_Registration.cpp:60` | `getHardwareInfo` uses the key `"part id"` while discovery uses `"part_id"`. | Preserved. |
| E4 | `setupStream` lines 186–196, 230–240 | The `buffers` argument parser catches `std::invalid_argument` only; `std::out_of_range` propagates. | Fixed – `Error::InvalidArgument`. |
| E5 | `getStreamArgsInfo` line 119 | `buffers` is described as "Number of buffers per read". | Fixed wording. |
| E6 | `setAntenna` line 203 | Accepts any antenna name silently (the source carries a TODO). | Fixed – unknown names are an error. |
| E7 | `getGainRange` line 389 | Unknown gain names return `Range(0, 0)` silently. | Fixed – `Error::UnknownGainName`. |
| E8 | `getHardwareKey` lines 106–113 | A failing `hackrf_board_id_read` is ignored and the board is reported as `BOARD_ID_INVALID`. | Fixed – error returned. |
| E9 | `setGainMode(true)` line 255 | AGC is silently "enabled" although the hardware has none. | Fixed – `Error::NotSupported`. |
| E10 | `activateStream` line 243 | `%lu` with a `uint64_t` argument (wrong on LLP64 platforms). | N/A. |

## Not ported

* SoapySDR module registration: SoapySDR has no C ABI for modules, so a Rust
  driver cannot register itself; a thin C++ shim calling into this crate
  through a C ABI would be needed.
* Logging: the C++ driver logged most failures and continued; the port
  returns errors instead.
