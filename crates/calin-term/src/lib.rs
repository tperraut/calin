//! Pseudo-terminals for the built-in terminal.
//!
//! [`Terminal::spawn`] starts a program (the user's shell by default) on a new
//! pty, in its own session, and hands back the terminal and its output stream.
//! Nothing here knows about HTTP: the server bridges a [`Terminal`] to the
//! socket described in `calin_proto::terminal`.
//!
//! ```no_run
//! # async fn bridge() -> Result<(), calin_term::TermError> {
//! use calin_term::{Terminal, TerminalConfig};
//!
//! let (terminal, mut output) = Terminal::spawn(&TerminalConfig::new("/tmp").size(120, 40))?;
//! terminal.write("echo hello\n").await?;
//! terminal.resize(100, 30)?;
//! while let Some(bytes) = output.recv().await {
//!     // forward `bytes` as one binary frame
//! }
//! let code: Option<i32> = terminal.wait().await;
//! # Ok(())
//! # }
//! ```
//!
//! Dropping the [`Terminal`] kills everything left in its session.

mod config;
mod session;
mod terminal;

pub use config::TerminalConfig;
pub use terminal::{Output, Terminal};

/// Why a terminal operation failed.
#[derive(Debug, thiserror::Error)]
pub enum TermError {
    /// The pty could not be set up or the program could not be started.
    #[error("failed to start terminal: {0}")]
    Spawn(String),
    /// The program has exited (or was killed); the terminal takes no more input.
    #[error("terminal is closed")]
    Closed,
    /// The pty refused an operation, such as a resize.
    #[error("terminal I/O error: {0}")]
    Io(#[from] std::io::Error),
}
