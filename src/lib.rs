use std::{cell::UnsafeCell, collections::BinaryHeap, ops::{Deref, DerefMut}, sync::{Arc, Mutex}, thread::{Thread, ThreadId}};

/// Denotes task priority. Higher number means higher priority. Must not be zero.
pub type Priority = u8;

#[derive(Debug)]
struct MutexState {
    // Equal to ceiling if mutex is locked, zero otherwise. Prevents branching.
    current_ceiling: Priority,
    // Thread that is currently holding the mutex. Needed for ceiling calculation.
    holding_thread: Option<ThreadId>,
    // Statically calculated ceiling of all tasks that use this mutex.
    ceiling: Priority,
}

/// Parked thread that was blocked by PCP and is waiting for wakeup.
#[derive(Debug)]
pub struct ParkedThread {
    thread: Thread,
    priority: Priority,
}

impl PartialEq for ParkedThread {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}

impl Eq for ParkedThread {}

impl PartialOrd for ParkedThread {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.priority.partial_cmp(&other.priority)
    }
}

impl Ord for ParkedThread {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.priority.cmp(&other.priority)
    }
}

#[derive(Debug, Default)]
pub struct PcpState {
    /// List of parked threads that were blocked by PCP and are waiting for wakeup.
    parked: BinaryHeap<ParkedThread>,
    /// List of mutexes in the priority ceiling protocol.
    mutexes: Vec<MutexState>,
}

impl PcpState {
    fn current_ceiling(&self, thread: ThreadId) -> Priority {
        self.mutexes.iter().filter(|m| m.holding_thread != Some(thread)).map(|m| m.current_ceiling).max().unwrap_or(0)
    }
}

/// Root entity for creating PCP mutexes belonging to the same group.
#[derive(Debug, Default, Clone)]
pub struct PcpManager {
    state: Arc<Mutex<PcpState>>,
}

impl PcpManager {
    /// Creates a new mutex with a given priority ceiling (calculated statically).
    pub fn create<T>(&self, res: T, ceiling: Priority) -> PcpMutex<T> {
        let index = {
            let mut state = self.state.lock().unwrap();
            state.mutexes.push(MutexState {
                current_ceiling: 0,
                holding_thread: None,
                ceiling,
            });
            state.mutexes.len()-1
        };

        PcpMutex {
            res: UnsafeCell::new(res),
            mgr: self.clone(),
            index,
        }
    }
}

/// A Mutex belonging to some PcpManager group
#[derive(Debug)]
pub struct PcpMutex<T> {
    /// Resource protected by the mutex
    res: UnsafeCell<T>,
    mgr: PcpManager,
    index: usize,
}

// PcpMutex is not Send nor Sync because we use UnsafeCell.
// It is save to `Send` it between threads as long as resource itself is `Send`.
unsafe impl<T> Send for PcpMutex<T> where T: Send {}
// We ensure `Sync` behavior by only having a single PcpMutexGuard.
unsafe impl<T> Sync for PcpMutex<T> where T: Send {}

/// A handle to a locked resource.
pub struct PcpMutexGuard<'a, T> {
    pcp_mutex: &'a PcpMutex<T>,
}

impl<'a, T> Deref for PcpMutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // Safe because we ensure that only one PcpMutexGuard is ever alive.
        unsafe { &*self.pcp_mutex.res.get() }
    }
}

impl<'a, T> DerefMut for PcpMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        // Safe because we ensure that only one PcpMutexGuard is ever alive.
        unsafe { &mut *self.pcp_mutex.res.get() }
    }
}

impl<T> PcpMutex<T> {
    /// Locks the mutex with the provided current thread priority.
    /// Priority must be greater or equal to 1.
    pub fn lock<'a>(&'a self, priority: Priority) -> PcpMutexGuard<'a, T> {
        loop {
            // Limit scope of state lock
            {
                let mut state = self.mgr.state.lock().unwrap();

                // Calculate ceiling for current thread (excludes locks held by it)
                let id = std::thread::current().id();
                let ceiling = state.current_ceiling(id);

                // Check if thread priority is higher than ceiling, if so, we can take the lock
                if priority > ceiling {
                    state.mutexes[self.index].current_ceiling = state.mutexes[self.index].ceiling;
                    state.mutexes[self.index].holding_thread = Some(id);
                    return PcpMutexGuard {
                        pcp_mutex: self,
                    }
                } else {
                    // Thread priority is not high enough and it has to wait.
                    // We have to do this lookup because thread::park() can wakeup spuriously and cause multiple entries.
                    if state.parked.iter().find(|p| p.thread.id() == id).is_none() {
                        state.parked.push(ParkedThread {
                            thread: std::thread::current(),
                            priority
                        });
                    }
                }
            }

            // Put current thread to sleep
            std::thread::park();
        }
    }
}

impl<'a, T> Drop for PcpMutexGuard<'a, T> {
    fn drop(&mut self) {
        let mut state = self.pcp_mutex.mgr.state.lock().unwrap();

        // Fetch highest priority thread from binary heap
        if let Some(p) = state.parked.pop() {
            p.thread.unpark();
        }

        // Reset mutex state, releasing the lock
        state.mutexes[self.pcp_mutex.index].current_ceiling = 0;
        state.mutexes[self.pcp_mutex.index].holding_thread = None;
    }
}
