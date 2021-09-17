use std::sync::Arc;
use std::thread;
use std::time::Instant;

use pcp_mutex::PcpManager;

use lazy_static::lazy_static;

lazy_static! {
    /// This is an example for using doc comment attributes
    static ref PCP_MANAGER: PcpManager = PcpManager::default();
}

fn bench<T>(f: impl FnOnce() -> T) -> T {
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
        let test = a.lock(1);
        let test = bench(|| *test+1);
        let test = bench(|| test+1);
        let test = bench(|| test+1);
        println!("Test: {}", test);
    }
    let mut handles = vec![];

    {
        let a = Arc::clone(&a);
        let b = Arc::clone(&b);
        let handle = thread::spawn(move || {
            println!("Thread 1 tries a lock");
            let mut a_num = bench(|| a.lock(1));
            *a_num += 1;
            println!("Thread 1 holds a lock");
            thread::sleep(std::time::Duration::from_millis(100));
            println!("Thread 1 tries b lock");
            let mut b_num = bench(|| b.lock(1));
            println!("Thread 1 holds b lock");
            *b_num += 1;
            bench(|| drop(a_num));
            bench(|| drop(b_num));
        });
        handles.push(handle);
    }

    {
        let a = Arc::clone(&a);
        let b = Arc::clone(&b);
        let handle = thread::spawn(move || {
            println!("Thread 2 tries b lock");
            let mut b_num = bench(|| b.lock(1));
            *b_num += 1;
            println!("Thread 2 holds b lock");
            thread::sleep(std::time::Duration::from_millis(100));
            println!("Thread 2 tries a lock");
            let mut a_num = bench(|| a.lock(1));
            println!("Thread 2 holds a lock");
            *a_num += 1;
            bench(|| drop(a_num));
            bench(|| drop(b_num));
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    println!("Done {}", *a.lock(1)); // never reach here
}
