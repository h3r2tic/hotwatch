//! `hotwatch` is a Rust library for comfortably watching and handling file changes.
//! It's a thin convenience wrapper over [`notify`](https://github.com/passcod/notify),
//! allowing you to easily set callbacks for each path you want to watch.
//!
//! Watching is done on a separate thread so you don't have to worry about blocking.
//! All handlers are run on that thread as well, so keep that in mind when attempting to access
//! outside data from within a handler.
//!
//! (There's also a [`blocking`] mode, in case you're a big fan of blocking.)
//!
//! Only the latest stable version of Rust is supported.

pub mod blocking;
mod util;

pub use notify::{self, DebouncedEvent as Event};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        mpsc::{channel, Receiver},
        Arc, Mutex,
    },
};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Notify(notify::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(fmt),
            Self::Notify(error) => error.fmt(fmt),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => error.source(),
            Self::Notify(error) => error.source(),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<notify::Error> for Error {
    fn from(err: notify::Error) -> Self {
        if let notify::Error::Io(err) = err {
            err.into()
        } else {
            Self::Notify(err)
        }
    }
}

type HandlerMap = HashMap<PathBuf, Box<dyn FnMut(Event) + Send>>;

/// A non-blocking hotwatch instance.
///
/// Watching begins as soon as [`Self::watch`] is called, and occurs in a
/// background thread. The background thread runs until this is dropped.
///
/// Dropping this will also unwatch everything.
pub struct GenericHotwatch<W: notify::Watcher> {
    watcher: W,
    handlers: Arc<Mutex<HandlerMap>>,
}

pub type Hotwatch = GenericHotwatch<notify::RecommendedWatcher>;

impl<W: notify::Watcher> std::fmt::Debug for GenericHotwatch<W> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.debug_struct("Hotwatch").finish()
    }
}

impl<W: notify::Watcher> GenericHotwatch<W> {
    /// Creates a new non-blocking hotwatch instance.
    ///
    /// # Errors
    ///
    /// This will fail if the underlying [notify](https://docs.rs/notify/4.0/notify/)
    /// instance fails to initialize.
    ///
    /// # Examples
    ///
    /// ```
    /// use hotwatch::Hotwatch;
    ///
    /// let hotwatch = Hotwatch::new().expect("hotwatch failed to initialize");
    /// ```
    pub fn new() -> Result<Self, Error> {
        Self::new_with_custom_delay(std::time::Duration::from_secs(2))
    }

    /// Using [`Hotwatch::new`] will give you a default delay of 2 seconds.
    /// This method allows you to specify your own value.
    ///
    /// # Notes
    ///
    /// A delay of over 30 seconds will prevent repetitions of previous events on macOS.
    pub fn new_with_custom_delay(delay: std::time::Duration) -> Result<Self, Error> {
        let (tx, rx) = channel();
        let handlers = Arc::<Mutex<_>>::default();
        Self::run(Arc::clone(&handlers), rx);
        let watcher = notify::Watcher::new(tx, delay).map_err(Error::Notify)?;
        Ok(Self { watcher, handlers })
    }

    /// Watch a path and register a handler to it.
    ///
    /// When watching a directory, that handler will receive all events for all directory
    /// contents, even recursing through subdirectories.
    ///
    /// Only the most specific applicable handler will be called. In other words, if you're
    /// watching "dir" and "dir/file1", then only the latter handler will fire for changes to
    /// `file1`.
    ///
    /// Note that handlers will be run in hotwatch's watch thread, so you'll have to use `move`
    /// if the closure captures anything.
    ///
    /// # Errors
    ///
    /// Watching will fail if the path can't be read, returning [`Error::Io`].
    ///
    /// # Examples
    ///
    /// ```
    /// use hotwatch::{Hotwatch, Event};
    ///
    /// let mut hotwatch = Hotwatch::new().expect("hotwatch failed to initialize!");
    /// hotwatch.watch("README.md", |event: Event| {
    ///     if let Event::Write(path) = event {
    ///         println!("{:?} changed!", path);
    ///     }
    /// }).expect("failed to watch file!");
    /// ```
    pub fn watch<P, F>(&mut self, path: P, handler: F) -> Result<(), Error>
    where
        P: AsRef<Path>,
        F: 'static + FnMut(Event) + Send,
    {
        let absolute_path = path.as_ref().canonicalize()?;
        self.watcher
            .watch(&absolute_path, notify::RecursiveMode::Recursive)?;
        let mut handlers = self.handlers.lock().expect("handler mutex poisoned!");
        handlers.insert(absolute_path, Box::new(handler));
        Ok(())
    }

    /// Stop watching a path.
    ///
    /// # Errors
    ///
    /// This will fail if the path wasn't being watched, or if the path
    /// couldn't be unwatched for some platform-specific internal reason.
    pub fn unwatch<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        let absolute_path = path.as_ref().canonicalize()?;
        self.watcher.unwatch(&absolute_path)?;
        let mut handlers = self.handlers.lock().expect("handler mutex poisoned!");
        handlers.remove(&absolute_path);
        Ok(())
    }

    fn run(handlers: Arc<Mutex<HandlerMap>>, rx: Receiver<Event>) {
        std::thread::spawn(move || loop {
            match rx.recv() {
                Ok(event) => {
                    util::log_event(&event);
                    let mut handlers = handlers.lock().expect("handler mutex poisoned!");
                    if let Some(handler) = util::handler_for_event(&event, &mut handlers) {
                        handler(event);
                    }
                }
                Err(_) => {
                    util::log_dead();
                    break;
                }
            }
        });
    }
}
