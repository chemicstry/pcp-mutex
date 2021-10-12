# pcp-mutex

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](https://github.com/chemicstry/pcp-mutex)
[![Cargo](https://img.shields.io/crates/v/pcp-mutex.svg)](https://crates.io/crates/pcp-mutex)
[![Documentation](https://docs.rs/pcp-mutex/badge.svg)](https://docs.rs/pcp-mutex)

A Priority Ceiling Protocol (PCP) mutex, based on Linux PI futex. Allows efficient and deadlock free execution.

## How it Works

Linux kernel does not natively support Priority Ceiling Protocol (PCP), which is essential for real-time systems to both ensure good schedulability and deadlock free execution. This crate emulates Original Priority Ceiling Protocol (OPCP) using Priority Inheritance (PI) futexes.

Under the hood each `PcpMutex` belongs to some `PcpGroup`, which contains atomic pointer to the highest locker. Any attempt to lock a `PcpMutex` is firstly checked against this highest locker if PCP conditions for locking are met. On success, futex is locked by an atomic operation involving zero syscalls. On failure, current thread calls futex `LOCK_PI` syscall on the highest locker and kernel raises the priority of the locker.

By default `PcpMutex::new()` uses the default global group. Multiple groups can be used for isolated parts of the program.

This is different from POSIX `PTHREAD_PRIO_PROTECT` mutexes in two ways:
- POSIX mutex calls `sched_setparam` syscall on each lock/unlock, which is much slower. This library has only atomic operations in the fast path.
- POSIX mutexes are individual (no system ceiling) and do not prevent deadlocks. It is not a 'real' PCP.

Each thread also holds an internal thread-local `ThreadState` object which contains the thread id (from `gettid` syscall) and priority (from `sched_getparam`) for internal logic. If thread priority is changed manually (i.e. `sched_setparam` syscall), then `thread::update_priority()` must be called to update internal state. For real-time applications there is a convenience function `thread::init_fifo_priority(priority: u8)`, which sets scheduling policy to `SCHED_FIFO` with a given priority and also updates the internal state. Avoid changing thread priority while holding a mutex as that might cause a deadlock.

## Example

Locking 2 mutexes in different order would result in deadlock, but PCP prevents that:

```rust
let a = Arc::new(PcpMutex::new(0, 3));
let b = Arc::new(PcpMutex::new(0, 3));

{
    let a = a.clone();
    let b = b.clone();
    thread::spawn(move || {
        println!("Thread 1 tries a lock");
        a.lock(|a| {
            println!("Thread 1 holds a lock");
            *a += 1;
            thread::sleep(std::time::Duration::from_millis(100));
            println!("Thread 1 tries b lock");
            b.lock(|b| {
                println!("Thread 1 holds b lock");
                *b += 1;
            });
            println!("Thread 1 released b lock");
        });
        println!("Thread 1 released a lock");
    });
}

{
    let a = a.clone();
    let b = b.clone();
    thread::spawn(move || {
        println!("Thread 2 tries b lock");
        b.lock(|b| {
            println!("Thread 2 holds b lock");
            *b += 1;
            thread::sleep(std::time::Duration::from_millis(100));
            println!("Thread 2 tries a lock");
            a.lock(|a| {
                println!("Thread 2 holds a lock");
                *a += 1;
            });
            println!("Thread 2 released a lock");
        });
        println!("Thread 2 released b lock");
    });
}
```

Output:
```
Thread 1 tries a lock
Thread 1 holds a lock
Thread 2 tries b lock <--- thread 2 is prevented from taking a lock here by PCP
Thread 1 tries b lock
Thread 1 holds b lock
Thread 1 released b lock
Thread 1 released a lock
Thread 2 holds b lock
Thread 2 tries a lock
Thread 2 holds a lock
Thread 2 released a lock
Thread 2 released b lock
```

## Credits

This work was done as a part of my Thesis at University of Twente.
