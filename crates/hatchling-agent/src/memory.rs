//! Agent side of the memory protocol.
//!
//! The framing and message vocabulary live in [`hatchling_core`]; this
//! module is the agent's behavior: validate the scope client-side,
//! send one op per frame, and require a well-formed [`MemoryResult`]
//! in reply. Must only be called on a [`Ready`](crate::Ready) cell —
//! the typestate enforces this.
//!
//! Scope discipline: the agent says `user`, `project`, or `session`.
//! The `project` spelling carries no name — the broker resolves its
//! configured project identity. Anything else is rejected before any
//! I/O (the broker re-validates regardless: the agent's bytes are
//! never trusted).

use std::os::unix::net::UnixStream;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{MemoryOp, MemoryResult, SessionRequest, SessionResponse};

use crate::Error;

/// The scope spellings the agent may send. `Project` carries no name:
/// the broker asserts the project identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryScope {
    /// The user's own memory.
    User,
    /// Project memory (broker-asserted identity).
    Project,
    /// Ephemeral session memory (broker-minted session id).
    Session,
}

impl MemoryScope {
    const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Session => "session",
        }
    }
}

/// Stores one memory item under (`scope`, `id`).
///
/// `retention` is `keep` or `ttl-secs:<n>`: there is no default, the
/// caller names it explicitly.
///
/// # Errors
///
/// Returns [`Error::MemoryDenied`] if the broker holds no grant,
/// [`Error::MemoryFailed`] on a broker-side backend failure,
/// [`Error::MemoryIo`] on transport failure, or
/// [`Error::MemoryProtocol`] on a malformed broker reply.
pub fn store(
    stream: &UnixStream,
    scope: MemoryScope,
    id: &str,
    content: &str,
    tags: &[&str],
    retention: &str,
) -> Result<(), Error> {
    let op = MemoryOp::Write {
        scope: scope.as_str().to_owned(),
        id: id.to_owned(),
        content: content.to_owned(),
        tags: tags.iter().map(|t| (*t).to_owned()).collect(),
        retention: retention.to_owned(),
    };
    match request(stream, &op)? {
        MemoryResult::Stored => Ok(()),
        MemoryResult::Denied => Err(denied(&op)),
        MemoryResult::MemoryError => Err(Error::MemoryFailed),
        // Every other variant is a protocol violation here. Each is
        // named so a future `MemoryResult` variant breaks compilation
        // instead of being silently accepted.
        other @ (MemoryResult::Item { .. }
        | MemoryResult::Items { .. }
        | MemoryResult::NotFound
        | MemoryResult::Removed { .. }
        | MemoryResult::ContextAppended
        | MemoryResult::ContextEntries { .. }) => Err(unexpected_reply(&op, &other)),
    }
}

/// Recalls one memory item by (`scope`, `id`), as its JSON form.
///
/// Returns `Ok(None)` when no item is stored (or it expired —
///
/// deliberately the same signal).
///
/// # Errors
///
/// Returns [`Error::MemoryDenied`] if the broker holds no grant,
/// [`Error::MemoryFailed`] on a broker-side backend failure,
/// [`Error::MemoryIo`] on transport failure, or
/// [`Error::MemoryProtocol`] on a malformed broker reply.
pub fn recall(stream: &UnixStream, scope: MemoryScope, id: &str) -> Result<Option<String>, Error> {
    let op = MemoryOp::Recall {
        scope: scope.as_str().to_owned(),
        id: id.to_owned(),
    };
    match request(stream, &op)? {
        MemoryResult::Item { item_json } => Ok(Some(item_json)),
        MemoryResult::NotFound => Ok(None),
        MemoryResult::Denied => Err(denied(&op)),
        MemoryResult::MemoryError => Err(Error::MemoryFailed),
        other @ (MemoryResult::Stored
        | MemoryResult::Items { .. }
        | MemoryResult::Removed { .. }
        | MemoryResult::ContextAppended
        | MemoryResult::ContextEntries { .. }) => Err(unexpected_reply(&op, &other)),
    }
}

