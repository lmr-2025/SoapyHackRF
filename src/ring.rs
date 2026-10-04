//! A fixed-size ring of transfer buffers with explicit slot ownership.
//!
//! Each slot moves through the states *free → filling → filled → draining →
//! free*. The producer (`begin_produce` / `end_produce`) and the consumer
//! (`begin_consume` / `end_consume`) are different actors: for RX the USB
//! callback produces and the user consumes; for TX the user produces and the
//! USB callback consumes.
//!
//! Free and filled slots are kept in explicit queues rather than derived from
//! head/tail indices, so a slot that is *filling* or *draining* is never
//! handed out again whatever order slots are released in. This fixes the C++
//! driver's RX accounting, where the callback could overwrite the buffer the
//! reader was still copying out of (BUGS.md, S1).
//!
//! Slot memory is addressed through raw pointers so that a reference to one
//! slot can be held by the user while the callback thread, holding the mutex
//! that protects the `Ring`, writes a *different* slot.

use std::collections::VecDeque;
use std::ptr::NonNull;

struct Slot {
    ptr: NonNull<[i8]>,
    valid: usize,
}

/// Ring of equally sized byte buffers.
pub struct Ring {
    slots: Vec<Slot>,
    slot_len: usize,
    /// Slots nobody owns, in the order they will be handed to the producer.
    free: VecDeque<usize>,
    /// Slots with data, oldest first.
    filled: VecDeque<usize>,
    producing: usize,
    consuming: usize,
    overflows: u64,
}

// SAFETY: the raw slot pointers are owned allocations; access to the slots
// is serialised by the state machine and the mutex that owns the `Ring`.
unsafe impl Send for Ring {}

impl Ring {
    /// Allocate `num_slots` zero-filled buffers of `slot_len` bytes each.
    pub fn new(num_slots: usize, slot_len: usize) -> Ring {
        assert!(num_slots >= 1, "a ring needs at least one slot");
        let slots = (0..num_slots)
            .map(|_| {
                let boxed = vec![0i8; slot_len].into_boxed_slice();
                Slot {
                    ptr: NonNull::new(Box::into_raw(boxed)).expect("box pointer"),
                    valid: 0,
                }
            })
            .collect();
        Ring {
            slots,
            slot_len,
            free: (0..num_slots).collect(),
            filled: VecDeque::with_capacity(num_slots),
            producing: 0,
            consuming: 0,
            overflows: 0,
        }
    }

    /// Number of slots.
    pub fn num_slots(&self) -> usize {
        self.slots.len()
    }

    /// Bytes per slot.
    pub fn slot_len(&self) -> usize {
        self.slot_len
    }

    /// Slots that are filled and waiting for the consumer.
    pub fn filled(&self) -> usize {
        self.filled.len()
    }

    /// Slots that are neither filled nor currently held by either side.
    pub fn free(&self) -> usize {
        self.free.len()
    }

    /// Slots currently held by the producer.
    pub fn producing(&self) -> usize {
        self.producing
    }

    /// Slots currently held by the consumer.
    pub fn consuming(&self) -> usize {
        self.consuming
    }

    /// Number of times [`Ring::drop_oldest`] discarded data.
    pub fn overflows(&self) -> u64 {
        self.overflows
    }

    /// Sum of the valid bytes of all filled slots.
    pub fn queued_bytes(&self) -> usize {
        self.filled.iter().map(|&s| self.slots[s].valid).sum()
    }

    /// Valid bytes recorded for `slot`.
    pub fn valid(&self, slot: usize) -> usize {
        self.slots[slot].valid
    }

    /// Raw pointer and capacity of `slot`.
    ///
    /// The caller must only dereference it while it owns the slot (between
    /// `begin_*` and `end_*`) and must not outlive the `Ring`.
    pub fn slot_ptr(&self, slot: usize) -> (*mut i8, usize) {
        (self.slots[slot].ptr.as_ptr() as *mut i8, self.slot_len)
    }

