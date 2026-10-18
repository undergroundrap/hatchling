//! Typing that isn't shown: a terminal's echo held off while a secret,
//! such as a security key's PIN, is read from it.

use std::os::fd::BorrowedFd;

use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcsetattr};

/// Echo held off on a terminal, and put back as it was when this drops.
///
/// Only the typed characters go unshown: the newline that ends the
/// answer is still echoed (`ECHONL`), so what's written next starts on a
/// line of its own. On anything that isn't a terminal (a pipe, a file)
/// nothing changes, since nothing there is echoed.
#[must_use = "echo comes back as soon as this is dropped"]
pub struct EchoOff<'fd> {
    fd: BorrowedFd<'fd>,
    saved: Option<Termios>,
}

impl<'fd> EchoOff<'fd> {
    /// Turns echo off on `fd`, when it's a terminal.
    pub fn on(fd: BorrowedFd<'fd>) -> Self {
        let saved = tcgetattr(fd).ok().and_then(|saved| {
            let mut quiet = saved.clone();
            quiet.local_modes.remove(LocalModes::ECHO);
            quiet.local_modes.insert(LocalModes::ECHONL);
            tcsetattr(fd, OptionalActions::Now, &quiet)
                .ok()
                .map(|()| saved)
        });
        Self { fd, saved }
    }

    /// Echo off on this process's standard input, when it's a terminal.
    pub fn on_stdin() -> EchoOff<'static> {
        EchoOff::on(rustix::stdio::stdin())
    }

    /// Whether echo was turned off: `fd` is a terminal.
    #[must_use]
    pub const fn held(&self) -> bool {
        self.saved.is_some()
    }
}

impl Drop for EchoOff<'_> {
    fn drop(&mut self) {
        if let Some(saved) = &self.saved {
            let _ = tcsetattr(self.fd, OptionalActions::Now, saved);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    use super::*;

    fn modes(file: &std::fs::File) -> LocalModes {
        tcgetattr(file).unwrap().local_modes
    }

    #[test]
    fn stdin_is_held_exactly_when_it_is_a_terminal() {
        // A test's stdin may or may not be a terminal; either way, echo
        // is held off exactly when it is one.
        let is_terminal = tcgetattr(rustix::stdio::stdin()).is_ok();
        assert_eq!(EchoOff::on_stdin().held(), is_terminal);
    }

    #[test]
    fn echo_is_off_while_held_and_as_it_was_after_on_a_terminal_only() {
        let main = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&main).unwrap();
        unlockpt(&main).unwrap();
        let name = ptsname(&main, Vec::new()).unwrap();
        let terminal = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(name.to_str().unwrap())
            .unwrap();
        let before = modes(&terminal);
        assert!(before.contains(LocalModes::ECHO), "a new terminal echoes");
        {
            let off = EchoOff::on(terminal.as_fd());
            assert!(off.held());
            let held = modes(&terminal);
            assert!(!held.contains(LocalModes::ECHO), "typing isn't shown");
            assert!(held.contains(LocalModes::ECHONL), "the newline still is");
        }
        assert_eq!(modes(&terminal), before, "put back as it was");
        // Not a terminal: nothing is held, and nothing fails.
        let null = std::fs::File::open("/dev/null").unwrap();
        assert!(!EchoOff::on(null.as_fd()).held());
    }
}
