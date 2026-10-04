//! Conversion between the HackRF's native interleaved CS8 samples and the
//! host-side formats (CS8, CS16, CF32, CF64).
//!
//! Reading is byte-for-byte that of the C++ driver's `readbuf`: integers
//! shift by 8 bits, floats scale by 1/127. Writing scales by 127 and rounds
//! to the nearest integer where the C++ `writebuf` truncated toward zero
//! (which made a CS8 → CF32 → CS8 round trip lose one LSB on almost every
//! value, BUGS.md item C3), and saturates where C++ invoked undefined
//! behaviour (BUGS.md item C2).

use crate::types::StreamFormat;

/// Scale factor between native CS8 and floating point samples.
///
/// The C++ driver divided by 127 while reporting a full scale of 128 from
/// `getNativeStreamFormat()`; both values are kept for equivalence
/// (BUGS.md, item C1).
pub const CONVERSION_SCALE: f64 = 127.0;
/// Full-scale value advertised for the native CS8 format.
pub const NATIVE_FULL_SCALE: f64 = 128.0;

/// A scalar sample element type that can be converted to and from native CS8.
///
/// Buffers are interleaved I/Q, so one complex sample is two elements.
pub trait Sample: Copy + Default + Send + Sync + 'static {
    /// The stream format this element type implements.
    const FORMAT: StreamFormat;
    /// Convert one native element.
    fn from_native(v: i8) -> Self;
    /// Convert into one native element.
    fn to_native(self) -> i8;
}

impl Sample for i8 {
    const FORMAT: StreamFormat = StreamFormat::CS8;
    #[inline]
    fn from_native(v: i8) -> i8 {
        v
    }
    #[inline]
    fn to_native(self) -> i8 {
        self
    }
}

impl Sample for i16 {
    const FORMAT: StreamFormat = StreamFormat::CS16;
    #[inline]
    fn from_native(v: i8) -> i16 {
        (v as i16) << 8
    }
    #[inline]
    fn to_native(self) -> i8 {
        (self >> 8) as i8
    }
}

impl Sample for f32 {
    const FORMAT: StreamFormat = StreamFormat::CF32;
    #[inline]
    fn from_native(v: i8) -> f32 {
        (v as f64 / CONVERSION_SCALE) as f32
    }
    #[inline]
    fn to_native(self) -> i8 {
        // `round()` then `as`: nearest integer, saturating; NaN becomes 0.
        (self as f64 * CONVERSION_SCALE).round() as i8
    }
}

impl Sample for f64 {
    const FORMAT: StreamFormat = StreamFormat::CF64;
    #[inline]
    fn from_native(v: i8) -> f64 {
        v as f64 / CONVERSION_SCALE
    }
    #[inline]
    fn to_native(self) -> i8 {
        (self * CONVERSION_SCALE).round() as i8
    }
}

/// Convert native elements into host elements. Both slices must have the same
/// length (in elements, two per complex sample).
pub fn native_to_samples<T: Sample>(src: &[i8], dst: &mut [T]) {
    assert_eq!(src.len(), dst.len(), "native_to_samples length mismatch");
    for (d, s) in dst.iter_mut().zip(src) {
        *d = T::from_native(*s);
    }
}

/// Convert host elements into native elements. Both slices must have the same
/// length (in elements, two per complex sample).
pub fn samples_to_native<T: Sample>(src: &[T], dst: &mut [i8]) {
    assert_eq!(src.len(), dst.len(), "samples_to_native length mismatch");
    for (d, s) in dst.iter_mut().zip(src) {
        *d = s.to_native();
    }
}

