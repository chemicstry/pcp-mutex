use lazy_static::lazy_static;
use linux_futex::{PiFutex, Private};
use std::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::MaybeUninit,
    sync::{
        atomic::{self, AtomicPtr},
        Arc,
    },
};

pub type Priority = i8;
pub type ThreadId = i32;

// Futex can contain WAITERS bit, so we use mask to obtain thread id
const FUTEX_TID_MASK: i32 = PiFutex::<linux_futex::Private>::TID_MASK;

lazy_static! {
    // Default PcpGroup when using `PcpMutex::new()`
    static ref DEFAULT_GROUP: PcpGroup = Default::default();
    // Used to temporarily block highest_locker AtomicPtr
    static ref BLOCKED_LOCKER: PcpMutexLock = PcpMutexLock {
        ceiling: Priority::MAX,
        futex: PiFutex::default(),
    };
}

/// A helper object that contains thread id and keeps track of current priority
#[derive(Debug)]
pub struct ThreadState {
    priority: Cell<Priority>,
    thread_id: ThreadId,
    _non_send: PhantomData<*const u8>,
}

impl ThreadState {
    /// Creates a new `ThreadState` with a given priority.
    ///
    /// # Safety
    ///
    /// The given priority and thread id must be valid.
    pub unsafe fn new(priority: Priority, thread_id: ThreadId) -> Self {
        assert!(priority >= 0, "Priority must be greater or equal to 0");

        Self {
            priority: Cell::new(priority),
            thread_id,
            _non_send: PhantomData::default(),
        }
    }

    /// Constructs `ThreadState` from `gettid` and `sched_getparam` syscalls
    pub fn from_sys() -> Self {
        let mut sched_param = MaybeUninit::<libc::sched_param>::uninit();

        unsafe {
            let thread_id = libc::syscall(libc::SYS_gettid) as _;
            libc::sched_getparam(0, sched_param.as_mut_ptr());
            Self::new(sched_param.assume_init().sched_priority as i8, thread_id)
        }
    }

    /// Sets current thread scheduling policy to SCHED_FIFO with a given priority and returns `ThreadState`
    pub fn init_fifo(priority: Priority) -> std::io::Result<Self> {
        let param = libc::sched_param {
            sched_priority: priority as i32,
        };

        unsafe {
            let thread = libc::syscall(libc::SYS_gettid) as _;

            if libc::sched_setscheduler(0, libc::SCHED_FIFO, &param) != 0 {
                return Err(std::io::Error::last_os_error());
            }

            Ok(Self::new(priority, thread))
        }
    }

    /// Returns id of the thread
    pub fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    /// Internal method for setting priority to the locked resource ceiling.
    fn set_priority(&self, priority: Priority) {
        self.priority.set(priority);
    }

    /// Returns the current thread priority
    pub fn get_priority(&self) -> Priority {
        self.priority.get()
    }
}

/// Group of mutexes that share the same system ceiling
#[derive(Debug, Default)]
pub struct PcpGroup {
    // Pointer to the locked mutex with the highest ceiling
    highest_locker: AtomicPtr<PcpMutexLock>,
}

impl PcpGroup {
    /// Creates a new mutex with a given priority ceiling.
    /// Ceiling must be calculated manually.
    pub fn create<'a, T>(&'a self, res: T, ceiling: Priority) -> PcpMutex<'a, T> {
        PcpMutex {
            res: UnsafeCell::new(res),
            group: &self,
            lock: Arc::new(PcpMutexLock {
                ceiling,
                futex: PiFutex::default(),
            }),
        }
    }
}

#[derive(Debug)]
struct PcpMutexLock {
    /// Static priority ceiling of the mutex
    ceiling: Priority,
    /// PiFutex that is used to lock the mutex
    futex: PiFutex<Private>,
}

/// A Priority Ceiling Protocol mutex
#[derive(Debug)]
pub struct PcpMutex<'a, T> {
    /// Resource protected by the mutex
    res: UnsafeCell<T>,
    /// Group which this mutex belongs to
    group: &'a PcpGroup,
    /// Internal lock implementation
    lock: Arc<PcpMutexLock>,
}

// PcpMutex is not Send nor Sync because we use UnsafeCell.
// It is save to `Send` it between threads as long as resource itself is `Send`.
unsafe impl<'a, T> Send for PcpMutex<'a, T> where T: Send {}
// We ensure `Sync` behavior by locking mechanism in `PcpMutex::lock()`
unsafe impl<'a, T> Sync for PcpMutex<'a, T> where T: Send {}

// Performs PCP check if thread can lock a mutex.
fn can_lock(highest: &Option<Arc<PcpMutexLock>>, thread_info: &ThreadState) -> bool {
    if let Some(highest) = highest {
        if thread_info.get_priority() > highest.ceiling {
            // Thread priority is higher than the system ceiling, lock can be taken
            true
        } else {
            // Get thread id of the highest locker
            let locker_tid = highest.futex.value.load(atomic::Ordering::Relaxed) & FUTEX_TID_MASK;

            if thread_info.get_priority() == highest.ceiling && thread_info.thread_id == locker_tid
            {
                // Highest locker is the same thread, lock can be taken
                true
            } else {
                // Thread does not meet PCP criteria and cannot take the lock
                false
            }
        }
    } else {
        // There are no locked mutexes, lock can be taken
        true
    }
}