/// Recalls all items carrying `tag` in `scope`, each as JSON, ordered
/// by id. Scoped tag recall is not a bulk export: it cannot cross the
/// scope boundary.
///
/// The broker answers one page per op ; this follows the pages
/// to the end, so the caller gets every item. Each cursor must move
/// forward (ids ascend), so a broker that stalls is a protocol violation,
/// never an endless loop.
///
/// # Errors
///
/// Returns [`Error::MemoryDenied`] if the broker holds no grant,
/// [`Error::MemoryFailed`] on a broker-side backend failure,
/// [`Error::MemoryIo`] on transport failure, or
/// [`Error::MemoryProtocol`] on a malformed broker reply.
pub fn recall_by_tag(
    stream: &UnixStream,
    scope: MemoryScope,
    tag: &str,
) -> Result<Vec<String>, Error> {
    let mut all = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let op = MemoryOp::RecallByTag {
            scope: scope.as_str().to_owned(),
            tag: tag.to_owned(),
            after: after.clone(),
        };
        match request(stream, &op)? {
            MemoryResult::Items { items_json, next } => {
                let advanced = next_cursor(
                    &op,
                    items_json.is_empty(),
                    after.as_deref(),
                    next,
                    |a, b| a < b,
                )?;
                all.extend(items_json);
                let Some(next) = advanced else {
                    return Ok(all);
                };
                after = Some(next);
            }
            MemoryResult::Denied => return Err(denied(&op)),
            MemoryResult::MemoryError => return Err(Error::MemoryFailed),
            other @ (MemoryResult::Stored
            | MemoryResult::Item { .. }
            | MemoryResult::NotFound
            | MemoryResult::Removed { .. }
            | MemoryResult::ContextAppended
            | MemoryResult::ContextEntries { .. }) => return Err(unexpected_reply(&op, &other)),
        }
    }
}

/// Removes one memory item. Idempotent: removing a missing item
/// succeeds. Returns whether an item was actually removed.
///
/// # Errors
///
/// Returns [`Error::MemoryDenied`] if the broker holds no grant,
/// [`Error::MemoryFailed`] on a broker-side backend failure,
/// [`Error::MemoryIo`] on transport failure, or
/// [`Error::MemoryProtocol`] on a malformed broker reply.
pub fn remove(stream: &UnixStream, scope: MemoryScope, id: &str) -> Result<bool, Error> {
    let op = MemoryOp::Remove {
        scope: scope.as_str().to_owned(),
        id: id.to_owned(),
    };
    match request(stream, &op)? {
        MemoryResult::Removed { removed } => Ok(removed),
        MemoryResult::Denied => Err(denied(&op)),
        MemoryResult::MemoryError => Err(Error::MemoryFailed),
        other @ (MemoryResult::Stored
        | MemoryResult::Item { .. }
        | MemoryResult::Items { .. }
        | MemoryResult::NotFound
        | MemoryResult::ContextAppended
        | MemoryResult::ContextEntries { .. }) => Err(unexpected_reply(&op, &other)),
    }
}

/// Appends a note to this session's append-only context log.
///
/// # Errors
///
/// Returns [`Error::MemoryFailed`] on a broker-side backend failure,
/// [`Error::MemoryIo`] on transport failure, or
/// [`Error::MemoryProtocol`] on a malformed broker reply.
pub fn context_append(stream: &UnixStream, text: &str) -> Result<(), Error> {
    let op = MemoryOp::ContextAppend {
        text: text.to_owned(),
    };
    match request(stream, &op)? {
        MemoryResult::ContextAppended => Ok(()),
        MemoryResult::Denied => Err(denied(&op)),
        MemoryResult::MemoryError => Err(Error::MemoryFailed),
        other @ (MemoryResult::Stored
        | MemoryResult::Item { .. }
        | MemoryResult::Items { .. }
        | MemoryResult::NotFound
        | MemoryResult::Removed { .. }
        | MemoryResult::ContextEntries { .. }) => Err(unexpected_reply(&op, &other)),
    }
}