    /// Mutable view of a slot the caller currently owns as producer.
    ///
    /// # Safety
    /// `slot` must have been returned by [`Ring::begin_produce`] and not yet
    /// passed to [`Ring::end_produce`]; no other reference to it may exist.
    pub unsafe fn slot_mut(&mut self, slot: usize) -> &mut [i8] {
        let (ptr, len) = self.slot_ptr(slot);
        // SAFETY: guaranteed by the caller (see the doc comment).
        unsafe { std::slice::from_raw_parts_mut(ptr, len) }
    }

    /// Shared view of the valid part of a slot the caller owns as consumer.
    ///
    /// # Safety
    /// `slot` must have been returned by [`Ring::begin_consume`] and not yet
    /// passed to [`Ring::end_consume`].
    pub unsafe fn slot_valid(&self, slot: usize) -> &[i8] {
        let (ptr, _) = self.slot_ptr(slot);
        // SAFETY: guaranteed by the caller (see the doc comment).
        unsafe { std::slice::from_raw_parts(ptr, self.slots[slot].valid) }
    }

    /// Take the next free slot for filling.
    pub fn begin_produce(&mut self) -> Option<usize> {
        let slot = self.free.pop_front()?;
        self.producing += 1;
        Some(slot)
    }

    /// Mark a slot taken with [`Ring::begin_produce`] as filled with
    /// `valid` bytes.
    pub fn end_produce(&mut self, slot: usize, valid: usize) {
        debug_assert!(self.producing > 0, "end_produce without begin_produce");
        debug_assert!(valid <= self.slot_len);
        self.slots[slot].valid = valid.min(self.slot_len);
        self.producing -= 1;
        self.filled.push_back(slot);
    }

    /// Give back a slot taken with [`Ring::begin_produce`] without filling
    /// it; it is handed out again first.
    pub fn cancel_produce(&mut self, slot: usize) {
        debug_assert!(self.producing > 0, "cancel_produce without begin_produce");
        self.producing -= 1;
        self.free.push_front(slot);
    }

    /// Take the oldest filled slot for draining.
    pub fn begin_consume(&mut self) -> Option<usize> {
        let slot = self.filled.pop_front()?;
        self.consuming += 1;
        Some(slot)
    }

    /// Return a slot taken with [`Ring::begin_consume`] to the free pool.
    pub fn end_consume(&mut self, slot: usize) {
        debug_assert!(self.consuming > 0, "end_consume without begin_consume");
        self.slots[slot].valid = 0;
        self.consuming -= 1;
        self.free.push_back(slot);
    }

    /// Discard the oldest filled slot (overflow policy of the C++ driver).
    /// Returns `false` if nothing was filled.
    pub fn drop_oldest(&mut self) -> bool {
        match self.filled.pop_front() {
            Some(slot) => {
                self.slots[slot].valid = 0;
                self.free.push_back(slot);
                self.overflows += 1;
                true
            }
            None => false,
        }
    }

    /// Discard all queued data. Slots currently held by the producer or the
    /// consumer stay held, so a buffer the user is still reading is never
    /// handed out again.
    pub fn reset(&mut self) {
        while let Some(slot) = self.filled.pop_front() {
            self.slots[slot].valid = 0;
            self.free.push_back(slot);
        }
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        for s in self.slots.drain(..) {
            // SAFETY: created by `Box::into_raw` in `Ring::new`.
            unsafe { drop(Box::from_raw(s.ptr.as_ptr())) };
        }
    }
}

impl std::fmt::Debug for Ring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ring")
            .field("num_slots", &self.slots.len())
            .field("slot_len", &self.slot_len)
            .field("free", &self.free)
            .field("filled", &self.filled.len())
            .field("producing", &self.producing)
            .field("consuming", &self.consuming)
            .finish()
    }
}
