use std::sync::{Arc, Mutex};
use std::thread;

fn main() {
    let a = Arc::new(Mutex::new(0));
    let b = Arc::new(Mutex::new(0));
    let mut handles = vec![];

    {
        let a = a.clone();
        let b = b.clone();
        let handle = thread::spawn(move || {
            println!("Thread 1 tries a lock");
            let mut a_num = a.lock().unwrap();
            println!("Thread 1 holds a lock");
            *a_num += 1;
            thread::sleep(std::time::Duration::from_millis(100));
            println!("Thread 1 tries b lock");
            let mut b_num = b.lock().unwrap();
            println!("Thread 1 holds b lock");
            *b_num += 1;
            core::mem::drop(b_num);
            println!("Thread 1 released b lock");
            core::mem::drop(a_num);
            println!("Thread 1 released a lock");
        });
        handles.push(handle);
    }

    {
        let a = a.clone();
        let b = b.clone();
        let handle = thread::spawn(move || {
            println!("Thread 2 tries b lock");
            let mut b_num = b.lock().unwrap();
            println!("Thread 2 holds b lock");
            *b_num += 1;
            thread::sleep(std::time::Duration::from_millis(100));
            println!("Thread 2 tries a lock");
            let mut a_num = a.lock().unwrap();
            println!("Thread 2 holds a lock");
            *a_num += 1;
            core::mem::drop(a_num);
            println!("Thread 2 released a lock");
            core::mem::drop(b_num);
            println!("Thread 2 released b lock");
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    println!("Done a: {}, b: {}", *a.lock().unwrap(), *b.lock().unwrap()); // never reach here
}
