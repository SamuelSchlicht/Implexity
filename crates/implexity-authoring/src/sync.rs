// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Default)]
pub struct ReentrantLock {
    state: Mutex<(Option<ThreadId>, usize)>,
    released: Condvar,
}

#[derive(Debug)]
pub struct ReentrantGuard<'a> {
    lock: &'a ReentrantLock,
}

impl ReentrantLock {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn acquire(&self) -> ReentrantGuard<'_> {
        let me = std::thread::current().id();
        let mut st = lock(&self.state);
        loop {
            match st.0 {
                None => {
                    *st = (Some(me), 1);
                    break;
                }
                Some(owner) if owner == me => {
                    st.1 += 1;
                    break;
                }
                Some(_) => {
                    st = self.released.wait(st).unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
        ReentrantGuard { lock: self }
    }

    #[must_use]
    pub fn held_by_current(&self) -> bool {
        lock(&self.state).0 == Some(std::thread::current().id())
    }
}

impl Drop for ReentrantGuard<'_> {
    fn drop(&mut self) {
        let mut st = lock(&self.lock.state);
        if st.1 > 0 {
            st.1 -= 1;
        }
        if st.1 == 0 {
            st.0 = None;
            self.lock.released.notify_one();
        }
    }
}
