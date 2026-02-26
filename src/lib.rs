mod builder;
mod consumer;
mod disruptor;
mod errors;
#[path = "_periphery/mod.rs"]
mod periphery;
mod producer;
mod ring_buffer;
mod sync;

use periphery::affinity;
use periphery::barrier;
use periphery::cursor;
use periphery::wait_strategies;

pub type Sequence = i64;

pub use crate::builder::{
    ProcessorSettings, build_multi_producer_unchecked, build_uni_producer_unchecked,
};
pub use crate::consumer::unmanaged::{EventGuard, EventPoller};
pub use crate::consumer::{MultiConsumerBarrier, UniConsumerBarrier};
pub use crate::disruptor::Disruptor;
pub use crate::errors::EPolling;
pub use crate::producer::{
    Producer,
    multi::{MultiProducer, MultiProducerBarrier},
    uni::{UniProducer, UniProducerBarrier},
};
pub use crate::wait_strategies::{BusySpin, BusySpinWithSpinLoopHint};

#[cfg(loom)]
pub mod loom_api {
    pub use crate::barrier::{Barrier, NONE};
    pub use crate::cursor::Cursor;
    pub use crate::producer::ProducerBarrier;
    pub use crate::ring_buffer::RingBuffer;
}

#[cfg(test)]
mod tests {
    use crate::builder::build_multi_producer_unchecked;
    use crate::errors::{EMissingFreeSlots, EPolling, ERingBufferFull};
    use crate::producer::Producer;

    use super::*;
    // use producer::EMissingFreeSlots;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::rc::Rc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::{Arc, mpsc};
    use std::thread;

    #[derive(Debug)]
    struct Event {
        num: i64,
    }

    fn factory() -> impl Fn() -> Event {
        || Event { num: -1 }
    }

    #[test]
    #[should_panic(expected = "Ring buffer size must be power of 2.")]
    fn size_not_a_factor_of_2() {
        build_uni_producer_unchecked(3, || 0, BusySpin);
    }

