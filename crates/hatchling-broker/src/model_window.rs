//! The model's context window, and how it fits, as its server says:
//! what the app's context meter measures each answer against, and
//! whether the model ran all on the GPU.
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
//! ([`crate::doctor::ask_within`]). Only numbers are read from the
//! answer.
//!
//! The same list says how much of the model is in the GPU's memory
//! (`size_vram`) of all it takes (`size`). What doesn't fit runs from the
//! computer's own memory, on its CPU, more slowly: a window too large for
//! the GPU, or other work holding the GPU's memory, does that. The owner
//! is told, and so is a computer low on memory ([`memory`]).

use std::time::Duration;

/// How long a run's end waits to connect to the model's server: one
/// that just answered the run's model calls answers at once.
const CONNECT: Duration = Duration::from_secs(1);

/// How long a run's end waits for the server's answer.
const ANSWER: Duration = Duration::from_secs(2);

/// Where Ollama lists the models it has loaded: at its root, whatever
/// path its OpenAI-compatible API has.
pub const LOADED_MODELS: &str = "/api/ps";

/// What the model's server says of a model it has loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loaded {
    /// The window it gave the model, when it says one.
    pub window: Option<u32>,
    /// All the model takes, in bytes: its weights and its window's cache.
    pub size: u64,
    /// How much of that is in the GPU's memory.
    pub on_gpu: u64,
}

impl Loaded {
    /// How much of the model runs from the computer's own memory, on its
    /// CPU: slower than the GPU.
    #[must_use]
    pub const fn on_cpu(&self) -> u64 {
        self.size.saturating_sub(self.on_gpu)
    }
}

/// What the model's server, at the plain-HTTP `endpoint`, says of the
/// model `name`, when it's loaded and the server says.
#[must_use]
pub fn learned(endpoint: &str, name: &str) -> Option<Loaded> {
    let (authority, _) = crate::doctor::plain_http(endpoint)?;
    loaded_at(authority, name, CONNECT, ANSWER)
}

/// What the server at `authority` says of the model `name`, loaded,
/// waiting `connect` to connect and `answer` for its answer.
#[must_use]
pub fn loaded_at(
    authority: &str,
    name: &str,
    connect: Duration,
    answer: Duration,
) -> Option<Loaded> {
    match crate::doctor::ask_within(authority, LOADED_MODELS, connect, answer) {
        Ok(Some(body)) => loaded_in(&body, name),
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
    loaded_in(answer, name)?.window
}

/// What Ollama's list of loaded models, `answer`, says of the model `name`.
///
/// The model is found by its name or `name:latest`: its window, and how
/// much of it is on the GPU. A server that doesn't say how much is on the
/// GPU is taken to have it all there: the owner is never told it's slow
/// on a guess.
#[must_use]
pub fn loaded_in(answer: &[u8], name: &str) -> Option<Loaded> {
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
    let window = model
        .get("context_length")
        .and_then(serde_json::Value::as_u64)
        .and_then(|window| u32::try_from(window).ok())
        .filter(|window| *window > 0);
    let size = model
        .get("size")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let on_gpu = model
        .get("size_vram")
        .and_then(serde_json::Value::as_u64)
        .map_or(size, |on_gpu| on_gpu.min(size));
    Some(Loaded {
        window,
        size,
        on_gpu,
    })
}

/// The command that says how a model fits, for the installer.
pub const MODEL_FIT_SUBCOMMAND: &str = "model-fit";

/// How `loaded` fits, in one word for a script.
///
/// `gpu` when it's all in the GPU's memory, `cpu` when some runs on the
/// CPU, `unknown` when the server doesn't say (it isn't Ollama, or the
/// model isn't loaded).
#[must_use]
pub const fn fit_word(loaded: Option<Loaded>) -> &'static str {
    match loaded {
        Some(loaded) if loaded.size > 0 && loaded.on_cpu() == 0 => "gpu",
        Some(loaded) if loaded.size > 0 => "cpu",
        Some(_) | None => "unknown",
    }
}

/// `model-fit <endpoint> <model>`: prints how the model fits, one word
/// ([`fit_word`]). It reads no settings and changes nothing: the
/// installer asks it, after loading the model, which window to keep.
#[must_use]
pub fn model_fit_main(args: &[String]) -> std::process::ExitCode {
    use std::io::Write as _;
    let (Some(endpoint), Some(name)) = (args.first(), args.get(1)) else {
        let _ = writeln!(
            std::io::stderr().lock(),
            "usage: hatchling-broker {MODEL_FIT_SUBCOMMAND} <endpoint> <model>"
        );
        return std::process::ExitCode::from(hatchling_core::cli::EX_USAGE);
    };
    let _ = writeln!(
        std::io::stdout().lock(),
        "{}",
        fit_word(learned(endpoint, name))
    );
    std::process::ExitCode::SUCCESS
}

