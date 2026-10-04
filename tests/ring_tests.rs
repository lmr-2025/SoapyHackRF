//! The transfer ring: ownership state machine, overflow policy and the
//! held-slot regression from the C++ RX callback.

use soapyhackrf::ring::Ring;

fn fill(ring: &mut Ring, slot: usize, byte: i8) {
    unsafe { ring.slot_mut(slot).iter_mut().for_each(|b| *b = byte) };
}

#[test]
fn fifo_order_and_accounting() {
    let mut r = Ring::new(3, 8);
    assert_eq!(
        (r.num_slots(), r.slot_len(), r.free(), r.filled()),
        (3, 8, 3, 0)
    );
    for i in 0..3 {
        let s = r.begin_produce().unwrap();
        assert_eq!(s, i);
        assert_eq!(r.producing(), 1);
        fill(&mut r, s, i as i8 + 1);
        r.end_produce(s, 8 - i);
        assert_eq!(r.filled(), i + 1);
    }
    assert_eq!(r.begin_produce(), None, "full");
    assert_eq!(r.queued_bytes(), 8 + 7 + 6);
    for i in 0..3 {
        let s = r.begin_consume().unwrap();
        assert_eq!(s, i);
        assert_eq!(r.consuming(), 1);
        assert_eq!(r.valid(s), 8 - i);
        assert_eq!(
            unsafe { r.slot_valid(s) },
            vec![i as i8 + 1; 8 - i].as_slice()
        );
        r.end_consume(s);
        assert_eq!(r.valid(s), 0);
    }
    assert_eq!(r.begin_consume(), None, "empty");
    assert_eq!(r.free(), 3);
}

#[test]
fn held_slots_are_never_handed_out_again() {
    // BUGS.md S1: the C++ callback computed the write slot from head+count
    // while the reader had advanced head without decrementing count, so a
    // transfer could land in the buffer the reader was still copying from.
    let n = 4;
    let mut r = Ring::new(n, 4);
    // Fill n-1 slots.
    for i in 0..n - 1 {
        let s = r.begin_produce().unwrap();
        fill(&mut r, s, i as i8);
        r.end_produce(s, 4);
    }
    // Reader holds the oldest.
    let held = r.begin_consume().unwrap();
    assert_eq!(held, 0);
    assert_eq!(r.free(), 1);
    // Producer fills the one free slot, then overflows.
    let s = r.begin_produce().unwrap();
    assert_ne!(s, held);
    fill(&mut r, s, 100);
    r.end_produce(s, 4);
    assert_eq!(r.free(), 0);
    assert_eq!(r.begin_produce(), None);
    assert!(r.drop_oldest());
    let s = r.begin_produce().unwrap();
    assert_ne!(s, held, "the held slot must not be overwritten");
    fill(&mut r, s, 101);
    r.end_produce(s, 4);
    assert_eq!(unsafe { r.slot_valid(held) }, &[0, 0, 0, 0]);
    r.end_consume(held);
    // Remaining data, oldest first; the dropped transfer is gone.
    let order: Vec<i8> = std::iter::from_fn(|| {
        let s = r.begin_consume()?;
        let v = unsafe { r.slot_valid(s) }[0];
        r.end_consume(s);
        Some(v)
    })
    .collect();
    assert_eq!(order, vec![2, 100, 101], "oldest unread (1) was dropped");
    assert_eq!(r.overflows(), 1);
}

#[test]
fn drop_oldest_with_nothing_filled_reports_false() {
    let mut r = Ring::new(2, 2);
    assert!(!r.drop_oldest());
    let a = r.begin_produce().unwrap();
    let b = r.begin_produce().unwrap();
    assert_eq!(r.free(), 0);
    assert!(!r.drop_oldest(), "slots being filled cannot be dropped");
    r.end_produce(a, 2);
    r.end_produce(b, 2);
    assert!(r.drop_oldest());
    assert_eq!(r.filled(), 1);
    assert_eq!(r.begin_consume(), Some(1));
}

#[test]
fn cancel_produce_returns_the_slot() {
    let mut r = Ring::new(2, 2);
    let s = r.begin_produce().unwrap();
    assert_eq!(r.free(), 1);
    r.cancel_produce(s);
    assert_eq!(r.free(), 2);
    assert_eq!(r.begin_produce(), Some(0), "the same slot is reused next");
}

#[test]
fn reset_forgets_everything() {
    let mut r = Ring::new(3, 2);
    let s = r.begin_produce().unwrap();
    r.end_produce(s, 2);
    let s = r.begin_produce().unwrap();
    r.end_produce(s, 1);
    let _held = r.begin_consume().unwrap();
    r.reset();
    assert_eq!(
        (r.free(), r.filled(), r.producing(), r.consuming()),
        (3, 0, 0, 0)
    );
    assert_eq!(r.queued_bytes(), 0);
    assert_eq!(r.begin_produce(), Some(0));
}

#[test]
fn wraps_around_many_times() {
    let mut r = Ring::new(5, 16);
    let mut expect = 0u32;
    let mut next = 0u32;
    for round in 0..1000 {
        let burst = (round % 5) + 1;
        for _ in 0..burst {
            if let Some(s) = r.begin_produce() {
                let bytes = next.to_le_bytes().map(|b| b as i8);
                unsafe { r.slot_mut(s)[..4].copy_from_slice(&bytes) };
                r.end_produce(s, 4);
                next += 1;
            }
        }
        while let Some(s) = r.begin_consume() {
            let v = unsafe { r.slot_valid(s) };
            assert_eq!(v.len(), 4);
            let got = u32::from_le_bytes(
                v.iter()
                    .map(|&b| b as u8)
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(got, expect);
            expect += 1;
            r.end_consume(s);
        }
    }
    assert_eq!(expect, next);
}

#[test]
fn end_produce_caps_valid_length() {
    let mut r = Ring::new(1, 4);
    let s = r.begin_produce().unwrap();
    r.end_produce(s, 4);
    assert_eq!(r.valid(s), 4);
    let fmt = format!("{r:?}");
    assert!(fmt.contains("filled: 1"), "{fmt}");
    assert!(fmt.contains("free: []"), "{fmt}");
}

#[test]
#[should_panic(expected = "at least one slot")]
fn zero_slots_is_rejected() {
    let _ = Ring::new(0, 4);
}
