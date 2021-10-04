use std::{cell::{Cell, UnsafeCell}, marker::PhantomData, mem::MaybeUninit, sync::{
        atomic::{self, AtomicPtr}, 
    }};

use linux_futex::{PiFutex, Private};


pub type Priority = u8;
pub type ThreadId = i32;

const FUTEX_TID_MASK: i32 = PiFutex::<linux_futex::Private>::TID_MASK;
// const FUTEX_WAITERS: i32 = PiFutex::<linux_futex::Private>::WAITERS;

#[derive(Debug)]
pub struct ThreadState {
    priority: Cell<Priority>,
    thread_id: ThreadId,
    _non_send: PhantomData<* const u8>,
}

impl ThreadState {
    /// Creates a new thread priority object with a given priority.
    ///
    /// # Safety
    ///
    /// The given priority and thread id must match that of the current thread.
    pub unsafe fn new(priority: Priority, thread_id: ThreadId) -> Self {
        assert!(priority > 0, "Priority must be greater or equal to 1");

        Self {
            priority: Cell::new(priority),
            thread_id,
            _non_send: PhantomData::default(),
        }
    }

    /// Constructs thread priority object from `sched_getparam` syscall
    pub fn from_sys() -> Self {
        let mut sched_param = MaybeUninit::<libc::sched_param>::uninit();

        unsafe {
            let thread_id = libc::syscall(libc::SYS_gettid) as _;
            libc::sched_getparam(0, sched_param.as_mut_ptr());
            Self::new(sched_param.assume_init().sched_priority as u8, thread_id)
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

    fn set_priority(&self, priority: Priority) {
        self.priority.set(priority);
    }

    pub fn get_priority(&self) -> Priority {
        self.priority.get()
    }
}

// #[derive(Debug, Clone, Copy)]
// pub struct LockerInfo<'a> {
//     thread: ThreadId,
//     ceiling: Priority,
//     futex: &'a PiFutex<Private>,
// }

#[derive(Debug, Default)]
pub struct PcpState{
    highest_locker: AtomicPtr<PcpMutexLock>,
}

/// Root entity for creating PCP mutexes belonging to the same group.
#[derive(Debug, Default)]
pub struct PcpManager {
    state: PcpState,
}

impl PcpManager {
    /// Creates a new mutex with a given priority ceiling (calculated statically).
    pub fn create<'a, T>(&'a self, res: T, ceiling: Priority) -> PcpMutex<'a, T> {
        PcpMutex {
            res: UnsafeCell::new(res),
            mgr: &self,
            lock: PcpMutexLock {
                ceiling,
                futex: PiFutex::default()
            }
            
        }
    }
}

#[derive(Debug)]
pub struct PcpMutexLock {
    ceiling: Priority,
    futex: PiFutex<Private>,
}

/// A Mutex belonging to some PcpManager group
#[derive(Debug)]
pub struct PcpMutex<'a, T> {
    /// Resource protected by the mutex
    res: UnsafeCell<T>,
    mgr: &'a PcpManager,
    lock: PcpMutexLock,
}

// PcpMutex is not Send nor Sync because we use UnsafeCell.
// It is save to `Send` it between threads as long as resource itself is `Send`.
unsafe impl<'a, T> Send for PcpMutex<'a, T> where T: Send {}
// We ensure `Sync` behavior by only having a single PcpMutexGuard.
unsafe impl<'a, T> Sync for PcpMutex<'a, T> where T: Send {}

fn can_lock(highest_ptr: *mut PcpMutexLock, thread_info: &ThreadState) -> bool {
    use atomic::Ordering::*;

    if highest_ptr.is_null() {
        true
    } else {
        let highest = unsafe { highest_ptr.as_ref().unwrap() };

        if thread_info.get_priority() > highest.ceiling {
            true
        } else {
            let locker_tid = highest.futex.value.load(SeqCst) & FUTEX_TID_MASK;

            if thread_info.get_priority() == highest.ceiling && thread_info.thread_id == locker_tid {
                true
            } else {
                false
            }
        }
    }
}

impl<'a, T> PcpMutex<'a, T> {
    /// Locks the mutex with the provided current thread priority.
    /// Priority must be greater or equal to 1.
    pub fn lock<R>(&self, thread_info: &ThreadState, f: impl FnOnce(&mut T) -> R) -> R {
        use atomic::Ordering::*;
        let state = &self.mgr.state;

        loop {
            let highest_ptr = state.highest_locker.load(SeqCst);

            if can_lock(highest_ptr, thread_info) {
                // println!("Thread {} acquiring mutex", thread_info.thread_id);

                if let Err(val) = self.lock.futex.value.compare_exchange(
                    0,
                    thread_info.thread_id,
                    SeqCst,
                    SeqCst,
                ) {
                    if val & FUTEX_TID_MASK != thread_info.thread_id {
                        continue;
                    }
                }

                if state.highest_locker.compare_exchange(
                    highest_ptr,
                    unsafe { core::mem::transmute(&self.lock) },
                    SeqCst,
                    SeqCst,
                ).is_err() {
                    // Race condition.
                    // Unlock the acquired futex and retry
                    if self.lock.futex.value.compare_exchange(
                        thread_info.thread_id,
                        0,
                        SeqCst,
                        SeqCst,
                    ).is_err() {
                        self.lock.futex.unlock_pi();
                    }

                    // retry
                    continue;
                }

                // println!("Thread {} acquired mutex", thread_info.thread_id);

                let prev_priority = thread_info.get_priority();
                thread_info.set_priority(self.lock.ceiling);
                let res = f(unsafe { &mut *self.res.get() });
                thread_info.set_priority(prev_priority);

                // println!("Thread {} releasing mutex", thread_info.thread_id);

                loop {
                    match state.highest_locker.compare_exchange(
                        unsafe { core::mem::transmute(&self.lock) },
                        highest_ptr,
                        SeqCst,
                        SeqCst,
                    ) {
                        Ok(_) => break,
                        // State was changed while running, retry.
                        // For multi-core it would be wise to lock_pi new state,
                        // but execution ordering gets complicated
                        Err(_) => continue,
                    }
                }

                if self.lock.futex.value.compare_exchange(
                    thread_info.thread_id,
                    0,
                    SeqCst,
                    SeqCst,
                ).is_err() {
                    self.lock.futex.unlock_pi();
                }

                return res;
            } else {
                let highest = unsafe { highest_ptr.as_ref().unwrap() };

                while !highest.futex.lock_pi().is_ok() {}

                let res = self.lock(thread_info, f);
                
                if let Err(val) = highest.futex.value.compare_exchange(
                    thread_info.thread_id,
                    0,
                    SeqCst,
                    SeqCst,
                ) {
                    if val & FUTEX_TID_MASK == thread_info.thread_id {
                        highest.futex.unlock_pi();
                    }
                }

                return res;
            }
        }
    }

    /// Returns priority ceiling of this mutex
    pub fn ceiling(&self) -> Priority {
        self.lock.ceiling
    }
}
