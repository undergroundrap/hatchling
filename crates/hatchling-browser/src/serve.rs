//! The controller's loop: one command in, one reply out, over the cell's
//! connection to the broker, until the broker closes it or says close.

use std::os::unix::net::UnixStream;

use hatchling_core::browser::{BrowserCommand, BrowserFailure, BrowserReply};
use hatchling_core::framing::{read_frame_or_eof, write_frame};

use crate::page::Session;

/// Why serving stopped before the broker was done.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The connection broke, or carried a frame that can't be read.
    #[error("the broker connection: {0}")]
    Frame(#[from] hatchling_core::Error),
    /// A reply couldn't be encoded.
    #[error("a reply couldn't be encoded: {0}")]
    Encode(#[from] serde_json::Error),
}

/// Serves `session` over `connection` until the broker closes it or sends
/// [`BrowserCommand::Close`]; the browser is closed either way.
///
/// # Errors
///
/// [`ServeError`], when the connection breaks, carries a frame that can't
/// be read, or a reply can't be encoded. A command that isn't understood
/// isn't an error: it's answered [`BrowserFailure::BadCommand`].
pub fn serve(connection: &UnixStream, mut session: Session) -> Result<(), ServeError> {
    loop {
        let Some(frame) = read_frame_or_eof(connection)? else {
            session.close();
            return Ok(());
        };
        let command = serde_json::from_slice::<BrowserCommand>(&frame).ok();
        let last = command == Some(BrowserCommand::Close);
        let reply = command.map_or(
            BrowserReply::Failed {
                failure: BrowserFailure::BadCommand,
            },
            |command| session.handle(command),
        );
        let bytes = serde_json::to_vec(&reply)?;
        write_frame(connection, &bytes)?;
        if last {
            return Ok(());
        }
    }
}
