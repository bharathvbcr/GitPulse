#![cfg(target_os = "linux")]

// Test the actual adapter linked by the patched TAO runtime, including shutdown
// and main-thread ownership. No second implementation in the test fixture.
#[path = "../framework/tao/src/platform_impl/linux/main_context_channel.rs"]
mod main_context_channel;

use gtk::glib::{ControlFlow, MainContext, Priority};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

fn pump_until(context: &MainContext, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "main-context dispatch timed out");
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn dispatch_preserves_order_thread_affinity_and_closed_channel_errors() {
    let context = MainContext::new();
    let _owner = context.acquire().unwrap();
    let main_thread = std::thread::current().id();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let received = seen.clone();
    let (tx, rx) = main_context_channel::channel(Priority::DEFAULT);
    let source = rx.attach(Some(&context), move |value| {
        assert_eq!(std::thread::current().id(), main_thread);
        received.borrow_mut().push(value);
        if value == 127 {
            ControlFlow::Break
        } else {
            ControlFlow::Continue
        }
    });
    let worker = tx.clone();
    std::thread::spawn(move || {
        for value in 0..128 {
            worker.send(value).unwrap();
        }
    })
    .join()
    .unwrap();
    pump_until(&context, || seen.borrow().len() == 128);
    assert_eq!(*seen.borrow(), (0..128).collect::<Vec<_>>());
    assert_eq!(tx.send(999).unwrap_err().0, 999);
    drop(source);

    let (tx, rx) = main_context_channel::channel(Priority::DEFAULT);
    tx.send("queued").unwrap();
    let source = rx.attach(Some(&context), |_| panic!("cancelled source dispatched"));
    drop(source);
    assert_eq!(tx.send("after drop").unwrap_err().0, "after drop");
    context.iteration(false);
}

#[test]
fn continuous_messages_yield_to_other_main_context_sources() {
    let context = MainContext::new();
    let _owner = context.acquire().unwrap();
    let count = Rc::new(Cell::new(0));
    let dispatched = count.clone();
    let (tx, rx) = main_context_channel::channel(Priority::DEFAULT);
    for value in 0..512 {
        tx.send(value).unwrap();
    }
    let source = rx.attach(Some(&context), move |_| {
        dispatched.set(dispatched.get() + 1);
        ControlFlow::Continue
    });
    let observed = Rc::new(Cell::new(None));
    let observed_by_task = observed.clone();
    let count_at_tick = count.clone();
    let tick = context.spawn_local(async move {
        observed_by_task.set(Some(count_at_tick.get()));
    });
    pump_until(&context, || count.get() == 512 && observed.get().is_some());
    assert!(
        observed.get().unwrap() <= 64,
        "message flood starved other sources"
    );
    drop(tx);
    drop(source);
    tick.abort();
}
