#![cfg(loom)]
//! Loom-based concurrency verification tests.
//!
//! These tests exhaustively explore all thread interleavings to catch
//! memory ordering bugs that conventional tests miss (especially on x86
//! where the strong memory model hides them).
//!
//! Run with: `RUSTFLAGS="--cfg loom" cargo test --test loom_tests --release`

use calvera::loom_api::{Barrier, Cursor, NONE, ProducerBarrier, RingBuffer};
use calvera::UniProducerBarrier;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicI64, Ordering, fence};

/// Two threads race to claim sequences via `compare_exchange` on the same Cursor.
/// Verifies lock-free CAS coordination (multi-producer sequence claiming).
#[test]
fn cursor_cas_contention() {
    loom::model(|| {
        let cursor = Arc::new(Cursor::new(NONE));

        let c1 = Arc::clone(&cursor);
        let c2 = Arc::clone(&cursor);

        let t1 = loom::thread::spawn(move || {
            // Try to claim sequence 0 (CAS from -1 to 0).
            c1.compare_exchange(-1, 0)
        });

        let t2 = loom::thread::spawn(move || {
            // Also try to claim sequence 0 (CAS from -1 to 0).
            c2.compare_exchange(-1, 0)
        });

        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();

        // Exactly one must win the CAS, the other must fail.
        match (r1, r2) {
            (Ok(-1), Err(0)) => {} // t1 won
            (Err(0), Ok(-1)) => {} // t2 won
            other => panic!("Unexpected CAS results: {:?}", other),
        }

        // Final value must be 0 regardless of who won.
        assert_eq!(cursor.relaxed_value(), 0);
    });
}

/// Producer writes data to a ring buffer slot and publishes with Release store.
/// Consumer reads after seeing the sequence with Acquire fence.
/// Verifies the fence guarantees data visibility.
#[test]
fn publish_consume_data_visibility() {
    loom::model(|| {
        let ring_buffer = Arc::new(RingBuffer::new(2, || AtomicI64::new(0)));
        let producer_barrier = Arc::new(UniProducerBarrier::new());

        let rb = Arc::clone(&ring_buffer);
        let pb = Arc::clone(&producer_barrier);

        // Producer thread: write data then publish.
        let producer = loom::thread::spawn(move || {
            // Write data to slot 0.
            let ptr = rb.get(0);
            unsafe { &*ptr }.store(42, Ordering::Relaxed);

            // Publish sequence 0 (this does a Release store on the barrier cursor).
            pb.publish(0);
        });

        // Consumer: spin until we see the published sequence, then read.
        loop {
            let available = producer_barrier.get_after(0);
            if available >= 0 {
                // Acquire fence to ensure we see the data written before publish.
                fence(Ordering::Acquire);

                let ptr = ring_buffer.get(0);
                let value = unsafe { &*ptr }.load(Ordering::Relaxed);
                assert_eq!(value, 42, "Data must be visible after acquire fence");
                break;
            }
            loom::thread::yield_now();
        }

        producer.join().unwrap();
    });
}

/// Minimal SPSC round trip: ring buffer of size 2, publish 1 event,
/// consume it via the barrier protocol. Verifies full sequence protocol.
#[test]
fn spsc_round_trip() {
    loom::model(|| {
        let ring_buffer = Arc::new(RingBuffer::new(2, || AtomicI64::new(-1)));
        let producer_barrier = Arc::new(UniProducerBarrier::new());
        let consumer_cursor = Arc::new(Cursor::new(NONE));

        let rb = Arc::clone(&ring_buffer);
        let pb = Arc::clone(&producer_barrier);

        // Producer: write event, publish.
        let producer = loom::thread::spawn(move || {
            let ptr = rb.get(0);
            unsafe { &*ptr }.store(99, Ordering::Relaxed);
            pb.publish(0);
        });

        // Consumer: wait for sequence 0, read, update cursor.
        loop {
            let available = producer_barrier.get_after(0);
            if available >= 0 {
                fence(Ordering::Acquire);

                let ptr = ring_buffer.get(0);
                let value = unsafe { &*ptr }.load(Ordering::Relaxed);
                assert_eq!(value, 99);

                // Signal that we've consumed up to sequence 0.
                consumer_cursor.store(0);
                break;
            }
            loom::thread::yield_now();
        }

        producer.join().unwrap();

        // Verify consumer cursor advanced.
        assert_eq!(consumer_cursor.relaxed_value(), 0);
    });
}

/// Two threads publish to a UniProducerBarrier sequentially (one after the other),
/// verifying that the barrier correctly tracks the latest published sequence
/// under concurrent reads.
#[test]
fn barrier_concurrent_read_write() {
    loom::model(|| {
        let barrier = Arc::new(UniProducerBarrier::new());

        let b1 = Arc::clone(&barrier);
        let b2 = Arc::clone(&barrier);

        // Writer: publishes sequence 0 then 1.
        let writer = loom::thread::spawn(move || {
            b1.publish(0);
            b1.publish(1);
        });

        // Reader: reads the barrier concurrently.
        let reader = loom::thread::spawn(move || {
            let val = b2.get_after(0);
            // Must see either NONE (-1), 0, or 1 — never garbage.
            assert!(
                val == NONE || val == 0 || val == 1,
                "Unexpected barrier value: {}",
                val
            );
            val
        });

        writer.join().unwrap();
        let _read_val = reader.join().unwrap();

        // After both threads complete, barrier must be at 1.
        assert_eq!(barrier.get_after(0), 1);
    });
}
