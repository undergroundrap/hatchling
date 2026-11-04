//! The model's context window, as its server says it: what the app's
//! context meter measures each answer against.
//!
//! Ollama says it. `GET /api/ps` lists the models it has loaded, each
//! with the window it gave it (`context_length`): the server's setting
//! (`OLLAMA_CONTEXT_LENGTH`, or its default), which is what it cuts a
//! prompt to, not the most the model could take. A model is listed only
//! while it's loaded, so a run asks when it ends, right after its model
//! calls. A server that isn't Ollama, or has let the model go, says
//! nothing, and the window the operator declared
//! (`HATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW`) stands; one it says is
//! the one used, since it's the one the server keeps to.
//!
//! The question goes from the broker, never a cell, to the endpoint the
//! operator set, and only to one in plain HTTP: the same bounded `GET`
//! with no credential that `hatchling-broker doctor` makes
//! ([`crate::doctor::ask_within`]). Only a number is read from the
//! answer.

use std::time::Duration;

/// How long a run's end waits to connect to the model's server: one
/// that just answered the run's model calls answers at once.
const CONNECT: Duration = Duration::from_secs(1);

/// How long a run's end waits for the server's answer.
const ANSWER: Duration = Duration::from_secs(2);

/// Where Ollama lists the models it has loaded: at its root, whatever
/// path its OpenAI-compatible API has.
pub const LOADED_MODELS: &str = "/api/ps";

/// The window the model's server, at the plain-HTTP `endpoint`, gives
/// the model `name`, when it says.
#[must_use]
pub fn learned(endpoint: &str, name: &str) -> Option<u32> {
    let (authority, _) = crate::doctor::plain_http(endpoint)?;
    window_at(authority, name, CONNECT, ANSWER)
}

/// The window the server at `authority` gives the model `name`, when it
/// says, waiting `connect` to connect and `answer` for its answer.
#[must_use]
pub fn window_at(authority: &str, name: &str, connect: Duration, answer: Duration) -> Option<u32> {
    match crate::doctor::ask_within(authority, LOADED_MODELS, connect, answer) {
        Ok(Some(body)) => window_in(&body, name),
        Ok(None) => None,
        Err(error) => {
            tracing::debug!(%error, "the model's server wasn't asked its window");
            None
        }
    }
}

/// The window Ollama's list of loaded models, `answer`, gives the model
/// `name` (or `name:latest`): `{"models": [{"name", "context_length"}]}`.
#[must_use]
pub fn window_in(answer: &[u8], name: &str) -> Option<u32> {
    let list: serde_json::Value = serde_json::from_slice(answer).ok()?;
    let latest = format!("{name}:latest");
    let named = |model: &serde_json::Value, key: &str| {
        model
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id == name || id == latest)
    };
    let model = list
        .get("models")?
        .as_array()?
        .iter()
        .find(|model| named(model, "name") || named(model, "model"))?;
    let window = model.get("context_length")?.as_u64()?;
    u32::try_from(window).ok().filter(|window| *window > 0)
}

/// A server at a free local port that answers each of `answers`, one
/// request each, in order, and the head of each request it got.
///
/// It waits at most 10 seconds for each request: one that never comes
/// ends it, rather than hanging the test.
#[cfg(test)]
pub fn answering(answers: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    listener.set_nonblocking(true).unwrap();
    let served = std::thread::spawn(move || {
        let mut heads = Vec::new();
        for answer in answers {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return heads,
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).is_ok() {
                head.extend_from_slice(&byte);
            }
            stream.write_all(answer.as_bytes()).unwrap();
            heads.push(String::from_utf8(head).unwrap());
        }
        heads
    });
    (address, served)
}

/// Ollama's answer to `GET /api/ps` with `models` loaded.
#[cfg(test)]
pub fn loaded(models: &str) -> String {
    format!("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{{\"models\":[{models}]}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const QWEN: &str = r#"{"name":"qwen3.5:9b","model":"qwen3.5:9b","size":6147423600,"details":{"family":"qwen35"},"context_length":16384,"runner":"llamacpp"}"#;

    /// The model is found by its name or its `latest` tag, under either
    /// key, and only it: another model's window isn't this one's.
    #[test]
    fn the_window_is_the_named_models() {
        let list = format!(
            r#"{{"models":[{{"name":"other","context_length":4096}},{QWEN},{{"model":"llama3.2:latest","context_length":8192}}]}}"#
        );
        let list = list.as_bytes();
        assert_eq!(window_in(list, "qwen3.5:9b"), Some(16384));
        assert_eq!(window_in(list, "llama3.2"), Some(8192));
        assert_eq!(window_in(list, "llama3.2:latest"), Some(8192));
        assert_eq!(window_in(list, "other"), Some(4096));
        assert_eq!(window_in(list, "qwen3.5"), None, "a name is matched whole");
        assert_eq!(window_in(list, "mistral"), None, "not loaded");
    }

    /// Anything but a window the server plainly gives says nothing.
    #[test]
    fn an_answer_without_a_window_says_nothing() {
        for answer in [
            r#"{"models":[]}"#,
            r#"{"models":[{"name":"m"}]}"#,
            r#"{"models":[{"name":"m","context_length":"16384"}]}"#,
            r#"{"models":[{"name":"m","context_length":0}]}"#,
            r#"{"models":[{"name":"m","context_length":-1}]}"#,
            r#"{"models":[{"name":"m","context_length":4294967296}]}"#,
            r#"{"models":{"name":"m","context_length":16384}}"#,
            r#"{"data":[{"id":"m","context_length":16384}]}"#,
            "not json",
        ] {
            assert_eq!(window_in(answer.as_bytes(), "m"), None, "{answer}");
        }
        assert_eq!(
            window_in(br#"{"models":[{"name":"m","context_length":4294967295}]}"#, "m"),
            Some(u32::MAX)
        );
    }

    /// The broker asks the endpoint's server at its root, with no
    /// credential, and reads the window from its answer; a hosted
    /// endpoint, or a server that answers anything but the list, says
    /// nothing.
    #[test]
    fn a_plain_http_server_is_asked_at_its_root() {
        let (address, served) = answering(vec![loaded(QWEN)]);
        assert_eq!(
            learned(&format!("http://{address}/v1"), "qwen3.5:9b"),
            Some(16384)
        );
        let heads = served.join().unwrap();
        assert_eq!(
            heads,
            [format!(
                "GET /api/ps HTTP/1.0\r\nHost: {address}\r\nAccept: application/json\r\n\r\n"
            )]
        );
        let (address, served) = answering(vec![String::from("HTTP/1.0 404 Not Found\r\n\r\n")]);
        assert_eq!(learned(&format!("http://{address}/v1"), "qwen3.5:9b"), None);
        served.join().unwrap();
        assert_eq!(learned("https://api.example/v1", "m"), None);
        assert_eq!(learned("http://user@localhost/v1", "m"), None);
        let refusing = crate::test_port::RefusingPort::new();
        assert_eq!(
            learned(&format!("http://{}/v1", refusing.addr()), "m"),
            None
        );
    }
}
