use std::thread::JoinHandle;

pub struct Consumer {
    /// Stores the handle to a thread. It doesn't send messages,
    /// doesn't synchronize, doesn't do anything while the thread is running.
    /// It sits there inert.                                                                                                                                       
    /// It only does work at shutdown, when you call .join()
    join_handle: Option<JoinHandle<()>>,
}

impl Consumer {
    pub(crate) fn new(join_handle: JoinHandle<()>) -> Self {
        Self {
            join_handle: Some(join_handle),
        }
    }

    /// Blocks the calling thread until the target thread exits.
    /// That's a single syscall, once, at the end of the disruptor's lifetime.
    pub(crate) fn join(&mut self) {
        if let Some(handle) = self.join_handle.take() {
            handle.join().expect("Consumer should not panic.")
        }
    }
}
