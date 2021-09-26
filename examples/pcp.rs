use std::sync::Arc;
use std::thread;
use std::time::Instant;

use pcp_mutex::PcpManager;

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
    let a = Arc::new(PCP_MANAGER.create(0, 1));
    let b = Arc::new(PCP_MANAGER.create(0, 1));

    {
        a.lock(1, |t| *t = *t + 1);
        a.lock(1, |t| *t = *t + 1);
        a.lock(1, |t| *t = *t + 1);
        let test = a.lock(1, |t| *t);
        println!("Test: {}", test);
    }
    let mut handles = vec![];

    {
        let a = Arc::clone(&a);
        let b = Arc::clone(&b);
        let handle = thread::spawn(move || {
            println!("Thread 1 tries a lock");
            a.lock(1, |a| {
                println!("Thread 1 holds a lock");
                *a += 1;
                thread::sleep(std::time::Duration::from_micros(1000));
                println!("Thread 1 tries b lock");
                b.lock(1, |b| {
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
            println!("Thread 2 tries b lock");
            b.lock(1, |b| {
                println!("Thread 2 holds b lock");
                *b += 1;
                thread::sleep(std::time::Duration::from_micros(1000));
                println!("Thread 2 tries a lock");
                a.lock(1, |a| {
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

    println!("Done {}", a.lock(1, |a| *a)); // never reach here
}