    #[test]
    fn spsc_full_ringbuffer() {
        let (s, r) = mpsc::channel();
        let barrier = Arc::new(AtomicBool::new(true));
        let processor = {
            let barrier = Arc::clone(&barrier);
            move |e: &Event, _, _| {
                while barrier.load(Relaxed) { /* Wait. */ }
                s.send(e.num).expect("Should be able to send.");
            }
        };
        let mut producer = build_uni_producer_unchecked(4, factory(), BusySpinWithSpinLoopHint)
            .handle_events_with(processor)
            .build();

        for i in 0..4 {
            producer.try_publish(|e| e.num = i).expect("Should publish");
        }
        // Now ring buffer is full.
        assert_eq!(
            ERingBufferFull,
            producer.try_publish(|e| e.num = 4).err().unwrap()
        );
        // And it stays full.
        assert_eq!(
            ERingBufferFull,
            producer.try_publish(|e| e.num = 4).err().unwrap()
        );
        // Until the processor continues reading events.
        barrier.store(false, Relaxed);
        producer.publish(|e| e.num = 4);

        drop(producer);
        let result: Vec<_> = r.iter().collect();
        assert_eq!(result, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn mpsc_full_ringbuffer() {
        let (s, r) = mpsc::channel();
        let barrier = Arc::new(AtomicBool::new(true));
        let processor = {
            let barrier = Arc::clone(&barrier);
            move |e: &Event, _, _| {
                while barrier.load(Relaxed) { /* Wait. */ }
                s.send(e.num).expect("Should be able to send.");
            }
        };
        let mut producer1 = build_multi_producer_unchecked(64, factory(), BusySpinWithSpinLoopHint)
            .handle_events_with(processor)
            .build();

        let mut producer2 = producer1.clone();

        for i in 0..64 {
            producer1
                .try_publish(|e| e.num = i)
                .expect("Should publish");
        }

        // Now ring buffer is full.
        assert_eq!(
            ERingBufferFull,
            producer1.try_publish(|e| e.num = 4).err().unwrap()
        );
        // And it is full also as seen from second producer.
        assert_eq!(
            ERingBufferFull,
            producer2.try_publish(|e| e.num = 4).err().unwrap()
        );
        // Until the processor continues reading events.
        barrier.store(false, Relaxed);
        producer1.publish(|e| e.num = 64);
        producer2.publish(|e| e.num = 65);

        drop(producer1);
        drop(producer2);
        let result: Vec<_> = r.iter().collect();
        assert_eq!(result, (0..=65).into_iter().collect::<Vec<i64>>());
    }

    #[test]
    fn spsc_insufficient_space_for_batch_publication() {
        let (s, r) = mpsc::channel();
        let barrier = Arc::new(AtomicBool::new(true));
        let processor = {
            let barrier = Arc::clone(&barrier);
            move |e: &Event, _, _| {
                while barrier.load(Relaxed) { /* Wait. */ }
                s.send(e.num).expect("Should be able to send.");
            }
        };
        let mut producer = build_uni_producer_unchecked(4, factory(), BusySpin)
            .handle_events_with(processor)
            .build();

        for i in 0..2 {
            producer.publish(|e| e.num = i);
        }
        assert_eq!(
            EMissingFreeSlots(2),
            producer.try_batch_publish(4, |_iter| {}).err().unwrap()
        );
        assert_eq!(
            EMissingFreeSlots(100),
            producer.try_batch_publish(102, |_iter| {}).err().unwrap()
        );

        barrier.store(false, Relaxed);
        producer
            .try_batch_publish(2, |iter| {
                for e in iter {
                    e.num = 2;
                }
            })
            .expect("Batch publication should now succeed.");

        drop(producer);
        let result: Vec<_> = r.iter().collect();
        assert_eq!(result, [0, 1, 2, 2]);
    }

    #[test]
    fn mpsc_insufficient_space_for_batch_publication() {
        let (s, r) = mpsc::channel();
        let barrier = Arc::new(AtomicBool::new(true));
        let processor = {
            let barrier = Arc::clone(&barrier);
            move |e: &Event, _, _| {
                while barrier.load(Relaxed) { /* Wait. */ }
                s.send(e.num).expect("Should be able to send.");
            }
        };
        let mut producer1 = build_multi_producer_unchecked(64, factory(), BusySpin)
            .handle_events_with(processor)
            .build();
        let mut producer2 = producer1.clone();

        for i in 0..58 {
            producer1.publish(|e| e.num = i);
        }
        assert_eq!(
            EMissingFreeSlots(2),
            producer1.try_batch_publish(8, |_iter| {}).err().unwrap()
        );
        assert_eq!(
            EMissingFreeSlots(100),
            producer1.try_batch_publish(106, |_iter| {}).err().unwrap()
        );
        assert_eq!(
            EMissingFreeSlots(2),
            producer2.try_batch_publish(8, |_iter| {}).err().unwrap()
        );
        assert_eq!(
            EMissingFreeSlots(100),
            producer2.try_batch_publish(106, |_iter| {}).err().unwrap()
        );

        barrier.store(false, Relaxed);
        producer1
            .try_batch_publish(2, |iter| {
                for e in iter {
                    e.num = 2;
                }
            })
            .expect("Batch publication should now succeed.");
        producer2
            .try_batch_publish(2, |iter| {
                for e in iter {
                    e.num = 3;
                }
            })
            .expect("Batch publication should now succeed.");

        drop(producer1);
        drop(producer2);
        let mut result: Vec<_> = r.iter().collect();
        result.sort();
        // Initial events published.
        let mut expected = (0..58).into_iter().collect::<Vec<i64>>();
        // Now add the two successfull batch publications.
        expected.push(2);
        expected.push(2);
        expected.push(3);
        expected.push(3);
        expected.sort();
        assert_eq!(result, expected);
    }

    #[test]
    fn spsc_disruptor() {
        let (s, r) = mpsc::channel();
        let processor = move |e: &Event, _, _| {
            s.send(e.num).expect("Should be able to send.");
        };
        let mut producer = build_uni_producer_unchecked(8, factory(), BusySpin)
            .handle_events_with(processor)
            .build();

        thread::scope(|s| {
            s.spawn(move || {
                for i in 0..10 {
                    producer.publish(|e| e.num = i * i);
                }
            });
        });

        let result: Vec<_> = r.iter().collect();
        assert_eq!(result, [0, 1, 4, 9, 16, 25, 36, 49, 64, 81]);
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn spsc_disruptor_with_pinned_and_named_thread() {
        let (s, r) = mpsc::channel();
        let processor = move |e: &Event, _, _| {
            s.send(e.num).expect("Should be able to send.");
        };
        let mut producer = build_uni_producer_unchecked(8, factory(), BusySpin)
            .pin_at_core(0)
            .thread_name("my-processor")
            .handle_events_with(processor)
            .build();

        thread::scope(|s| {
            s.spawn(move || {
                for i in 0..10 {
                    producer.publish(|e| e.num = i * i);
                }
            });
        });

        let result: Vec<_> = r.iter().collect();
        assert_eq!(result, [0, 1, 4, 9, 16, 25, 36, 49, 64, 81]);
    }

    #[test]
    fn spsc_disruptor_with_batch_publication() {
        let (s, r) = mpsc::channel();
        let processor = move |e: &Event, _, _| {
            s.send(e.num).expect("Should be able to send.");
        };
        let mut producer = build_uni_producer_unchecked(8, factory(), BusySpin)
            .handle_events_with(processor)
            .build();

        let mut i = 0;
        for _ in 0..3 {
            producer.batch_publish(3, |iter| {
                // We are guaranteed that the iterator will yield three elements:
                assert_eq!((3, Some(3)), iter.size_hint());

                // Publish.
                for e in iter {
                    e.num = i * i;
                    i += 1;
                }
            });
        }
        drop(producer);

        let result: Vec<_> = r.iter().collect();
        assert_eq!(result, [0, 1, 4, 9, 16, 25, 36, 49, 64]);
    }

    #[test]
    fn spsc_disruptor_with_zero_batch_publication() {
        let processor = |_e: &Event, _, _| {
            panic!("No events should be published.");
        };
        let mut producer = build_uni_producer_unchecked(8, factory(), BusySpin)
            .handle_events_with(processor)
            .build();

        producer.batch_publish(0, |iter| {
            assert_eq!(0, iter.count());
        });
        drop(producer);
    }

    #[test]
    fn spsc_disruptor_with_state() {
        let (s, r) = mpsc::channel();
        let initialize_state = || Rc::new(RefCell::new(0));
        let processor = move |state: &mut Rc<RefCell<i64>>, e: &Event, _, _| {
            let mut ref_cell = state.borrow_mut();
            *ref_cell += e.num;
            s.send(*ref_cell).expect("Should be able to send.");
        };
        let mut producer = build_uni_producer_unchecked(8, factory(), BusySpin)
            .handle_events_and_state_with(processor, initialize_state)
            .build();

        for i in 0..10 {
            producer.publish(|e| e.num = i);
        }

        drop(producer);
        let result: Vec<_> = r.iter().collect();
        assert_eq!(result, [0, 1, 3, 6, 10, 15, 21, 28, 36, 45]);
    }

    #[test]
    fn pipeline_of_two_spsc_disruptors() {
        let (s, r) = mpsc::channel();
        let processor = move |e: &Event, _, _| {
            s.send(e.num).expect("Should be able to send.");
        };

        // Last Disruptor.
        let mut producer = build_uni_producer_unchecked(8, factory(), BusySpin)
            .handle_events_with(processor)
            .build();
        let processor = move |e: &Event, _, _| {
            producer.publish(|e2| e2.num = e.num);
        };

        // First Disruptor.
        let mut producer = build_uni_producer_unchecked(8, factory(), BusySpin)
            .handle_events_with(processor)
            .build();
        for i in 0..10 {
            producer.publish(|e| e.num = i * i);
        }

        drop(producer);
        let result: Vec<_> = r.iter().collect();
        assert_eq!(result, [0, 1, 4, 9, 16, 25, 36, 49, 64, 81]);
    }

    #[test]
    fn multi_publisher_disruptor() {
        let (s, r) = mpsc::channel();
        let processor = move |e: &Event, _, _| {
            s.send(e.num).expect("Should be able to send.");
        };

        let mut producer1 = build_multi_producer_unchecked(64, factory(), BusySpinWithSpinLoopHint)
            .handle_events_with(processor)
            .build();
        let mut producer2 = producer1.clone();

        let num_items = 100;

        thread::scope(|s| {
            s.spawn(move || {
                for i in 0..num_items / 2 {
                    producer1.publish(|e| e.num = i);
                }
            });

            s.spawn(move || {
                for i in (num_items / 2)..num_items {
                    producer2.publish(|e| e.num = i);
                }
            });
        });

        let mut result: Vec<_> = r.iter().collect();
        result.sort();

        let expected: Vec<i64> = (0..num_items).collect();
        assert_eq!(result, expected);
    }

    #[test]
    fn multi_publisher_disruptor_with_batch_publication() {
        let (s, r) = mpsc::channel();
        let processor = move |e: &Event, _, _| {
            s.send(e.num).expect("Should be able to send.");
        };

        let mut producer1 = build_multi_producer_unchecked(64, factory(), BusySpin)
            .handle_events_with(processor)
            .build();
        let mut producer2 = producer1.clone();

        let num_items = 100_i64;
        let batch_size = 5;

        thread::scope(|s| {
            s.spawn(move || {
                for b in 0..(num_items / 2) / batch_size {
                    producer1.batch_publish(batch_size as usize, |iter| {
                        for (i, e) in iter.enumerate() {
                            e.num = batch_size * b + i as i64;
                        }
                    });
                }
            });

            s.spawn(move || {
                for i in (num_items / 2)..num_items {
                    producer2.publish(|e| e.num = i as i64);
                }
            });
        });

        let mut result: Vec<_> = r.iter().collect();
        result.sort();

        let expected: Vec<i64> = (0..num_items).collect();
        assert_eq!(result, expected);
    }

    #[test]
    fn spmc_with_concurrent_consumers() {
        let (s, r) = mpsc::channel();

        let processor1 = {
            let s = s.clone();
            move |e: &Event, _, _| {
                s.send(e.num + 1).unwrap();
            }
        };
        let processor2 = {
            let s = s.clone();
            move |e: &Event, _, _| {
                s.send(e.num + 2).unwrap();
            }
        };
        let processor3 = {
            move |e: &Event, _, _| {
                s.send(e.num + 3).unwrap();
            }
        };

        let builder = build_uni_producer_unchecked(8, factory(), BusySpin);
        let mut producer = builder
            .handle_events_with(processor1)
            .handle_events_with(processor2)
            .handle_events_with(processor3)
            .build();

        producer.publish(|e| {
            e.num = 0;
        });
        drop(producer);

        let result: HashSet<i64> = r.iter().collect();
        let expected = HashSet::from([1, 2, 3]);
        assert_eq!(expected, result);
    }

    #[test]
    fn spmc_with_dependent_consumers_and_some_with_state() {
        let (s, r) = mpsc::channel();

        let processor1 = {
            let s = s.clone();
            move |e: &Event, _, _| {
                s.send(e.num + 0).unwrap();
            }
        };
        let processor2 = {
            let s = s.clone();
            move |state: &mut RefCell<i64>, e: &Event, _, _| {
                *state.borrow_mut() += e.num;
                s.send(*state.borrow() + 10).unwrap();
            }
        };
        let processor3 = {
            let s = s.clone();
            move |e: &Event, _, _| {
                s.send(e.num + 2).unwrap();
            }
        };
        let processor4 = {
            let s = s.clone();
            move |state: &mut RefCell<i64>, e: &Event, _, _| {
                *state.borrow_mut() += e.num;
                s.send(*state.borrow() + 20).unwrap();
            }
        };
        let processor5 = {
            let s = s.clone();
            move |e: &Event, _, _| {
                s.send(e.num + 4).unwrap();
            }
        };
        let processor6 = {
            let s = s.clone();
            move |state: &mut RefCell<i64>, e: &Event, _, _| {
                *state.borrow_mut() += e.num;
                s.send(*state.borrow() + 30).unwrap();
            }
        };
        let processor7 = {
            move |state: &mut RefCell<i64>, e: &Event, _, _| {
                *state.borrow_mut() += e.num;
                s.send(*state.borrow() + 40).unwrap();
            }
        };

        let builder = build_uni_producer_unchecked(8, factory(), BusySpin);

        let mut producer = builder
            .handle_events_with(processor1)
            .handle_events_and_state_with(processor2, || RefCell::new(0))
            .and_then()
            .handle_events_with(processor3)
            .handle_events_and_state_with(processor4, || RefCell::new(0))
            .and_then()
            .handle_events_with(processor5)
            .handle_events_and_state_with(processor6, || RefCell::new(0))
            .handle_events_and_state_with(processor7, || RefCell::new(0))
            .build();

        producer.publish(|e| {
            e.num = 1;
        });
        drop(producer);

        let mut result: Vec<i64> = r.iter().collect();
        result.sort();
        assert_eq!(vec![1, 3, 5, 11, 21, 31, 41], result);
    }

    #[test]
    fn mpmc_with_dependent_consumers_and_some_with_state() {
        let (s, r) = mpsc::channel();

        let processor1 = {
            let s = s.clone();
            move |e: &Event, _, _| {
                s.send(e.num + 0).unwrap();
            }
        };
        let processor2 = {
            let s = s.clone();
            move |state: &mut RefCell<i64>, e: &Event, _, _| {
                *state.borrow_mut() += e.num;
                s.send(*state.borrow() + 10).unwrap();
            }
        };
        let processor3 = {
            let s = s.clone();
            move |state: &mut RefCell<i64>, e: &Event, _, _| {
                *state.borrow_mut() += e.num;
                s.send(*state.borrow() + 20).unwrap();
            }
        };
        let processor4 = {
            let s = s.clone();
            move |e: &Event, _, _| {
                s.send(e.num + 2).unwrap();
            }
        };
        let processor5 = {
            let s = s.clone();
            move |e: &Event, _, _| {
                s.send(e.num + 4).unwrap();
            }
        };
        let processor6 = {
            let s = s.clone();
            move |state: &mut RefCell<i64>, e: &Event, _, _| {
                *state.borrow_mut() += e.num;
                s.send(*state.borrow() + 30).unwrap();
            }
        };
        let processor7 = {
            move |state: &mut RefCell<i64>, e: &Event, _, _| {
                *state.borrow_mut() += e.num;
                s.send(*state.borrow() + 40).unwrap();
            }
        };

        let builder = build_multi_producer_unchecked(64, factory(), BusySpin);
        let mut producer1 = builder
            .handle_events_with(processor1)
            .handle_events_and_state_with(processor2, || RefCell::new(0))
            .and_then()
            .handle_events_and_state_with(processor3, || RefCell::new(0))
            .handle_events_with(processor4)
            .and_then()
            .handle_events_with(processor5)
            .handle_events_and_state_with(processor6, || RefCell::new(0))
            .handle_events_and_state_with(processor7, || RefCell::new(0))
            .build();

        let mut producer2 = producer1.clone();

        thread::scope(|s| {
            s.spawn(move || {
                producer1.publish(|e| e.num = 1);
            });

            s.spawn(move || {
                producer2.publish(|e| e.num = 1);
            });
        });

        let mut result: Vec<i64> = r.iter().collect();
        result.sort();
        assert_eq!(
            vec![1, 1, 3, 3, 5, 5, 11, 12, 21, 22, 31, 32, 41, 42],
            result
        );
    }

    #[test]
    fn spmc_with_event_pollers() {
        let builder = build_uni_producer_unchecked(8, factory(), BusySpin);
        let (mut ep1, b) = builder.event_poller();
        let (mut ep2, b) = b.and_then().event_poller();
        let (mut ep3, b) = b.and_then().event_poller();
        let mut producer = b.build();

        // Polling before publication should yield no events.
        assert_eq!(ep1.poll().err(), Some(EPolling::NoEvents));
        assert_eq!(ep2.poll().err(), Some(EPolling::NoEvents));
        assert_eq!(ep3.poll().err(), Some(EPolling::NoEvents));

        // Publish two events.
        producer.publish(|e| {
            e.num = 1;
        });
        producer.publish(|e| {
            e.num = 2;
        });

        let expected = vec![1, 2];

        {
            // Only first poller sees events.
            let mut guard_1 = ep1.poll().unwrap();
            assert_eq!(2, (&mut guard_1).len());
            assert_eq!(ep2.poll().err(), Some(EPolling::NoEvents));
            assert_eq!(ep2.poll_take(2).err(), Some(EPolling::NoEvents));
            assert_eq!(ep3.poll().err(), Some(EPolling::NoEvents));
            assert_eq!(ep3.poll_take(2).err(), Some(EPolling::NoEvents));
            assert_eq!(expected, guard_1.map(|e| e.num).collect::<Vec<_>>());
        }

        {
            // Now second poller sees events - here one at a time.
            let mut guard_2 = ep2.poll_take(1).unwrap();
            assert_eq!(ep3.poll().err(), Some(EPolling::NoEvents));
            assert_eq!(vec![1], guard_2.map(|e| e.num).collect::<Vec<_>>());
            drop(guard_2);
            // Read next event.
            let mut guard_2 = ep2.poll_take(1).unwrap();
            assert_eq!(vec![2], guard_2.map(|e| e.num).collect::<Vec<_>>());
        }

        {
            // Now third poller sees both events.
            let mut guard_3 = ep3.poll().unwrap();
            assert_eq!(expected, guard_3.map(|e| e.num).collect::<Vec<_>>());
        }

        // Dropping the producer should indicate shutdown to all pollers.
        drop(producer);
        assert_eq!(ep1.poll().err(), Some(EPolling::Shutdown));
        assert_eq!(ep2.poll().err(), Some(EPolling::Shutdown));
        assert_eq!(ep3.poll().err(), Some(EPolling::Shutdown));
    }

    #[test]
    fn mpmc_with_event_pollers() {
        let builder = build_multi_producer_unchecked(64, factory(), BusySpin);
        let (mut ep1, b) = builder.event_poller();
        let (mut ep2, b) = b.and_then().event_poller();
        let (mut ep3, b) = b.and_then().event_poller();
        let mut producer1 = b.build();
        let mut producer2 = producer1.clone();

        // Polling before publication should yield no events.
        assert_eq!(ep1.poll().err(), Some(EPolling::NoEvents));
        assert_eq!(ep2.poll().err(), Some(EPolling::NoEvents));
        assert_eq!(ep3.poll().err(), Some(EPolling::NoEvents));

        // Publish two events.
        producer1.publish(|e| {
            e.num = 1;
        });
        producer2.publish(|e| {
            e.num = 2;
        });

        let expected = HashSet::from([1, 2]);

        {
            // Only first poller sees events.
            let mut guard_1 = ep1.poll().unwrap();
            assert_eq!(ep2.poll().err(), Some(EPolling::NoEvents));
            assert_eq!(ep3.poll().err(), Some(EPolling::NoEvents));
            assert_eq!(expected, guard_1.map(|e| e.num).collect::<HashSet<_>>());
        }

        {
            // Now second poller sees events - here one at a time.
            let mut guard_2 = ep2.poll_take(1).unwrap();
            assert_eq!(ep3.poll().err(), Some(EPolling::NoEvents));
            assert_eq!(vec![1], guard_2.map(|e| e.num).collect::<Vec<_>>());
            drop(guard_2);
            // Read next event.
            let mut guard_2 = ep2.poll_take(1).unwrap();
            assert_eq!(vec![2], guard_2.map(|e| e.num).collect::<Vec<_>>());
        }

        {
            // Now third poller sees both events.
            let mut guard_3 = ep3.poll().unwrap();
            assert_eq!(expected, guard_3.map(|e| e.num).collect::<HashSet<_>>());
        }

        // Dropping the producers should indicate shutdown to all pollers.
        drop(producer1);
        drop(producer2);
        assert_eq!(ep1.poll().err(), Some(EPolling::Shutdown));
        assert_eq!(ep2.poll().err(), Some(EPolling::Shutdown));
        assert_eq!(ep3.poll().err(), Some(EPolling::Shutdown));
    }

    #[test]
    fn spmc_with_mixed_event_pollers_and_processors() {
        let (s, r) = mpsc::channel();
        let processor1 = move |e: &Event, _, _| {
            s.send(e.num).unwrap();
        };

        let builder =
            build_uni_producer_unchecked(8, factory(), BusySpin).handle_events_with(processor1);
        let (mut ep1, b) = builder.event_poller();
        let (mut ep2, b) = b.and_then().event_poller();
        let mut producer = b.build();

        // Polling before publication should yield no events.
        assert_eq!(ep1.poll().err(), Some(EPolling::NoEvents));
        assert_eq!(ep2.poll().err(), Some(EPolling::NoEvents));

        // Publish two events.
        producer.publish(|e| {
            e.num = 1;
        });
        producer.publish(|e| {
            e.num = 2;
        });
        drop(producer);

        let expected = vec![1, 2];

        {
            // Only first poller sees events.
            let mut guard_1 = ep1.poll().unwrap();
            assert_eq!(ep2.poll().err(), Some(EPolling::NoEvents));
            assert_eq!(expected, guard_1.map(|e| e.num).collect::<Vec<_>>());
        }
        // Next poll should indicate shutdown to the poller.
        assert_eq!(ep1.poll().err(), Some(EPolling::Shutdown));

        {
            // Now second poller sees events.
            let mut guard_2 = ep2.poll().unwrap();
            assert_eq!(expected, guard_2.map(|e| e.num).collect::<Vec<_>>());
        }

        let mut result: Vec<i64> = r.iter().collect();
        result.sort();
        assert_eq!(expected, result);
    }

    #[test]
    fn mpmc_with_mixed_event_pollers_and_processors() {
        let (s, r) = mpsc::channel();
        let processor1 = move |e: &Event, _, _| {
            s.send(e.num).unwrap();
        };

        let builder =
            build_multi_producer_unchecked(64, factory(), BusySpin).handle_events_with(processor1);
        let (mut ep1, b) = builder.event_poller();
        let (mut ep2, b) = b.and_then().event_poller();
        let mut producer1 = b.build();
        let mut producer2 = producer1.clone();

        // Polling before publication should yield no events.
        assert_eq!(ep1.poll().err(), Some(EPolling::NoEvents));
        assert_eq!(ep2.poll().err(), Some(EPolling::NoEvents));

        // Publish two events.
        producer1.publish(|e| {
            e.num = 1;
        });
        producer2.publish(|e| {
            e.num = 2;
        });
        drop(producer1);
        drop(producer2);

        let expected = vec![1, 2];

        {
            // Only first poller sees events.
            let mut guard_1 = ep1.poll().unwrap();
            assert_eq!(ep2.poll().err(), Some(EPolling::NoEvents));
            assert_eq!(expected, guard_1.map(|e| e.num).collect::<Vec<_>>());
        }
        // Next poll should indicate shutdown to the poller.
        assert_eq!(ep1.poll().err(), Some(EPolling::Shutdown));

        {
            // Now second poller sees events.
            let mut guard_2 = ep2.poll().unwrap();
            assert_eq!(expected, guard_2.map(|e| e.num).collect::<Vec<_>>());
        }

        let mut result: Vec<i64> = r.iter().collect();
        result.sort();
        assert_eq!(expected, result);
    }

    #[cfg_attr(miri, ignore)] // Miri disabled due to excessive runtime.
    #[test]
    fn stress_test() {
        #[derive(Debug)]
        struct StressEvent {
            i: Sequence,
            a: i64,
            b: i64,
            s: String,
        }

        let (s_seq, r_seq) = mpsc::channel();
        let num_events = 250_000;
        let producers = 4;
        let consumers = 3;

        let mut processors: Vec<_> = (0..consumers)
            .into_iter()
            .map(|pid| {
                let s_seq = s_seq.clone();
                let mut prev_seq = -1;
                Some(move |e: &StressEvent, sequence, _| {
                    assert_eq!(e.a, e.i - 5);
                    assert_eq!(e.b, e.i + 7);
                    assert_eq!(e.s, format!("Blackbriar {}", e.i).to_string());
                    assert_eq!(sequence, prev_seq + 1);

                    prev_seq = sequence;
                    let sequence_seen_by_pid = sequence * consumers + pid;
                    s_seq.send(sequence_seen_by_pid).expect("Should send.");
                })
            })
            .collect();

        // Drop unused Sender.
        drop(s_seq);

        let factory = || StressEvent {
            i: -1,
            a: -1,
            b: -1,
            s: "".to_string(),
        };

        let producer = build_multi_producer_unchecked(1 << 16, factory, BusySpin)
            .handle_events_with(processors[0].take().unwrap())
            .handle_events_with(processors[1].take().unwrap())
            .handle_events_with(processors[2].take().unwrap())
            .build();

        thread::scope(|s| {
            for _ in 0..producers {
                let mut producer = producer.clone();
                s.spawn(move || {
                    for i in 0..num_events {
                        producer.publish(|e| {
                            e.i = i;
                            e.a = i - 5;
                            e.b = i + 7;
                            e.s = format!("Blackbriar {}", i).to_string();
                        });
                    }
                });
            }
            drop(producer); // Drop excess producer not used.
        });

        let expected_sequence_reads = consumers * num_events * producers;
        let mut seen_sequences: Vec<_> = r_seq.iter().collect();

        assert_eq!(expected_sequence_reads as usize, seen_sequences.len());
        // Assert that each consumer saw each sequence number.
        seen_sequences.sort();
        for seq_seen_by_pid in 0..expected_sequence_reads {
            assert_eq!(seq_seen_by_pid, seen_sequences[seq_seen_by_pid as usize]);
        }
    }
}