/// Reads this session's context entries, each as JSON, in sequence
/// order.
///
/// Follows the broker's pages to the end of the log ; each
/// cursor must be a sequence number past the last.
///
/// # Errors
///
/// Returns [`Error::MemoryFailed`] on a broker-side backend failure,
/// [`Error::MemoryIo`] on transport failure, or
/// [`Error::MemoryProtocol`] on a malformed broker reply.
pub fn context_read(stream: &UnixStream) -> Result<Vec<String>, Error> {
    let mut all = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let op = MemoryOp::ContextRead {
            after: after.clone(),
        };
        match request(stream, &op)? {
            MemoryResult::ContextEntries { entries_json, next } => {
                let advanced = next_cursor(
                    &op,
                    entries_json.is_empty(),
                    after.as_deref(),
                    next,
                    |a, b| matches!((a.parse::<u64>(), b.parse::<u64>()), (Ok(a), Ok(b)) if a < b),
                )?;
                all.extend(entries_json);
                let Some(next) = advanced else {
                    return Ok(all);
                };
                after = Some(next);
            }
            MemoryResult::Denied => return Err(denied(&op)),
            MemoryResult::MemoryError => return Err(Error::MemoryFailed),
            other @ (MemoryResult::Stored
            | MemoryResult::Item { .. }
            | MemoryResult::Items { .. }
            | MemoryResult::NotFound
            | MemoryResult::Removed { .. }
            | MemoryResult::ContextAppended) => return Err(unexpected_reply(&op, &other)),
        }
    }
}

/// Checks a page's `next` cursor before following it: a page that
/// continues must have carried something, and its cursor must lie past
/// the one this page resumed from (`before(previous, next)`). Anything
/// else would re-request the same page forever.
fn next_cursor(
    op: &MemoryOp,
    page_empty: bool,
    previous: Option<&str>,
    next: Option<String>,
    before: impl Fn(&str, &str) -> bool,
) -> Result<Option<String>, Error> {
    let Some(next) = next else {
        return Ok(None);
    };
    let advanced = !page_empty && previous.is_none_or(|previous| before(previous, &next));
    if !advanced {
        return Err(Error::MemoryProtocol {
            reason: format!(
                "broker's {} cursor {next:?} does not advance past {previous:?}",
                action_name(op)
            ),
        });
    }
    Ok(Some(next))
}

/// The wire action name of an op, for denial errors.
const fn action_name(op: &MemoryOp) -> &'static str {
    match op {
        MemoryOp::Write { .. } => "write",
        MemoryOp::Recall { .. } => "recall",
        MemoryOp::RecallByTag { .. } => "recall-by-tag",
        MemoryOp::Remove { .. } => "remove",
        MemoryOp::ContextAppend { .. } => "context-append",
        MemoryOp::ContextRead { .. } => "context-read",
    }
}

/// The scope string of an op, for denial errors.
fn scope_name(op: &MemoryOp) -> &str {
    match op {
        MemoryOp::Write { scope, .. }
        | MemoryOp::Recall { scope, .. }
        | MemoryOp::RecallByTag { scope, .. }
        | MemoryOp::Remove { scope, .. } => scope,
        MemoryOp::ContextAppend { .. } | MemoryOp::ContextRead { .. } => "session",
    }
}

/// Maps a frame-layer failure onto the memory vocabulary: transport
/// failures are [`MemoryIo`](Error::MemoryIo), everything else is a
/// protocol violation. Used for both directions of a request.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::MemoryIo { source };
    }
    // Refused before sending: a typed per-op error, and the
    // session is untouched.
    if let hatchling_core::Error::FrameTooLarge { len, max } = error {
        return Error::MemoryRequestTooLarge { len, max };
    }
    Error::MemoryProtocol {
        reason: error.to_string(),
    }
}

