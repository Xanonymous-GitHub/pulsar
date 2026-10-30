//! The text that scrolls on the screen, shared between the thread that fetches it and the screen.
//!
//! Nothing here touches hardware. [`SharedMessage`] is a cheap handle (clone it for every thread
//! that needs it). The screen asks [`SharedMessage::take_update`] on every frame, which only
//! copies the text when it has changed.
//!
//! Besides the normal text (the weather) there is a **notice**: a text that shows instead of the
//! normal one for a limited time, see [`SharedMessage::show_notice`]. The normal text keeps
//! updating underneath, and comes back when the notice ends.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A text that hides the normal one until `until`.
#[derive(Debug)]
struct Notice {
    text: String,
    until: Instant,
}

/// The texts and a counter that grows whenever the **visible** text changes.
#[derive(Debug, Default)]
struct Inner {
    generation: u64,
    text: String,
    notice: Option<Notice>,
}

impl Inner {
    /// Ends the notice if its time is up. That changes the visible text, so the counter grows.
    fn expire_notice(&mut self) {
        if self
            .notice
            .as_ref()
            .is_some_and(|notice| Instant::now() >= notice.until)
        {
            self.notice = None;
            self.generation += 1;
        }
    }

    /// What the screen should show: the notice if there is one, else the normal text.
    fn visible(&self) -> &str {
        self.notice
            .as_ref()
            .map_or(self.text.as_str(), |notice| notice.text.as_str())
    }
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
            notice: None,
        })))
    }

    /// Replaces the normal text. Setting the text it already has changes nothing. While a notice
    /// is showing, the new text waits underneath it.
    pub fn set(&self, text: impl Into<String>) {
        let text = text.into();
        let mut inner = self.lock();
        inner.expire_notice();
        if inner.text != text {
            inner.text = text;
            if inner.notice.is_none() {
                inner.generation += 1;
            }
        }
    }

    /// Shows `text` instead of the normal text for `duration`. A new notice replaces the one that
    /// is showing, and starts its time from zero.
    pub fn show_notice(&self, text: impl Into<String>, duration: Duration) {
        let mut inner = self.lock();
        inner.notice = Some(Notice {
            text: text.into(),
            until: Instant::now() + duration,
        });
        inner.generation += 1;
    }

    /// The normal text, without any notice.
    #[must_use]
    pub fn text(&self) -> String {
        self.lock().text.clone()
    }

    /// Returns the visible text if it changed since `seen`, and stores the new generation in
    /// `seen`. Start with `seen` at 0 to get the first text.
    pub fn take_update(&self, seen: &mut u64) -> Option<String> {
        let mut inner = self.lock();
        inner.expire_notice();
        (inner.generation != *seen).then(|| {
            *seen = inner.generation;
            inner.visible().to_owned()
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock only means another thread panicked; the data is still a valid text.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