/// A mutable host buffer of any supported format (interleaved I/Q).
#[derive(Debug)]
pub enum SampleBufMut<'a> {
    /// CS8 elements.
    Cs8(&'a mut [i8]),
    /// CS16 elements.
    Cs16(&'a mut [i16]),
    /// CF32 elements.
    Cf32(&'a mut [f32]),
    /// CF64 elements.
    Cf64(&'a mut [f64]),
}

impl<'a> SampleBufMut<'a> {
    /// Format of the buffer.
    pub fn format(&self) -> StreamFormat {
        match self {
            SampleBufMut::Cs8(_) => StreamFormat::CS8,
            SampleBufMut::Cs16(_) => StreamFormat::CS16,
            SampleBufMut::Cf32(_) => StreamFormat::CF32,
            SampleBufMut::Cf64(_) => StreamFormat::CF64,
        }
    }

    /// Number of elements (two per complex sample).
    pub fn len_elements(&self) -> usize {
        match self {
            SampleBufMut::Cs8(b) => b.len(),
            SampleBufMut::Cs16(b) => b.len(),
            SampleBufMut::Cf32(b) => b.len(),
            SampleBufMut::Cf64(b) => b.len(),
        }
    }

    /// Number of whole complex samples the buffer can hold.
    pub fn num_samples(&self) -> usize {
        self.len_elements() / 2
    }

    /// Convert `src` (native elements) into the buffer starting at complex
    /// sample `offset_samples`.
    pub fn write_from_native(&mut self, offset_samples: usize, src: &[i8]) {
        let start = offset_samples * 2;
        let end = start + src.len();
        match self {
            SampleBufMut::Cs8(b) => native_to_samples(src, &mut b[start..end]),
            SampleBufMut::Cs16(b) => native_to_samples(src, &mut b[start..end]),
            SampleBufMut::Cf32(b) => native_to_samples(src, &mut b[start..end]),
            SampleBufMut::Cf64(b) => native_to_samples(src, &mut b[start..end]),
        }
    }
}

impl<'a, T: Sample> From<&'a mut [T]> for SampleBufMut<'a> {
    fn from(buf: &'a mut [T]) -> SampleBufMut<'a> {
        // SAFETY: `T::FORMAT` uniquely identifies the concrete type, so the
        // pointer cast re-types the slice to exactly its own type.
        unsafe {
            let ptr = buf.as_mut_ptr();
            let len = buf.len();
            match T::FORMAT {
                StreamFormat::CS8 => {
                    SampleBufMut::Cs8(std::slice::from_raw_parts_mut(ptr as *mut i8, len))
                }
                StreamFormat::CS16 => {
                    SampleBufMut::Cs16(std::slice::from_raw_parts_mut(ptr as *mut i16, len))
                }
                StreamFormat::CF32 => {
                    SampleBufMut::Cf32(std::slice::from_raw_parts_mut(ptr as *mut f32, len))
                }
                StreamFormat::CF64 => {
                    SampleBufMut::Cf64(std::slice::from_raw_parts_mut(ptr as *mut f64, len))
                }
            }
        }
    }
}

/// An immutable host buffer of any supported format (interleaved I/Q).
#[derive(Clone, Copy, Debug)]
pub enum SampleBuf<'a> {
    /// CS8 elements.
    Cs8(&'a [i8]),
    /// CS16 elements.
    Cs16(&'a [i16]),
    /// CF32 elements.
    Cf32(&'a [f32]),
    /// CF64 elements.
    Cf64(&'a [f64]),
}

impl<'a> SampleBuf<'a> {
    /// Format of the buffer.
    pub fn format(&self) -> StreamFormat {
        match self {
            SampleBuf::Cs8(_) => StreamFormat::CS8,
            SampleBuf::Cs16(_) => StreamFormat::CS16,
            SampleBuf::Cf32(_) => StreamFormat::CF32,
            SampleBuf::Cf64(_) => StreamFormat::CF64,
        }
    }

    /// Number of elements (two per complex sample).
    pub fn len_elements(&self) -> usize {
        match self {
            SampleBuf::Cs8(b) => b.len(),
            SampleBuf::Cs16(b) => b.len(),
            SampleBuf::Cf32(b) => b.len(),
            SampleBuf::Cf64(b) => b.len(),
        }
    }

    /// Number of whole complex samples in the buffer.
    pub fn num_samples(&self) -> usize {
        self.len_elements() / 2
    }

    /// Convert the buffer starting at complex sample `offset_samples` into
    /// `dst` (native elements); `dst.len()` elements are converted.
    pub fn read_to_native(&self, offset_samples: usize, dst: &mut [i8]) {
        let start = offset_samples * 2;
        let end = start + dst.len();
        match self {
            SampleBuf::Cs8(b) => samples_to_native(&b[start..end], dst),
            SampleBuf::Cs16(b) => samples_to_native(&b[start..end], dst),
            SampleBuf::Cf32(b) => samples_to_native(&b[start..end], dst),
            SampleBuf::Cf64(b) => samples_to_native(&b[start..end], dst),
        }
    }
}

impl<'a, T: Sample> From<&'a [T]> for SampleBuf<'a> {
    fn from(buf: &'a [T]) -> SampleBuf<'a> {
        // SAFETY: see `SampleBufMut::from`.
        unsafe {
            let ptr = buf.as_ptr();
            let len = buf.len();
            match T::FORMAT {
                StreamFormat::CS8 => {
                    SampleBuf::Cs8(std::slice::from_raw_parts(ptr as *const i8, len))
                }
                StreamFormat::CS16 => {
                    SampleBuf::Cs16(std::slice::from_raw_parts(ptr as *const i16, len))
                }
                StreamFormat::CF32 => {
                    SampleBuf::Cf32(std::slice::from_raw_parts(ptr as *const f32, len))
                }
                StreamFormat::CF64 => {
                    SampleBuf::Cf64(std::slice::from_raw_parts(ptr as *const f64, len))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_views_keep_data() {
        let mut v = vec![1i16, -2, 3, -4];
        let view = SampleBufMut::from(v.as_mut_slice());
        assert_eq!(view.format(), StreamFormat::CS16);
        assert_eq!(view.num_samples(), 2);
        match view {
            SampleBufMut::Cs16(s) => assert_eq!(s, &[1, -2, 3, -4]),
            _ => panic!(),
        }
        let f = [0.5f32, -0.5];
        match SampleBuf::from(&f[..]) {
            SampleBuf::Cf32(s) => assert_eq!(s, &[0.5, -0.5]),
            _ => panic!(),
        }
    }
}