/// Sends one op and reads the broker's reply.
///
/// The session speaks [`SessionRequest`]/[`SessionResponse`]
/// envelopes. Every op gets exactly one reply. EOF mid-op is a
/// transport failure; a reply that is not a well-formed
/// `SessionResponse` or that carries the wrong kind is a protocol
/// violation. Fail-closed throughout.
fn request(stream: &UnixStream, op: &MemoryOp) -> Result<MemoryResult, Error> {
    let request = SessionRequest::Memory(op.clone());
    let payload = serde_json::to_vec(&request).map_err(|e| Error::MemoryProtocol {
        reason: format!("failed to encode memory op: {e}"),
    })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|e| Error::MemoryProtocol {
            reason: format!("broker reply is not a valid session response: {e}"),
        })?;
    match response {
        SessionResponse::Memory(result) => Ok(result),
        SessionResponse::Vault(_) => Err(Error::MemoryProtocol {
            reason: String::from("broker gave a vault reply to a memory op"),
        }),
        SessionResponse::Egress(_) => Err(Error::MemoryProtocol {
            reason: String::from("broker gave an egress reply to a memory op"),
        }),
        SessionResponse::Channel(_) => Err(Error::MemoryProtocol {
            reason: String::from("broker gave a channel reply to a memory op"),
        }),
        SessionResponse::Skill(_) => Err(Error::MemoryProtocol {
            reason: String::from("broker gave a skill reply to a memory op"),
        }),
        SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_)
        | SessionResponse::Browser(_) => Err(Error::MemoryProtocol {
            reason: String::from("broker gave a goals or query reply to a memory op"),
        }),
    }
}

/// Builds the denial error for an op the broker holds no grant for.
fn denied(op: &MemoryOp) -> Error {
    Error::MemoryDenied {
        action: action_name(op).to_owned(),
        scope: scope_name(op).to_owned(),
    }
}

/// Builds the protocol-violation error for a reply that does not match
/// the op that was sent (e.g. an `Item` answer to a `Write`).
fn unexpected_reply(op: &MemoryOp, result: &MemoryResult) -> Error {
    Error::MemoryProtocol {
        reason: format!(
            "broker gave an unexpected reply to {}: {result:?}",
            action_name(op),
        ),
    }
}

#[cfg(test)]
mod paging_tests {
    use std::time::Duration;

    use super::*;

