use std::sync::Arc;
use std::thread;
use std::time::Instant;

use pcp_mutex::{PcpManager, ThreadPriority};

use lazy_static::lazy_static;

lazy_static! {
    /// This is an example for using doc comment attributes
    static ref PCP_MANAGER: PcpManager = PcpManager::default();
}

fn _bench<T>(f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let ret = f();
    let duration = start.elapsed();

    println!("Time: {:?}", duration);

    ret
}

fn main() {
    let priority = ThreadPriority::init_fifo(1).unwrap();

    let a = Arc::new(PCP_MANAGER.create(0, 3));
    let b = Arc::new(PCP_MANAGER.create(0, 3));

    {
        a.lock(&priority, |t| *t = *t + 1);
        a.lock(&priority, |t| *t = *t + 1);
        a.lock(&priority, |t| *t = *t + 1);
        let test = a.lock(&priority, |t| *t);
        println!("Test: {}", test);
    }
    let mut handles = vec![];

    {
        let a = Arc::clone(&a);
        let b = Arc::clone(&b);
        let handle = thread::spawn(move || {
            let priority = ThreadPriority::init_fifo(2).unwrap();

            println!("Thread 1 tries a lock");
            a.lock(&priority, |a| {
                println!("Thread 1 holds a lock");
                *a += 1;
                thread::sleep(std::time::Duration::from_micros(1000));
                println!("Thread 1 tries b lock");
                b.lock(&priority, |b| {
                    println!("Thread 1 holds b lock");
                    *b += 1;
                });
                println!("Thread 1 released b lock");
            });
            println!("Thread 1 released a lock");
        });
        handles.push(handle);
    }

    {
        let a = Arc::clone(&a);
        let b = Arc::clone(&b);
        let handle = thread::spawn(move || {
            let priority = ThreadPriority::init_fifo(3).unwrap();

            println!("Thread 2 tries b lock");
            b.lock(&priority, |b| {
                println!("Thread 2 holds b lock");
                *b += 1;
                thread::sleep(std::time::Duration::from_micros(1000));
                println!("Thread 2 tries a lock");
                a.lock(&priority, |a| {
                    println!("Thread 2 holds a lock");
                    *a += 1;
                });
                println!("Thread 2 released a lock");
            });
            println!("Thread 2 released b lock");
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    println!("Done {}", a.lock(&priority, |a| *a)); // never reach here
}
