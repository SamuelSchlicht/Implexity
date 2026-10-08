// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, ThreadId};

#[derive(Debug, Default)]
pub struct ReLock {
    state: Mutex<(Option<ThreadId>, usize)>,
    released: Condvar,
}

#[derive(Debug)]
pub struct ReLockGuard<'a> {
    lock: &'a ReLock,
}

impl ReLock {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lock(&self) -> ReLockGuard<'_> {
        let me = thread::current().id();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            match state.0 {
                None => {
                    *state = (Some(me), 1);
                    break;
                }
                Some(owner) if owner == me => {
                    state.1 += 1;
                    break;
                }
                Some(_) => {
                    state = self.released.wait(state).unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
        ReLockGuard { lock: self }
    }

    pub fn try_lock(&self) -> Option<ReLockGuard<'_>> {
        let me = thread::current().id();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match state.0 {
            None => {
                *state = (Some(me), 1);
                Some(ReLockGuard { lock: self })
            }
            Some(owner) if owner == me => {
                state.1 += 1;
                Some(ReLockGuard { lock: self })
            }
            Some(_) => None,
        }
    }

    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0.is_some()
    }
}

impl Drop for ReLockGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.lock.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.1 = state.1.saturating_sub(1);
        if state.1 == 0 {
            state.0 = None;
            self.lock.released.notify_one();
        }
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}


