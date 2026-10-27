//! The text that scrolls on the screen, shared between the thread that fetches it and the screen.
//!
//! Nothing here touches hardware. [`SharedMessage`] is a cheap handle (clone it for every thread
//! that needs it). The screen asks [`SharedMessage::take_update`] on every frame, which only
//! copies the text when it has changed.

use std::sync::{Arc, Mutex, PoisonError};

/// The current text and a counter that grows with every change.
#[derive(Debug, Default)]
struct Inner {
    generation: u64,
    text: String,
}

/// A text shared between threads.
#[derive(Clone, Debug, Default)]
pub struct SharedMessage(Arc<Mutex<Inner>>);

impl SharedMessage {
    /// A message that starts with `text`.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            generation: 1,
            text: text.into(),
        })))
    }

    /// Replaces the text. Setting the text it already has changes nothing.
    pub fn set(&self, text: impl Into<String>) {
        let text = text.into();
        let mut inner = self.lock();
        if inner.text != text {
            inner.text = text;
            inner.generation += 1;
        }
    }

    /// Returns the text if it changed since `seen`, and stores the new generation in `seen`.
    /// Start with `seen` at 0 to get the first text.
    pub fn take_update(&self, seen: &mut u64) -> Option<String> {
        let inner = self.lock();
        (inner.generation != *seen).then(|| {
            *seen = inner.generation;
            inner.text.clone()
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock only means another thread panicked; the data is still a valid text.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