    /// A fake broker that answers each request with the next scripted
    /// reply, records the requests, and closes when the script ends.
    /// Both ends bounded: a stalled exchange fails instead of hanging.
    fn scripted(
        replies: Vec<MemoryResult>,
    ) -> (UnixStream, std::thread::JoinHandle<Vec<SessionRequest>>) {
        let (agent, broker) = UnixStream::pair().unwrap();
        for end in [&agent, &broker] {
            end.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in replies {
                let Ok(frame) = read_frame(&broker) else {
                    break;
                };
                if let Ok(request) = serde_json::from_slice::<SessionRequest>(&frame) {
                    seen.push(request);
                }
                let payload = serde_json::to_vec(&SessionResponse::Memory(reply)).unwrap();
                if write_frame(&broker, &payload).is_err() {
                    break;
                }
            }
            seen
        });
        (agent, handle)
    }

    fn afters(seen: &[SessionRequest]) -> Vec<Option<String>> {
        seen.iter()
            .filter_map(|request| match request {
                SessionRequest::Memory(
                    MemoryOp::RecallByTag { after, .. } | MemoryOp::ContextRead { after },
                ) => Some(after.clone()),
                SessionRequest::Memory(_)
                | SessionRequest::Vault(_)
                | SessionRequest::Egress(_)
                | SessionRequest::Channel(_)
                | SessionRequest::Skill(_)
                | SessionRequest::Goals(_)
                | SessionRequest::Query(_)
                | SessionRequest::Schedule(_)
                | SessionRequest::Hook(_)
                | SessionRequest::Artifact(_)
                | SessionRequest::Space(_)
                | SessionRequest::Subagent(_)
                | SessionRequest::Web(_)
                | SessionRequest::Browser(_) => None,
            })
            .collect()
    }

    fn items(json: &[&str], next: Option<&str>) -> MemoryResult {
        MemoryResult::Items {
            items_json: json.iter().map(|j| (*j).to_owned()).collect(),
            next: next.map(str::to_owned),
        }
    }

    fn entries(json: &[&str], next: Option<&str>) -> MemoryResult {
        MemoryResult::ContextEntries {
            entries_json: json.iter().map(|j| (*j).to_owned()).collect(),
            next: next.map(str::to_owned),
        }
    }

    #[test]
    fn recall_by_tag_follows_the_cursors_to_the_end() {
        let (agent, broker) = scripted(vec![
            items(&["a", "b"], Some("b")),
            items(&["c"], Some("c")),
            items(&["d"], None),
        ]);
        let all = recall_by_tag(&agent, MemoryScope::User, "t").unwrap();
        assert_eq!(all, vec!["a", "b", "c", "d"]);
        let seen = broker.join().unwrap();
        assert_eq!(
            afters(&seen),
            vec![None, Some(String::from("b")), Some(String::from("c"))]
        );
    }

    #[test]
    fn context_read_follows_the_cursors_to_the_end() {
        let (agent, broker) = scripted(vec![
            entries(&["e0"], Some("0")),
            entries(&["e1", "e2"], None),
        ]);
        assert_eq!(context_read(&agent).unwrap(), vec!["e0", "e1", "e2"]);
        let seen = broker.join().unwrap();
        assert_eq!(afters(&seen), vec![None, Some(String::from("0"))]);
    }

    #[test]
    fn a_stalled_cursor_is_a_protocol_violation_not_a_loop() {
        // The same cursor twice, a cursor that goes backwards, and a
        // "more to come" page that carried nothing: each would loop.
        for script in [
            vec![items(&["a"], Some("a")), items(&["b"], Some("a"))],
            vec![items(&["b"], Some("b")), items(&["a"], Some("a"))],
            vec![items(&[], Some("a"))],
        ] {
            let (agent, broker) = scripted(script);
            let result = recall_by_tag(&agent, MemoryScope::User, "t");
            assert!(
                matches!(&result, Err(Error::MemoryProtocol { reason }) if reason.contains("does not advance")),
                "{result:?}"
            );
            drop(agent);
            let _ = broker.join();
        }
        for script in [
            vec![entries(&["x"], Some("5")), entries(&["y"], Some("5"))],
            vec![entries(&["x"], Some("5")), entries(&["y"], Some("4"))],
            vec![entries(&["x"], Some("5")), entries(&["y"], Some("six"))],
            vec![entries(&[], Some("0"))],
        ] {
            let (agent, broker) = scripted(script);
            let result = context_read(&agent);
            assert!(
                matches!(&result, Err(Error::MemoryProtocol { reason }) if reason.contains("does not advance")),
                "{result:?}"
            );
            drop(agent);
            let _ = broker.join();
        }
    }

    #[test]
    fn a_numeric_cursor_compares_as_a_number() {
        // "10" sorts before "9" as a string; as a sequence it comes after.
        let (agent, broker) = scripted(vec![
            entries(&["x"], Some("9")),
            entries(&["y"], Some("10")),
            entries(&["z"], None),
        ]);
        assert_eq!(context_read(&agent).unwrap(), vec!["x", "y", "z"]);
        let _ = broker.join();
    }

    #[test]
    fn denial_and_backend_failure_mid_walk_surface() {
        let (agent, broker) = scripted(vec![items(&["a"], Some("a")), MemoryResult::MemoryError]);
        assert!(matches!(
            recall_by_tag(&agent, MemoryScope::User, "t"),
            Err(Error::MemoryFailed)
        ));
        let _ = broker.join();
        let (agent, broker) = scripted(vec![MemoryResult::Denied]);
        assert!(matches!(
            context_read(&agent),
            Err(Error::MemoryDenied { .. })
        ));
        let _ = broker.join();
    }
}