/// The computer's memory, from `/proc/meminfo`: what's available, and
/// all of it, in bytes.
#[must_use]
pub fn memory() -> Option<(u64, u64)> {
    memory_in(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

/// What `meminfo`, as `/proc/meminfo` has it, says is available, and the
/// whole, in bytes.
#[must_use]
pub fn memory_in(meminfo: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        meminfo.lines().find_map(|line| {
            let kib = line.strip_prefix(name)?.strip_prefix(':')?;
            let kib = kib.trim().strip_suffix("kB")?.trim();
            kib.parse::<u64>().ok()?.checked_mul(1024)
        })
    };
    Some((field("MemAvailable")?, field("MemTotal")?))
}

/// Whether `available` bytes of `total` is low: under a tenth.
///
/// Past that, another program's next need sends the computer to its swap,
/// and every answer slows.
#[must_use]
pub const fn memory_low(available: u64, total: u64) -> bool {
    available.saturating_mul(10) < total
}

/// `bytes` as gigabytes, to a tenth: "6.6 GB".
#[must_use]
pub fn gigabytes(bytes: u64) -> String {
    let tenths = bytes / 100_000_000;
    format!("{}.{} GB", tenths / 10, tenths % 10)
}

/// A server at a free local port that answers each of `answers`, one
/// request each, in order, and the head of each request it got.
///
/// It waits at most 10 seconds for each request: one that never comes
/// ends it, rather than hanging the test.
#[cfg(test)]
pub(crate) fn answering(answers: Vec<String>) -> (String, std::thread::JoinHandle<Vec<String>>) {
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
pub(crate) fn loaded(models: &str) -> String {
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

    /// How much of the model is on the GPU is read with its window, and
    /// a server that doesn't say is taken to have it all there.
    #[test]
    fn the_split_is_read_with_the_window() {
        let list = br#"{"models":[{"name":"m","context_length":131072,"size":10700000000,"size_vram":9800000000}]}"#;
        let loaded = loaded_in(list, "m").unwrap();
        assert_eq!(
            loaded,
            Loaded {
                window: Some(131_072),
                size: 10_700_000_000,
                on_gpu: 9_800_000_000
            }
        );
        assert_eq!(loaded.on_cpu(), 900_000_000);
        let unsaid = loaded_in(br#"{"models":[{"name":"m","size":6600000000}]}"#, "m").unwrap();
        assert_eq!((unsaid.window, unsaid.on_cpu()), (None, 0));
        let over = loaded_in(br#"{"models":[{"name":"m","size":5,"size_vram":9}]}"#, "m").unwrap();
        assert_eq!(over.on_cpu(), 0, "never more on the GPU than all of it");
        assert_eq!(loaded_in(br#"{"models":[]}"#, "m"), None);
    }

    /// A model all on the GPU is `gpu`, one partly on the CPU `cpu`, and
    /// one the server says nothing of `unknown`.
    #[test]
    fn the_fit_is_one_word() {
        let loaded_list = loaded;
        let loaded = |size, on_gpu| {
            Some(Loaded {
                window: Some(65_536),
                size,
                on_gpu,
            })
        };
        assert_eq!(fit_word(loaded(7_700_000_000, 7_700_000_000)), "gpu");
        assert_eq!(fit_word(loaded(10_700_000_000, 9_800_000_000)), "cpu");
        assert_eq!(fit_word(loaded(10_700_000_000, 0)), "cpu", "all on the CPU");
        assert_eq!(fit_word(loaded(0, 0)), "unknown", "no size said");
        assert_eq!(fit_word(None), "unknown");
        let (address, served) = answering(vec![loaded_list(
            r#"{"name":"m","size":100,"size_vram":100,"context_length":65536}"#,
        )]);
        assert_eq!(
            fit_word(learned(&format!("http://{address}/v1"), "m")),
            "gpu"
        );
        served.join().unwrap();
    }

    /// The computer's memory is read from meminfo's own words, in kB.
    #[test]
    fn memory_is_read_from_meminfo() {
        let meminfo = "MemTotal:       32000000 kB\nMemFree:         1000000 kB\nMemAvailable:    3000000 kB\n";
        assert_eq!(memory_in(meminfo), Some((3_072_000_000, 32_768_000_000)));
        assert_eq!(memory_in("MemTotal: 1 kB\n"), None, "no MemAvailable");
        assert_eq!(memory_in("MemTotal: x kB\nMemAvailable: 1 kB\n"), None);
        assert_eq!(memory_in("MemTotalX: 1 kB\nMemAvailable: 1 kB\n"), None);
        assert!(memory_low(99, 1000));
        assert!(!memory_low(100, 1000), "a tenth is not low");
        assert!(!memory_low(u64::MAX, 1000));
        assert!(memory().is_some_and(|(available, total)| available <= total && total > 0));
        assert_eq!(gigabytes(6_600_000_000), "6.6 GB");
        assert_eq!(gigabytes(950_000_000), "0.9 GB");
        assert_eq!(gigabytes(0), "0.0 GB");
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
            window_in(
                br#"{"models":[{"name":"m","context_length":4294967295}]}"#,
                "m"
            ),
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
            Some(Loaded {
                window: Some(16384),
                size: 6_147_423_600,
                on_gpu: 6_147_423_600
            })
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
