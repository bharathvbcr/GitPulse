// Copyright 2026 GitPulse contributors
// SPDX-License-Identifier: Apache-2.0

//! Replacement for the removed GLib channel API. The receiver stays on its
//! owning main context; senders never execute GTK callbacks on another thread.
use gtk::glib::{self, ControlFlow, MainContext, Priority};
use std::{sync::mpsc::SendError, time::Duration};

pub(crate) struct Sender<T>(async_channel::Sender<T>);
pub(crate) struct Receiver<T>(async_channel::Receiver<T>, Priority);
pub(crate) struct AttachedReceiver(glib::JoinHandle<()>);

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Sender<T> {
    pub(crate) fn send(&self, value: T) -> Result<(), SendError<T>> {
        // Keep the former channel's nonblocking, ordered, unbounded send contract.
        self.0
            .try_send(value)
            .map_err(|e| SendError(e.into_inner()))
    }
}

pub(crate) fn channel<T>(priority: Priority) -> (Sender<T>, Receiver<T>) {
    let (tx, rx) = async_channel::unbounded();
    (Sender(tx), Receiver(rx, priority))
}

impl<T: 'static> Receiver<T> {
    pub(crate) fn attach(
        self,
        context: Option<&MainContext>,
        mut callback: impl FnMut(T) -> ControlFlow + 'static,
    ) -> AttachedReceiver {
        let context = context.cloned().unwrap_or_default();
        AttachedReceiver(context.spawn_local_with_priority(self.1, async move {
            let mut dispatched = 0;
            while let Ok(value) = self.0.recv().await {
                if callback(value) == ControlFlow::Break {
                    break;
                }
                dispatched += 1;
                // A continuous producer must not monopolize the GTK main context.
                if dispatched == 64 {
                    glib::timeout_future_with_priority(self.1, Duration::ZERO).await;
                    dispatched = 0;
                }
            }
        }))
    }
}

impl Drop for AttachedReceiver {
    fn drop(&mut self) {
        // Dropping an event loop closes its channels even if a window or portal
        // still holds a sender. GLib JoinHandle alone detaches on drop.
        self.0.abort();
    }
}
