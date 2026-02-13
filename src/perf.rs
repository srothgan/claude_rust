// claude_rust — A native Rust terminal interface for Claude Code
// Copyright (C) 2025  Simon Peter Rothgang
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Lightweight per-frame performance logger for rendering instrumentation.
//!
//! Gated behind `--features perf`. When the feature is disabled, all types
//! become zero-size and all methods are no-ops that the compiler eliminates.
//!
//! # Usage
//!
//! ```bash
//! cargo run --features perf
//! # Creates performance.log with JSON lines:
//! # {"frame":1234,"fn":"chat::render_msgs","ms":2.345,"n":42}
//! ```

#[cfg(feature = "perf")]
mod enabled {
    use std::cell::RefCell;
    use std::fs::{File, OpenOptions};
    use std::io::Write;
    use std::path::Path;
    use std::time::Instant;

    // Thread-local file handle so Timer::drop can log without borrowing PerfLogger.
    thread_local! {
        pub(crate) static LOG_FILE: RefCell<Option<File>> = const { RefCell::new(None) };
        static FRAME_COUNTER: RefCell<u64> = const { RefCell::new(0) };
    }

    pub struct PerfLogger {
        _private: (),
    }

    #[allow(clippy::unused_self)]
    impl PerfLogger {
        /// Open (or create) the log file. Returns `None` on I/O error.
        pub fn open(path: &Path) -> Option<Self> {
            let file =
                OpenOptions::new().create(true).truncate(true).write(true).open(path).ok()?;
            LOG_FILE.with(|f| *f.borrow_mut() = Some(file));
            Some(Self { _private: () })
        }

        /// Increment the frame counter. Call once at the start of each render frame.
        pub fn next_frame(&mut self) {
            FRAME_COUNTER.with(|c| *c.borrow_mut() += 1);
        }

        /// Start a named timer. Logs duration on drop.
        #[must_use]
        pub fn start(&self, name: &'static str) -> Timer {
            Timer { name, start: Instant::now(), extra: None }
        }

        /// Start a named timer with an extra numeric field (e.g. message count).
        #[must_use]
        pub fn start_with(
            &self,
            name: &'static str,
            extra_name: &'static str,
            extra_val: usize,
        ) -> Timer {
            Timer { name, start: Instant::now(), extra: Some((extra_name, extra_val)) }
        }
    }

    pub struct Timer {
        pub(crate) name: &'static str,
        pub(crate) start: Instant,
        pub(crate) extra: Option<(&'static str, usize)>,
    }

    #[allow(clippy::unused_self)]
    impl Timer {
        /// Manually stop and log. Useful when you need to end timing before scope exit.
        pub fn stop(self) {
            // Drop impl handles logging
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            let ms = self.start.elapsed().as_secs_f64() * 1000.0;
            let frame = FRAME_COUNTER.with(|c| *c.borrow());
            let name = self.name;
            LOG_FILE.with(|f| {
                if let Some(ref mut file) = *f.borrow_mut() {
                    if let Some((k, v)) = self.extra {
                        let _ = writeln!(
                            file,
                            r#"{{"frame":{frame},"fn":"{name}","ms":{ms:.3},"{k}":{v}}}"#,
                        );
                    } else {
                        let _ =
                            writeln!(file, r#"{{"frame":{frame},"fn":"{name}","ms":{ms:.3}}}"#,);
                    }
                }
            });
        }
    }
}

#[cfg(not(feature = "perf"))]
mod disabled {
    use std::path::Path;

    pub struct PerfLogger;
    pub struct Timer;

    #[allow(clippy::unused_self)]
    impl PerfLogger {
        #[inline]
        pub fn open(_path: &Path) -> Option<Self> {
            None
        }
        #[inline]
        pub fn next_frame(&mut self) {}
        #[inline]
        #[must_use]
        pub fn start(&self, _name: &'static str) -> Timer {
            Timer
        }
        #[inline]
        #[must_use]
        pub fn start_with(
            &self,
            _name: &'static str,
            _extra_name: &'static str,
            _extra_val: usize,
        ) -> Timer {
            Timer
        }
    }

    #[allow(clippy::unused_self)]
    impl Timer {
        #[inline]
        pub fn stop(self) {}
    }
}

/// Start a timer without needing a `PerfLogger` reference.
/// Uses the thread-local log file directly. Returns `None` (and is a no-op)
/// when the `perf` feature is disabled or no logger has been opened.
#[cfg(feature = "perf")]
#[must_use]
#[inline]
pub fn start(name: &'static str) -> Option<Timer> {
    // Only create a timer if the log file is actually open
    enabled::LOG_FILE.with(|f| {
        if f.borrow().is_some() {
            Some(Timer { name, start: std::time::Instant::now(), extra: None })
        } else {
            None
        }
    })
}

#[cfg(not(feature = "perf"))]
#[must_use]
#[inline]
pub fn start(_name: &'static str) -> Option<Timer> {
    None
}

#[cfg(feature = "perf")]
pub use enabled::{PerfLogger, Timer};

#[cfg(not(feature = "perf"))]
pub use disabled::{PerfLogger, Timer};
