//! A cell's network: its own loopback, and the one listener on
//! it that the owner serves.
//!
//! A new network namespace holds a loopback interface, down, and nothing
//! else. A harness cell brings it up while it still holds its namespace's
//! capabilities, opens a TCP listener on `127.0.0.1`, and hands the
//! listener out over a unix socket ([`send_fd`]). The cell keeps no copy:
//! the owner accepts the connections, so the only thing the cell can reach
//! is what the owner chooses to answer.

use std::io::{IoSlice, IoSliceMut};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::os::fd::{BorrowedFd, OwnedFd};

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};

use crate::Error;

/// The loopback interface's name.
const LOOPBACK: &[u8] = b"lo";

/// `IFF_UP` (linux/if.h).
const IFF_UP: libc::c_short = 1;

const fn net_error(operation: &'static str, source: std::io::Error) -> Error {
    Error::Net { operation, source }
}

/// Brings the loopback interface of the caller's network namespace up.
///
/// Needs `CAP_NET_ADMIN` over that namespace: in a cell, before its
/// capabilities are dropped.
///
/// # Errors
///
/// Returns [`Error::Net`] if the interface cannot be read or set.
pub fn loopback_up() -> Result<(), Error> {
    // SAFETY: a plain datagram socket, used only for the interface ioctl
    // below and closed when `socket` drops. The type and its flag are
    // disjoint bits, added: `|` there would leave `^` an equivalent mutant.
    let raw = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM + libc::SOCK_CLOEXEC, 0) };
    if raw < 0 {
        return Err(net_error("socket", std::io::Error::last_os_error()));
    }
    // SAFETY: `raw` is a socket we just opened and own.
    let socket = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw) };
    // SAFETY: `ifreq` is plain old data; all zeroes is a valid value.
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request.ifr_name.iter_mut().zip(LOOPBACK) {
        *slot = libc::c_char::from_ne_bytes([*byte]);
    }
    let fd = std::os::fd::AsRawFd::as_raw_fd(&socket);
    // Up, and nothing else asked: the kernel keeps the flags userspace
    // cannot change (`IFF_LOOPBACK`) and marks it running itself. A new
    // namespace's loopback has no other flag to keep.
    request.ifr_ifru.ifru_flags = IFF_UP;
    // SAFETY: SIOCSIFFLAGS sets the flags of the interface named in
    // `request`; `request` outlives the call.
    if unsafe { libc::ioctl(fd, libc::SIOCSIFFLAGS, std::ptr::addr_of_mut!(request)) } < 0 {
        return Err(net_error(
            "bring loopback up",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

/// A TCP listener on `127.0.0.1:port` in the caller's network namespace,
/// close-on-exec.
///
/// # Errors
///
/// Returns [`Error::Net`] if it cannot be bound.
pub fn loopback_listener(port: u16) -> Result<OwnedFd, Error> {
    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .map(OwnedFd::from)
        .map_err(|source| net_error("listen on loopback", source))
}

/// Sends `fd` over the connected unix stream socket `socket`, with one
/// byte of data so the receiver can tell it from an end of stream.
///
/// # Errors
///
/// Returns [`Error::Net`] if the send fails.
pub fn send_fd(socket: BorrowedFd<'_>, fd: BorrowedFd<'_>) -> Result<(), Error> {
    let fds = [fd];
    let mut space = [0u8; rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(net_error(
            "send fd",
            std::io::Error::other("no room for the fd"),
        ));
    }
    sendmsg(
        socket,
        &[IoSlice::new(b"F")],
        &mut control,
        SendFlags::NOSIGNAL,
    )
    .map(|_| ())
    .map_err(|errno| net_error("send fd", errno.into()))
}

/// Receives one fd sent by [`send_fd`] on `socket`, close-on-exec.
///
/// # Errors
///
/// Returns [`Error::Net`] if the receive fails or carries no fd.
pub fn recv_fd(socket: BorrowedFd<'_>) -> Result<OwnedFd, Error> {
    let mut byte = [0u8; 1];
    let mut space = [0u8; rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let received = recvmsg(
        socket,
        &mut [IoSliceMut::new(&mut byte)],
        &mut control,
        RecvFlags::CMSG_CLOEXEC,
    )
    .map_err(|errno| net_error("receive fd", errno.into()))?;
    let mut fds = control.drain().filter_map(|message| match message {
        RecvAncillaryMessage::ScmRights(fds) => Some(fds),
        RecvAncillaryMessage::ScmCredentials(_) | _ => None,
    });
    let fd = fds.next().and_then(|mut fds| fds.next());
    match (received.bytes, fd) {
        (1, Some(fd)) => Ok(fd),
        _ => Err(net_error(
            "receive fd",
            std::io::Error::other("no fd was sent"),
        )),
    }
}

/// Whether `fd` is a listening socket: what [`recv_fd`]'s caller checks
/// before serving it.
///
/// # Errors
///
/// Returns [`Error::Net`] if the socket option cannot be read.
pub fn is_listening(fd: BorrowedFd<'_>) -> Result<bool, Error> {
    rustix::net::sockopt::get_socket_acceptconn(fd)
        .map_err(|errno| net_error("read SO_ACCEPTCONN", errno.into()))
}

/// Takes ownership of the Unix stream socket this process inherited at
/// `fd`.
///
/// That is the one descriptor a cell's program is given on purpose
/// ([`keep_across_exec`](crate::keep_across_exec)). It must be an
/// `AF_UNIX` stream socket above stderr. It is marked close-on-exec
/// again, so nothing the program starts inherits it.
///
/// Call it once for a descriptor: the stream it returns owns it.
///
/// # Errors
///
/// [`Error::Net`] if `fd` is stdio, isn't open, or isn't a Unix stream
/// socket; the descriptor is left as it was.
pub fn inherited_unix_stream(fd: i32) -> Result<std::os::unix::net::UnixStream, Error> {
    let refused = |why: &str| {
        net_error(
            "adopt the inherited connection",
            std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_owned()),
        )
    };
    if fd <= 2 {
        return Err(refused("stdio is not a connection"));
    }
    if crate::fd_flags(fd).is_none() {
        return Err(refused("the descriptor is not open"));
    }
    // SAFETY: `fd` is open (checked above), and the borrow ends before
    // anything closes it.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let stream = rustix::net::sockopt::get_socket_type(borrowed)
        .is_ok_and(|kind| kind == rustix::net::SocketType::STREAM);
    // SO_DOMAIN, not getsockname: the cell's filter allows getsockopt.
    let unix = rustix::net::sockopt::get_socket_domain(borrowed)
        .is_ok_and(|domain| domain == rustix::net::AddressFamily::UNIX);
    if !(stream && unix) {
        return Err(refused("the descriptor is not a Unix stream socket"));
    }
    rustix::io::fcntl_setfd(borrowed, rustix::io::FdFlags::CLOEXEC)
        .map_err(|errno| net_error("mark the connection close-on-exec", errno.into()))?;
    // SAFETY: `fd` is an open Unix stream socket this process inherited,
    // and the caller hands its ownership over: nothing else here owns it.
    let owned = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    Ok(std::os::unix::net::UnixStream::from(owned))
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpStream;
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn only_an_inherited_unix_stream_is_adopted_and_it_closes_on_exec() {
        use std::os::fd::IntoRawFd as _;
        let (ours, theirs) = UnixStream::pair().unwrap();
        let fd = theirs.into_raw_fd();
        // SAFETY: the pair's end was just released to this raw fd, open.
        crate::keep_across_exec(unsafe { BorrowedFd::borrow_raw(fd) }).unwrap();
        let mut adopted = inherited_unix_stream(fd).unwrap();
        assert_eq!(crate::fd_flags(fd), Some(libc::FD_CLOEXEC));
        (&ours).write_all(b"hi").unwrap();
        let mut got = [0u8; 2];
        adopted.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hi");
        // Not a connection: stdio, a closed descriptor, a datagram
        // socket, a TCP listener.
        let refused = |fd: i32| {
            matches!(
                inherited_unix_stream(fd),
                Err(Error::Net {
                    operation: "adopt the inherited connection",
                    ..
                })
            )
        };
        assert!(refused(0) && refused(2));
        assert!(refused(i32::MAX - 1));
        let (datagram, _other) = std::os::unix::net::UnixDatagram::pair().unwrap();
        assert!(refused(std::os::fd::AsRawFd::as_raw_fd(&datagram)));
        let tcp = loopback_listener(0).unwrap();
        assert!(refused(std::os::fd::AsRawFd::as_raw_fd(&tcp)));
    }

    #[test]
    fn a_listener_passes_over_a_unix_socket_and_serves_from_the_other_side() {
        let listener = loopback_listener(0).unwrap();
        let port = TcpListener::from(listener.try_clone().unwrap())
            .local_addr()
            .unwrap()
            .port();
        let (ours, theirs) = UnixStream::pair().unwrap();
        // Bounded: a send that never happened fails the receive, not the
        // suite.
        theirs
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        send_fd(ours.as_fd(), listener.as_fd()).unwrap();
        drop(listener);
        let received = recv_fd(theirs.as_fd()).unwrap();
        assert!(is_listening(received.as_fd()).unwrap());
        assert!(!is_listening(theirs.as_fd()).unwrap());
        let flags = rustix::io::fcntl_getfd(&received).unwrap();
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        let server = TcpListener::from(received);
        let mut client = TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();
        let (mut accepted, _) = server.accept().unwrap();
        client.write_all(b"hello").unwrap();
        let mut got = [0u8; 5];
        accepted.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hello");
    }

    #[test]
    fn a_socket_that_cannot_be_opened_is_reported_as_such() {
        // In a forked child allowed no open files, the interface socket
        // itself fails (EMFILE): that failure is reported, nothing goes on.
        use crate::process::{ForkResult, child_exit_code, exit_child, fork};
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let none = rustix::process::Rlimit {
                    current: Some(0),
                    maximum: Some(0),
                };
                if rustix::process::setrlimit(rustix::process::Resource::Nofile, none).is_err() {
                    exit_child(2);
                }
                // A panic here must fail the test: unwinding out of a forked
                // child's only thread would end it with status 0.
                let reported = std::panic::catch_unwind(|| {
                    matches!(
                        loopback_up(),
                        Err(Error::Net { operation: "socket", source })
                            if source.raw_os_error() == Some(24)
                    )
                });
                exit_child(match reported {
                    Ok(true) => 0,
                    Ok(false) => 1,
                    Err(_) => 3,
                });
            }
        };
        let status = child
            .wait_deadline(std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(child_exit_code(status), 0);
    }

    #[test]
    fn a_network_failure_names_its_operation_and_keeps_its_cause() {
        let error = net_error("send fd", std::io::Error::from_raw_os_error(32));
        assert!(matches!(
            &error,
            Error::Net { operation: "send fd", source } if source.raw_os_error() == Some(32)
        ));
    }

    #[test]
    fn a_byte_without_an_fd_or_an_end_of_stream_is_no_fd() {
        let (mut ours, theirs) = UnixStream::pair().unwrap();
        ours.write_all(b"F").unwrap();
        assert!(matches!(
            recv_fd(theirs.as_fd()),
            Err(Error::Net {
                operation: "receive fd",
                ..
            })
        ));
        // Shut down, not dropped: another test's fork can hold a dropped
        // end open, and the receive would wait for it.
        ours.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(recv_fd(theirs.as_fd()).is_err());
    }

    #[test]
    fn a_new_namespaces_loopback_comes_up_and_carries_the_relay() {
        use std::time::Duration;

        use crate::process::{ForkResult, child_exit_code, exit_child, fork};
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                if crate::unshare_user().is_err() || crate::unshare_net().is_err() {
                    exit_child(99);
                }
                // Down, a new namespace's loopback carries nothing.
                let early = loopback_listener(0)
                    .ok()
                    .and_then(|fd| TcpListener::from(fd).local_addr().ok())
                    .map(|addr| TcpStream::connect(addr).is_ok());
                if early == Some(true) {
                    exit_child(1);
                }
                if loopback_up().is_err() {
                    exit_child(2);
                }
                let Ok(listener) = loopback_listener(0).map(TcpListener::from) else {
                    exit_child(3);
                };
                let reached = listener
                    .local_addr()
                    .ok()
                    .and_then(|addr| TcpStream::connect(addr).ok())
                    .and_then(|_| listener.accept().ok());
                exit_child(if reached.is_some() { 0 } else { 4 });
            }
        };
        let code = child_exit_code(
            child
                .wait_deadline(Duration::from_secs(30))
                .expect("bounded wait"),
        );
        if code == 99 {
            let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1");
            assert!(
                !required,
                "HATCHLING_REQUIRE_CELL=1 but namespaces are unavailable"
            );
            return;
        }
        assert_eq!(
            code, 0,
            "1: reachable while down; 2: no up; 3: no listener; 4: unreachable"
        );
    }

    #[test]
    fn the_hosts_loopback_is_not_ours_to_change() {
        // Only a namespace's own capabilities may bring its loopback up:
        // outside one, an unprivileged caller is refused.
        match loopback_up() {
            Ok(()) => assert_eq!(rustix::process::geteuid().as_raw(), 0, "only root"),
            Err(error) => assert!(
                matches!(
                    error,
                    Error::Net {
                        operation: "bring loopback up",
                        ..
                    }
                ),
                "{error}"
            ),
        }
    }

    #[test]
    fn a_listener_needs_a_free_port_and_a_send_needs_a_peer() {
        let taken = TcpListener::from(loopback_listener(0).unwrap());
        let port = taken.local_addr().unwrap().port();
        assert!(matches!(
            loopback_listener(port),
            Err(Error::Net {
                operation: "listen on loopback",
                ..
            })
        ));
        // Shut down, not dropped: another test's fork can hold a dropped
        // peer open for a moment, and the send would land.
        let (ours, _theirs) = UnixStream::pair().unwrap();
        ours.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(matches!(
            send_fd(ours.as_fd(), taken.as_fd()),
            Err(Error::Net {
                operation: "send fd",
                ..
            })
        ));
    }
}
