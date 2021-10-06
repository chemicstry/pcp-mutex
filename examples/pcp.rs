use std::sync::Arc;
use std::thread;

use pcp_mutex::{PcpMutex, ThreadState};

fn main() {
    let priority = ThreadState::from_sys();

    let a = Arc::new(PcpMutex::new(0, 3));
    let b = Arc::new(PcpMutex::new(0, 3));

    let mut handles = vec![];

    {
        let a = a.clone();
        let b = b.clone();
        let handle = thread::spawn(move || {
            let priority = ThreadState::from_sys();

            println!("Thread 1 tries a lock");
            a.lock(&priority, |a| {
                println!("Thread 1 holds a lock");
                *a += 1;
                thread::sleep(std::time::Duration::from_millis(100));
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
        let a = a.clone();
        let b = b.clone();
        let handle = thread::spawn(move || {
            let priority = ThreadState::from_sys();

            println!("Thread 2 tries b lock");
            b.lock(&priority, |b| {
                println!("Thread 2 holds b lock");
                *b += 1;
                thread::sleep(std::time::Duration::from_millis(100));
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

    println!("Done a: {}, b: {}", a.lock(&priority, |a| *a), b.lock(&priority, |b| *b));
}
