use std::{cell::UnsafeCell, sync::{Arc, atomic::{self, AtomicPtr}}};

use linux_futex::PiFutex;

/// Denotes task priority. Higher number means higher priority. Must not be zero.
pub type Priority = u8;

pub type ThreadId = i32;

const FUTEX_TID_MASK: i32 = PiFutex::<linux_futex::Private>::TID_MASK;
const FUTEX_WAITERS: i32 = PiFutex::<linux_futex::Private>::WAITERS;

#[derive(Debug, Clone, Copy)]
pub struct LockerInfo {
    thread: ThreadId,
    ceiling: Priority,
}

#[derive(Debug)]
pub struct PcpState {
    null_locker: Box<LockerInfo>,
    current: AtomicPtr<LockerInfo>,
    /// Linux priority inheritance futex for thread blocking
    futex: PiFutex<linux_futex::Private>,
}

impl Default for PcpState {
    fn default() -> Self {
        let null_locker = Box::new(LockerInfo {
            thread: 0,
            ceiling: 0,
        });
        let null_locker_ptr = unsafe { core::mem::transmute(&*null_locker) };

        Self { null_locker, current: AtomicPtr::new(null_locker_ptr), futex: Default::default() }
    }
}

/// Root entity for creating PCP mutexes belonging to the same group.
#[derive(Debug, Clone, Default)]
pub struct PcpManager {
    state: Arc<PcpState>,
}

impl PcpManager {
    /// Creates a new mutex with a given priority ceiling (calculated statically).
    pub fn create<T>(&self, res: T, ceiling: Priority) -> PcpMutex<T> {
        PcpMutex {
            res: UnsafeCell::new(res),
            ceiling,
            mgr: self.clone(),
        }
    }
}

/// A Mutex belonging to some PcpManager group
#[derive(Debug)]
pub struct PcpMutex<T> {
    /// Resource protected by the mutex
    res: UnsafeCell<T>,
    mgr: PcpManager,
    ceiling: Priority,
}

// PcpMutex is not Send nor Sync because we use UnsafeCell.
// It is save to `Send` it between threads as long as resource itself is `Send`.
unsafe impl<T> Send for PcpMutex<T> where T: Send {}
// We ensure `Sync` behavior by only having a single PcpMutexGuard.
unsafe impl<T> Sync for PcpMutex<T> where T: Send {}

impl<T> PcpMutex<T> {
    /// Locks the mutex with the provided current thread priority.
    /// Priority must be greater or equal to 1.
    pub fn lock<R>(&self, priority: Priority, f: impl FnOnce(&mut T) -> R) -> R {
        use atomic::Ordering::*;
        let state = &self.mgr.state;

        let current = LockerInfo {
            thread: current_thread_id(),
            ceiling: self.ceiling,
        };

        loop {
            let prev_ptr = state.current.load(SeqCst);
            let previous = unsafe { *prev_ptr };
            //println!("Prev: {:?}, Next: {:?}", previous, current);

            if priority > previous.ceiling || (priority == previous.ceiling && current.thread == previous.thread) {
                match state.current.compare_exchange(prev_ptr, unsafe { core::mem::transmute(&current) }, SeqCst, SeqCst) {
                    Ok(_) => {},
                    // State was changed while running, retry
                    Err(_) => continue,
                }

                loop {
                    let prev_futex = state.futex.value.load(SeqCst);

                    // We may already hold the futex because UNLOCK_PI queued this thread for execution
                    if prev_futex & FUTEX_TID_MASK == current.thread {
                        break;
                    // Try to take the lock from previous owner
                    } else if prev_futex & FUTEX_TID_MASK == previous.thread {
                        match state.futex.value.compare_exchange(prev_futex, current.thread + (prev_futex & FUTEX_WAITERS), SeqCst, SeqCst) {
                            Ok(_) => break,
                            Err(_) => continue,
                        }
                    // Owner has changed while running. This can happen if a higher priority task locked a resource with higher ceiling.
                    // Wait until lock is returned.
                    } else {
                        // try a few more loops and then do futex lock_pi
                    }
                }

                let res = f(unsafe { &mut *self.res.get() });

                loop {
                    match state.current.compare_exchange(unsafe { core::mem::transmute(&current) }, prev_ptr, SeqCst, SeqCst) {
                        Ok(_) => break,
                        // State was changed while running, retry
                        Err(_) => continue,
                    }
                }

                loop {
                    let cur_futex = state.futex.value.load(SeqCst);
                    // Check if we are still the owner of the futex. A higher priority task could have been running with higher ceiling.
                    if cur_futex & FUTEX_TID_MASK == current.thread {
                        // Wakeup waiter if there are any
                        if cur_futex & FUTEX_WAITERS != 0 {
                            state.futex.unlock_pi();
                            break;
                        } else {
                            match state.futex.value.compare_exchange(cur_futex, previous.thread, SeqCst, SeqCst) {
                                Ok(_) => break,
                                // A waiter appeared while running, retry
                                Err(_) => continue,
                            }
                        }
                    // Futex is currently held by higher priority task, wait until it is released.
                    } else {
                        // try a few more loops and then do futex lock_pi
                    }
                }

                return res;
            } else {
                loop {
                    let cur_futex = state.futex.value.load(SeqCst);
                    if cur_futex & FUTEX_TID_MASK == current.thread {
                        state.futex.value.compare_exchange(cur_futex, previous.thread + (cur_futex & FUTEX_WAITERS), SeqCst, SeqCst).ok();
                    }

                    if state.futex.lock_pi().is_ok() {
                        break;
                    }
                }
            }
        }
    }

    /// Returns priority ceiling of this mutex
    pub fn ceiling(&self) -> Priority {
        self.ceiling
    }
}

/// Returns current thread id
fn current_thread_id() -> ThreadId {
    unsafe { libc::syscall(libc::SYS_gettid) as _ }
}