// This safely clones Arc that is in the highest_locker AtomicPtr by temporarily swapping in BLOCKED_LOCKER.
// Without this, Arc could be dropped between AtomicPtr::load() and Arc::clone() calls.
// There is probably a more efficient way of doing this.
fn get_highest_locker(
    ptr: &AtomicPtr<PcpMutexLock>,
    thread_info: &ThreadState,
) -> (*mut PcpMutexLock, Option<Arc<PcpMutexLock>>) {
    use atomic::Ordering::*;

    if BLOCKED_LOCKER
        .futex
        .value
        .compare_exchange(0, thread_info.thread_id, Acquire, Relaxed)
        .is_err()
    {
        while !BLOCKED_LOCKER.futex.lock_pi().is_ok() {}
    }

    let blocked_ptr = unsafe { core::mem::transmute(&*BLOCKED_LOCKER) };
    let highest_ptr = ptr.swap(blocked_ptr, Acquire);

    let highest = if highest_ptr.is_null() {
        None
    } else {
        let arc = unsafe { Arc::from_raw(highest_ptr) };
        let arc_new = arc.clone();
        // prevent original ref count from being decremented
        core::mem::forget(arc);
        Some(arc_new)
    };

    while !ptr
        .compare_exchange(blocked_ptr, highest_ptr, Release, Relaxed)
        .is_ok()
    {}

    if BLOCKED_LOCKER
        .futex
        .value
        .compare_exchange(thread_info.thread_id, 0, Release, Relaxed)
        .is_err()
    {
        BLOCKED_LOCKER.futex.unlock_pi();
    }

    (highest_ptr, highest)
}

impl<'a, T> PcpMutex<'a, T> {
    /// Creates a new PcpMutex with a default global group
    pub fn new(res: T, ceiling: Priority) -> Self {
        DEFAULT_GROUP.create(res, ceiling)
    }

    /// Locks the mutex and executes critical section in a closure.
    pub fn lock<R>(&'a self, thread_info: &ThreadState, f: impl FnOnce(&mut T) -> R) -> R {
        use atomic::Ordering::*;

        // Get pointer to our lock.
        // If any thread is waiting on this lock, they will firstly clone the Arc.
        // We transmute *const to *mut, because AtomicPtr only takes *mut pointer.
        // However, we only use immutable references to PcpMutexLock so this is safe.
        let self_ptr = unsafe { core::mem::transmute(Arc::as_ptr(&self.lock)) };

        loop {
            // Fetch the current highest locker (system ceiling)
            let (highest_ptr, highest) =
                get_highest_locker(&self.group.highest_locker, thread_info);

            if can_lock(&highest, thread_info) {
                // This would work, but we don't want to create multiple mutable references to the resource
                if highest
                    .map(|l| Arc::ptr_eq(&l, &self.lock))
                    .unwrap_or(false)
                {
                    panic!("PcpMutex is not reentrant!");
                }

                // Try locking the mutex
                if let Err(val) = self.lock.futex.value.compare_exchange(
                    0,
                    thread_info.thread_id,
                    Acquire,
                    Acquire,
                ) {
                    // Futex might already contain current thread, because of `lock_pi()` syscall.
                    // Use TID mask, because WAITERS bit might be set and comparison would fail.
                    if val & FUTEX_TID_MASK != thread_info.thread_id {
                        // Some other thread locked futex before us.
                        while !self.lock.futex.lock_pi().is_ok() {}
                        // Retry
                        continue;
                    }
                }

                // Set the new highest locker
                if self
                    .group
                    .highest_locker
                    .compare_exchange(highest_ptr, self_ptr, SeqCst, SeqCst)
                    .is_err()
                {
                    // Race condition.
                    // Unlock the acquired futex and retry
                    if self
                        .lock
                        .futex
                        .value
                        .compare_exchange(thread_info.thread_id, 0, SeqCst, SeqCst)
                        .is_err()
                    {
                        self.lock.futex.unlock_pi();
                    }

                    // retry
                    continue;
                }

                let prev_priority = thread_info.get_priority();
                // Note that actual priority will only be updated when another thread calls `lock_pi()` on the futex.
                thread_info.set_priority(self.lock.ceiling);

                // Enter the critical section.
                // Accessing resource is safe because no other thread is allowed to reach this.
                let result = f(unsafe { &mut *self.res.get() });

                // Restore priority
                thread_info.set_priority(prev_priority);

                loop {
                    match self.group.highest_locker.compare_exchange(
                        self_ptr,
                        highest_ptr,
                        SeqCst,
                        SeqCst,
                    ) {
                        Ok(_) => break,
                        // State was changed while running, retry.
                        // For multi-core it would be wise to lock_pi new state,
                        // but execution ordering gets complicated, so just busy wait.
                        Err(_) => continue,
                    }
                }

                // Unlock the futex
                if self
                    .lock
                    .futex
                    .value
                    .compare_exchange(thread_info.thread_id, 0, SeqCst, SeqCst)
                    .is_err()
                {
                    // WAITERS bit was set and we must unlock via syscall to wakeup waiting thread.
                    self.lock.futex.unlock_pi();
                }

                return result;
            } else {
                // Thread hit the system ceiling and could not take the lock. It must wait.

                // can_lock will not return false if Option is None so this never fails
                let highest = highest.unwrap();

                // Suspend current thread. Kernel will raise the locker priority automatically.
                while !highest.futex.lock_pi().is_ok() {}

                // Retry locking recursively
                let res = self.lock(thread_info, f);

                // Release the acquired `highest` lock
                if let Err(val) =
                    highest
                        .futex
                        .value
                        .compare_exchange(thread_info.thread_id, 0, SeqCst, SeqCst)
                {
                    // Lock could already have been unlocked by a recursive self.lock() call
                    if val != 0 {
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
