//! Lock-free single-producer/single-consumer ring buffer of f32 samples.
//!
//! The capture callback is the only thread that calls [`SpscRing::push`] and
//! the render callback is the only thread that calls [`SpscRing::pop`]. Only
//! atomics are involved (no locks, no allocation, no syscalls), so it is safe
//! to use on real-time threads. Sample slots are `AtomicU32`s holding f32 bit
//! patterns, accessed with `Relaxed` ordering: all synchronization comes from
//! the `head`/`tail` atomics, and a slot is only reused after the consumer
//! has already read it.
//!
//! When the buffer is full, the producer drops the *oldest* sample so the
//! consumer always gets the freshest audio (blocking would be worse).

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

#[derive(Debug)]
pub struct SpscRing {
    buf: Vec<AtomicU32>,
    cap: usize,
    /// Monotonic index of the next slot the producer will write.
    /// Only `push` ever stores this.
    head: AtomicUsize,
    /// Monotonic index of the next slot the consumer will read.
    /// `pop` stores this; `push` may also advance it when dropping samples
    /// to make room.
    tail: AtomicUsize,
}

impl SpscRing {
    /// Create a ring; capacity is rounded up to a power of two (min 8).
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(8).next_power_of_two();
        Self {
            buf: (0..cap).map(|_| AtomicU32::new(0)).collect(),
            cap,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Number of samples currently queued.
    #[allow(dead_code)]
    #[inline]
    pub fn len(&self) -> usize {
        self.head.load(Ordering::Acquire) - self.tail.load(Ordering::Acquire)
    }

    /// `true` when no samples are queued.
    #[allow(dead_code)]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Buffer capacity in samples.
    #[allow(dead_code)]
    #[inline]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Enqueue samples. If the ring is full, the oldest samples are dropped.
    #[inline]
    pub fn push(&self, samples: &[f32]) {
        for &s in samples {
            let head = self.head.load(Ordering::Relaxed);
            if head - self.tail.load(Ordering::Acquire) >= self.cap {
                self.tail.fetch_add(1, Ordering::Release);
            }
            self.buf[head % self.cap].store(s.to_bits(), Ordering::Relaxed);
            self.head.store(head + 1, Ordering::Release);
        }
    }

    /// Dequeue up to `out.len()` samples into `out`; returns how many were
    /// written.
    #[inline]
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let mut n = 0;
        for slot in out.iter_mut() {
            let tail = self.tail.load(Ordering::Relaxed);
            if tail >= self.head.load(Ordering::Acquire) {
                break;
            }
            *slot = f32::from_bits(self.buf[tail % self.cap].load(Ordering::Relaxed));
            self.tail.store(tail + 1, Ordering::Release);
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_in_order() {
        let ring = SpscRing::new(8);
        ring.push(&[0.0, 1.0, 2.0, 3.0, 4.0]);
        let mut out = [99.0f32; 10];
        let n = ring.pop(&mut out);
        assert_eq!(n, 5);
        assert_eq!(&out[..5], &[0.0, 1.0, 2.0, 3.0, 4.0]);
        assert_eq!(ring.pop(&mut out), 0);
        assert!(ring.is_empty());
    }

    #[test]
    fn wraps_around() {
        let ring = SpscRing::new(8);
        ring.push(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        let mut out = [0.0f32; 5];
        assert_eq!(ring.pop(&mut out), 5);
        assert_eq!(&out, &[0.0, 1.0, 2.0, 3.0, 4.0]);
        ring.push(&[7.0, 8.0, 9.0, 10.0, 11.0]);
        let mut out = [0.0f32; 8];
        assert_eq!(ring.pop(&mut out), 7);
        assert_eq!(&out[..7], &[5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0]);
    }

    #[test]
    fn drops_oldest_when_full() {
        let ring = SpscRing::new(8);
        ring.push(&(0..12).map(|x| x as f32).collect::<Vec<_>>());
        let mut out = [0.0f32; 8];
        assert_eq!(ring.pop(&mut out), 8);
        assert_eq!(&out, &[4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0]);
    }

    #[test]
    fn capacity_rounds_up_to_power_of_two() {
        assert_eq!(SpscRing::new(3).capacity(), 8);
        assert_eq!(SpscRing::new(8).capacity(), 8);
        assert_eq!(SpscRing::new(10).capacity(), 16);
    }
}
