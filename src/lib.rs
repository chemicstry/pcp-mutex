use std::{cell::UnsafeCell, sync::{Arc, Mutex, atomic}};

use linux_futex::PiFutex;

/// Denotes task priority. Higher number means higher priority. Must not be zero.
pub type Priority = u8;

pub type ThreadId = i32;

#[derive(Debug, Clone, Copy)]
pub struct CeilingInfo {
    /// Thread that raised the ceiling
    thread: ThreadId,
    /// The value ceiling was raised to,
    ceiling: Priority,
}

#[derive(Debug, Default)]
pub struct PcpState {
    ceiling_stack: Mutex<Vec<CeilingInfo>>,
    /// Linux priority inheritance futex for thread blocking
    futex: PiFutex<linux_futex::Private>,
}

/// Root entity for creating PCP mutexes belonging to the same group.
#[derive(Debug, Default, Clone)]
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
    pub fn lock<R>(&self, priority: Priority, f: impl Fn(&mut T) -> R) -> R {
        loop {
            // Limit scope of state lock
            {
                let state = &self.mgr.state;
                let mut ceiling_stack = state.ceiling_stack.lock().unwrap();

                let previous = ceiling_stack.last().copied().unwrap_or(CeilingInfo {
                    thread: 0,
                    ceiling: 0,
                });

                let current = CeilingInfo {
                    thread: current_thread_id(),
                    ceiling: self.ceiling,
                };

                // Check if thread priority is higher than ceiling, if so, we can take the lock
                if priority > previous.ceiling || (priority == previous.ceiling && current.thread == previous.thread) {
                    // Try to set futex to this thread
                    if state.futex.value.compare_exchange(previous.thread, current.thread, atomic::Ordering::SeqCst, atomic::Ordering::SeqCst).is_ok() {
                        // Update is successful, push new state to ceiling stack
                        ceiling_stack.push(current);
                    } else {
                        // Futex was updated while running, try again
                        continue;
                    }

                    drop(ceiling_stack);

                    let res = f(unsafe { &mut *self.res.get() });

                    let mut ceiling_stack = state.ceiling_stack.lock().unwrap();
                    ceiling_stack.pop().unwrap();

                    if previous.thread != 0 || state.futex.value.compare_exchange(current.thread, 0, atomic::Ordering::SeqCst, atomic::Ordering::SeqCst).is_err() {
                        state.futex.unlock_pi();
                    }
                    
                    drop(ceiling_stack);

                    return res;
                }
            }

            // Thread priority is not high enough and it has to wait.
            self.mgr.state.futex.lock_pi().unwrap();
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
