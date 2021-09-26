use std::{cell::{Cell, UnsafeCell}, marker::PhantomData, mem::MaybeUninit, sync::{
        atomic::{self, AtomicPtr}, 
        Arc,
    }};

use linux_futex::PiFutex;


pub type Priority = u8;
pub type ThreadId = i32;

const FUTEX_TID_MASK: i32 = PiFutex::<linux_futex::Private>::TID_MASK;
const FUTEX_WAITERS: i32 = PiFutex::<linux_futex::Private>::WAITERS;

#[derive(Debug)]
pub struct ThreadPriority {
    priority: Cell<Priority>,
    thread: ThreadId,
    _non_send: PhantomData<* const u8>,
}

impl ThreadPriority {
    /// Creates a new thread priority object with a given priority.
    ///
    /// # Safety
    ///
    /// The given priority and thread id must match that of the current thread.
    pub unsafe fn new(priority: Priority, thread: ThreadId) -> Self {
        assert!(priority > 0, "Priority must be greater or equal to 1");

        Self {
            priority: Cell::new(priority),
            thread,
            _non_send: PhantomData::default(),
        }
    }

    /// Constructs thread priority object from `sched_getparam` syscall
    pub fn from_sys() -> Self {
        let mut sched_param = MaybeUninit::<libc::sched_param>::uninit();

        unsafe {
            let thread = libc::syscall(libc::SYS_gettid) as _;
            libc::sched_getparam(0, sched_param.as_mut_ptr());
            Self::new(sched_param.assume_init().sched_priority as u8, thread)
        }
    }

    /// Sets current thread scheduling policy to SCHED_FIFO and sets a given priority
    pub fn init_fifo(priority: Priority) -> std::io::Result<Self> {
        let param = libc::sched_param {
            sched_priority: priority as i32,
        };

        unsafe {
            let thread = libc::syscall(libc::SYS_gettid) as _;

            if libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) != 0 {
                return Err(std::io::Error::last_os_error())
            } 
            
            Ok(Self::new(priority, thread))
        }
    }

    fn set(&self, priority: Priority) {
        self.priority.set(priority);
    }

    pub fn get(&self) -> Priority {
        self.priority.get()
    }
}

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

        Self {
            null_locker,
            current: AtomicPtr::new(null_locker_ptr),
            futex: Default::default(),
        }
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
    pub fn lock<R>(&self, priority: &ThreadPriority, f: impl FnOnce(&mut T) -> R) -> R {
        use atomic::Ordering::*;
        let state = &self.mgr.state;

        let current = LockerInfo {
            thread: priority.thread,
            ceiling: self.ceiling,
        };

        loop {
            let prev_ptr = state.current.load(SeqCst);
            let previous = unsafe { *prev_ptr };
            // println!("Prev: {:?}, Next: {:?}", previous, current);

            if priority.get() > previous.ceiling {
                match state.current.compare_exchange(
                    prev_ptr,
                    unsafe { core::mem::transmute(&current) },
                    SeqCst,
                    SeqCst,
                ) {
                    Ok(_) => {}
                    // State was changed while running, retry
                    Err(_) => continue,
                }

                loop {
                    let prev_futex = state.futex.value.load(SeqCst);

                    // If thread came out of LOCK_PI, it will already have its TID in futex
                    if prev_futex & FUTEX_TID_MASK == current.thread {
                        break;
                    // Try to take the lock from previous owner
                    } else if prev_futex & FUTEX_TID_MASK == previous.thread {
                        match state.futex.value.compare_exchange(
                            prev_futex,
                            current.thread + (prev_futex & FUTEX_WAITERS),
                            SeqCst,
                            SeqCst,
                        ) {
                            Ok(_) => {
                                // println!("Futex {}", current.thread + (prev_futex & FUTEX_WAITERS));
                                break;
                            }
                            Err(_) => continue,
                        }
                    // Owner has changed while running. This can happen if a higher priority task locked a resource with higher ceiling.
                    // Wait until lock is returned.
                    } else {
                        // try a few more loops and then do futex lock_pi
                    }
                }

                let prev_priority = priority.get();
                priority.set(self.ceiling);
                let res = f(unsafe { &mut *self.res.get() });
                priority.set(prev_priority);

                loop {
                    match state.current.compare_exchange(
                        unsafe { core::mem::transmute(&current) },
                        prev_ptr,
                        SeqCst,
                        SeqCst,
                    ) {
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
                            // println!("{} unlocking", current.thread);
                            state.futex.unlock_pi();
                            break;
                        } else {
                            match state.futex.value.compare_exchange(
                                cur_futex,
                                previous.thread,
                                SeqCst,
                                SeqCst,
                            ) {
                                Ok(_) => {
                                    // println!("Futex {}", previous.thread);
                                    break;
                                }
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
            } else if priority.get() == previous.ceiling && current.thread == previous.thread {
                return f(unsafe { &mut *self.res.get() });
            } else {
                while !state.futex.lock_pi().is_ok() {}
                // println!("{} resumed. Futex {}", current.thread, state.futex.value.load(SeqCst));
            }
        }
    }

    /// Returns priority ceiling of this mutex
    pub fn ceiling(&self) -> Priority {
        self.ceiling
    }
}
