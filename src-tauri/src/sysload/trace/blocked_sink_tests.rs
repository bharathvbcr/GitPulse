//! A trace sink that blocks until released: what a flush that stalls on the
//! disk looks like to the feed, without touching the disk.
use super::{TraceOpener, TraceRecord, TraceSink, TraceWriter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Closed until [`Gate::release`]. Records that a caller reached it.
pub(in crate::sysload) struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
    entered: AtomicBool,
}

impl Gate {
    pub(in crate::sysload) fn new() -> Arc<Self> {
        Arc::new(Self {
            open: Mutex::new(false),
            changed: Condvar::new(),
            entered: AtomicBool::new(false),
        })
    }

    pub(in crate::sysload) fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    /// Whether something is, or has been, waiting at the gate.
    pub(in crate::sysload) fn entered(&self) -> bool {
        self.entered.load(Ordering::SeqCst)
    }

    /// Waits up to `within` for something to reach the gate.
    pub(in crate::sysload) fn wait_entered(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while !self.entered() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.entered()
    }

    fn pass(&self) {
        self.entered.store(true, Ordering::SeqCst);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }
}

struct GatedSink {
    ring: TraceWriter,
    gate: Arc<Gate>,
}

impl TraceSink for GatedSink {
    fn append(&mut self, record: &TraceRecord) -> Result<(), String> {
        self.gate.pass();
        self.ring.append(record)
    }
}

/// Opens the real ring, and blocks every write at `gate`.
pub(in crate::sysload) fn blocked_writes(gate: Arc<Gate>) -> TraceOpener {
    Arc::new(move |dir, boot_id| {
        Ok(Box::new(GatedSink {
            ring: TraceWriter::open(dir, boot_id)?,
            gate: gate.clone(),
        }) as Box<dyn TraceSink>)
    })
}

/// Blocks the open itself at `gate`, then opens the real ring.
pub(in crate::sysload) fn blocked_open(gate: Arc<Gate>) -> TraceOpener {
    Arc::new(move |dir, boot_id| {
        gate.pass();
        TraceWriter::open(dir, boot_id).map(|w| Box::new(w) as Box<dyn TraceSink>)
    })
}
