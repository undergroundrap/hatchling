//! Model usage, counted by the broker.
//!
//! Every model call a cell makes goes out through the broker's egress,
//! and its answer comes back the same way. For each model request the
//! server answered with success (a `POST` to [`COUNTED_ENDPOINTS`]), the
//! broker reads the token counts the server reported in its reply, and
//! adds a line to the day's file in the usage directory
//! (`<schedule store>.usage`): when, which model, the tokens it read and
//! the tokens it wrote. Never the request, the reply, or which cell asked.
//!
//! - **The counts are the server's.** The agent never sees a reply
//!   before the broker has read it, so it can't change what's counted.
//!   A reply that reports no usage is counted as a call, as unreported.
//! - **One file per day,** `YYYY-MM-DD.jsonl` by UTC, written a line at
//!   a time. When a day's first line is written, files older than
//!   [`KEEP_DAYS`] are removed.
//! - **Reported by this machine's days:** the last [`REPORT_DAYS`], today
//!   first, as the owner's app shows them.

use std::collections::BTreeMap;
use std::fs::{DirBuilder, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_core::ui::{UsageDay, UsageModel, UsageReport};
use hatchling_egress::{EgressRequest, EgressResponse};
use hatchling_scheduler::recent_dates;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The model endpoints whose replies carry usage, as the end of their
/// path: chat completions, `OpenAI`'s responses, Anthropic's messages.
pub const COUNTED_ENDPOINTS: [&str; 3] = ["/chat/completions", "/responses", "/messages"];

/// How many days of usage are kept.
pub const KEEP_DAYS: usize = 40;

/// How many days a report covers, today included.
pub const REPORT_DAYS: usize = 7;

/// The most of a model's name kept, in characters: the server chose it.
pub const MAX_MODEL_CHARS: usize = 100;

/// The largest day's file read: past it, that day isn't reported.
const MAX_FILE_BYTES: u64 = 64 << 20;

/// What a reply that names no model is counted under.
const UNNAMED: &str = "unnamed model";

/// Serializes the broker's writers: its sessions are threads of one
/// process.
static WRITES: Mutex<()> = Mutex::new(());

/// One model call, as kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Call {
    /// When it was answered, Unix seconds.
    at: i64,
    /// The model, as its server named it.
    model: String,
    /// Tokens read, if the server said.
    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    input: Option<u64>,
    /// Tokens written, if the server said.
    #[serde(rename = "out", default, skip_serializing_if = "Option::is_none")]
    output: Option<u64>,
}

/// Counts `request`'s answer `response` in the usage directory `dir`.
///
/// Only a model call answered with success counts, at `at` (Unix seconds),
/// and nothing does when `dir` is empty. A count that can't be written is
/// logged, and the answer goes on to the cell all the same.
pub fn count(dir: &Path, request: &EgressRequest, response: &EgressResponse, at: i64) {
    if dir.as_os_str().is_empty() || !(200..=299).contains(&response.status) {
        return;
    }
    let Some(call) = read_reply(&request.method, &request.path, &response.body, at) else {
        return;
    };
    if let Err(error) = append(dir, &call) {
        tracing::warn!(error = %error, "a model call's usage couldn't be counted");
    }
}

/// The call a reply to `method` `path` reports, at `at`; `None` when it
/// isn't a model call.
fn read_reply(method: &str, path: &str, body: &[u8], at: i64) -> Option<Call> {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    if method != "POST" || !COUNTED_ENDPOINTS.iter().any(|end| path.ends_with(end)) {
        return None;
    }
    let reply: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let model = reply
        .get("model")
        .and_then(Value::as_str)
        .map(named)
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| String::from(UNNAMED));
    let usage = reply.get("usage");
    let tokens = |keys: &[&str]| -> Option<u64> {
        keys.iter()
            .filter_map(|key| usage?.get(key)?.as_u64())
            .reduce(u64::saturating_add)
    };
    Some(Call {
        at,
        model,
        // Anthropic counts cached prompt tokens apart; the others count
        // them in.
        input: tokens(&[
            "prompt_tokens",
            "input_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ]),
        output: tokens(&["completion_tokens", "output_tokens"]),
    })
}

/// `model`, bounded, without control characters.
fn named(model: &str) -> String {
    model
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_MODEL_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Adds `call` to its day's file in `dir`, and on a day's first line,
/// removes the days past keeping.
fn append(dir: &Path, call: &Call) -> std::io::Result<()> {
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let Some(day) = recent_dates(call.at, Some("UTC"), 1).pop() else {
        return Err(std::io::Error::other("the call's time is out of range"));
    };
    let file = dir.join(format!("{day}.jsonl"));
    let first = !file.exists();
    let mut line = serde_json::to_string(call)?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&file)?
        .write_all(line.as_bytes())?;
    if first {
        prune(dir, call.at);
    }
    Ok(())
}

/// Removes the day files in `dir` older than [`KEEP_DAYS`] before `at`.
fn prune(dir: &Path, at: i64) {
    let Some(oldest) = recent_dates(at, Some("UTC"), KEEP_DAYS).pop() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(day) = name.to_str().and_then(|name| name.strip_suffix(".jsonl")) else {
            continue;
        };
        if is_date(day) && day < oldest.as_str() {
            if let Err(error) = std::fs::remove_file(entry.path()) {
                tracing::warn!(day, error = %error, "an old day's usage couldn't be removed");
            }
        }
    }
}

/// Whether `day` is `YYYY-MM-DD`.
fn is_date(day: &str) -> bool {
    day.len() == 10
        && day.char_indices().all(|(at, c)| {
            if at == 4 || at == 7 {
                c == '-'
            } else {
                c.is_ascii_digit()
            }
        })
}

/// The usage counted in `dir` over the [`REPORT_DAYS`] up to `now` (Unix
/// seconds), by the days of `tz` (this machine's when `None`).
#[must_use]
pub fn report(dir: &Path, now: i64, tz: Option<&str>) -> UsageReport {
    let counted = !dir.as_os_str().is_empty();
    let mut days: Vec<UsageDay> = recent_dates(now, tz, REPORT_DAYS)
        .into_iter()
        .map(|date| UsageDay {
            date,
            calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            unreported: 0,
        })
        .collect();
    let mut models: BTreeMap<String, UsageModel> = BTreeMap::new();
    // A day on any clock lies within a day of the same UTC date.
    let files = if counted {
        recent_dates(now, Some("UTC"), REPORT_DAYS + 2)
    } else {
        Vec::new()
    };
    for call in files.iter().flat_map(|file| calls(dir, file)) {
        let Some(date) = recent_dates(call.at, tz, 1).pop() else {
            continue;
        };
        let Some(day) = days.iter_mut().find(|day| day.date == date) else {
            continue;
        };
        let (input, output) = (call.input.unwrap_or(0), call.output.unwrap_or(0));
        day.calls = day.calls.saturating_add(1);
        day.input_tokens = day.input_tokens.saturating_add(input);
        day.output_tokens = day.output_tokens.saturating_add(output);
        if call.input.is_none() && call.output.is_none() {
            day.unreported = day.unreported.saturating_add(1);
        }
        let model = models
            .entry(call.model.clone())
            .or_insert_with(|| UsageModel {
                model: call.model,
                calls: 0,
                input_tokens: 0,
                output_tokens: 0,
            });
        model.calls = model.calls.saturating_add(1);
        model.input_tokens = model.input_tokens.saturating_add(input);
        model.output_tokens = model.output_tokens.saturating_add(output);
    }
    let mut models: Vec<UsageModel> = models.into_values().collect();
    models.sort_by_key(|model| {
        std::cmp::Reverse(model.input_tokens.saturating_add(model.output_tokens))
    });
    UsageReport {
        counted,
        days,
        models,
    }
}

/// The calls kept in `dir` for the UTC day `day`: none when there's no
/// file, or it's too large to read; a line that doesn't read is skipped.
fn calls(dir: &Path, day: &str) -> Vec<Call> {
    let path = dir.join(format!("{day}.jsonl"));
    let Ok(file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    if !file
        .metadata()
        .is_ok_and(|meta| meta.len() <= MAX_FILE_BYTES)
    {
        tracing::warn!(day, "a day's usage is too large to read");
        return Vec::new();
    }
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-03 14:13:20 UTC.
    const NOW: i64 = 1_793_715_200;
    const DAY: i64 = 86_400;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-usage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn request(path: &str) -> EgressRequest {
        EgressRequest {
            method: String::from("POST"),
            destination: String::from("localhost:11434"),
            path: path.to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
            surrogate_b64: String::new(),
        }
    }

    fn reply(status: u16, body: &str) -> EgressResponse {
        EgressResponse {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    const OLLAMA: &str = r#"{"model":"qwen3.5:9b","usage":{"prompt_tokens":1200,"completion_tokens":300,"total_tokens":1500}}"#;

    #[test]
    fn each_vendors_counts_are_read() {
        let read = |body: &str| read_reply("POST", "/v1/chat/completions", body.as_bytes(), NOW);
        let call = read(OLLAMA).unwrap();
        assert_eq!(
            (call.model.as_str(), call.input, call.output, call.at),
            ("qwen3.5:9b", Some(1200), Some(300), NOW)
        );
        let anthropic = read(
            r#"{"model":"claude","usage":{"input_tokens":10,"cache_read_input_tokens":90,"cache_creation_input_tokens":5,"output_tokens":7}}"#,
        )
        .unwrap();
        assert_eq!((anthropic.input, anthropic.output), (Some(105), Some(7)));
        let silent = read(r#"{"model":"m"}"#).unwrap();
        assert_eq!((silent.input, silent.output), (None, None));
        let half = read(r#"{"model":"m","usage":{"completion_tokens":4}}"#).unwrap();
        assert_eq!((half.input, half.output), (None, Some(4)));
        let huge = read(&format!(
            r#"{{"model":"m","usage":{{"input_tokens":{max},"cache_read_input_tokens":{max}}}}}"#,
            max = u64::MAX
        ))
        .unwrap();
        assert_eq!(huge.input, Some(u64::MAX));
    }

    #[test]
    fn only_a_model_call_is_a_call() {
        for (method, path) in [
            ("POST", "/v1/chat/completions?x=1"),
            ("POST", "/v1/responses"),
            ("POST", "/v1/messages"),
        ] {
            assert!(
                read_reply(method, path, OLLAMA.as_bytes(), NOW).is_some(),
                "{path}"
            );
        }
        for (method, path) in [
            ("GET", "/v1/chat/completions"),
            ("POST", "/v1/messages/count_tokens"),
            ("POST", "/v1/embeddings"),
            ("POST", "/v1/models"),
        ] {
            assert_eq!(
                read_reply(method, path, OLLAMA.as_bytes(), NOW),
                None,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn a_models_name_is_the_servers_bounded() {
        let read =
            |body: &str| read_reply("POST", "/chat/completions", body.as_bytes(), NOW).unwrap();
        assert_eq!(read("not json").model, UNNAMED);
        assert_eq!(read(r#"{"model":42}"#).model, UNNAMED);
        assert_eq!(read(r#"{"model":"  \u0007 "}"#).model, UNNAMED);
        assert_eq!(read(r#"{"model":"a\nb"}"#).model, "ab");
        let long = format!(r#"{{"model":"{}"}}"#, "m".repeat(500));
        assert_eq!(read(&long).model.chars().count(), MAX_MODEL_CHARS);
    }

    #[test]
    fn calls_are_counted_by_day_and_model() {
        let dir = scratch("count");
        let path = "/v1/chat/completions";
        count(&dir, &request(path), &reply(200, OLLAMA), NOW);
        count(&dir, &request(path), &reply(200, OLLAMA), NOW + 60);
        count(
            &dir,
            &request(path),
            &reply(200, r#"{"model":"gemma4:12b"}"#),
            NOW - DAY,
        );
        count(
            &dir,
            &request(path),
            &reply(
                200,
                r#"{"model":"gemma4:12b","usage":{"prompt_tokens":5000,"completion_tokens":1}}"#,
            ),
            NOW - 2 * DAY,
        );
        // Not counted: a failure, another endpoint, a call too old.
        count(&dir, &request(path), &reply(500, OLLAMA), NOW);
        count(&dir, &request(path), &reply(199, OLLAMA), NOW);
        count(&dir, &request("/v1/models"), &reply(200, OLLAMA), NOW);
        count(&dir, &request(path), &reply(200, OLLAMA), NOW - 8 * DAY);

        let usage = report(&dir, NOW, Some("UTC"));
        assert!(usage.counted);
        let dates: Vec<&str> = usage.days.iter().map(|day| day.date.as_str()).collect();
        assert_eq!(
            dates,
            [
                "2026-11-03",
                "2026-11-02",
                "2026-11-01",
                "2026-10-31",
                "2026-10-30",
                "2026-10-29",
                "2026-10-28"
            ]
        );
        let today = usage.days.first().unwrap();
        assert_eq!(
            (
                today.calls,
                today.input_tokens,
                today.output_tokens,
                today.unreported
            ),
            (2, 2400, 600, 0)
        );
        let yesterday = usage.days.get(1).unwrap();
        assert_eq!(
            (
                yesterday.calls,
                yesterday.input_tokens,
                yesterday.unreported
            ),
            (1, 0, 1)
        );
        assert_eq!(usage.days.get(2).unwrap().input_tokens, 5000);
        assert_eq!(usage.days.get(3).unwrap().calls, 0);
        let models: Vec<(&str, u32, u64, u64)> = usage
            .models
            .iter()
            .map(|model| {
                (
                    model.model.as_str(),
                    model.calls,
                    model.input_tokens,
                    model.output_tokens,
                )
            })
            .collect();
        assert_eq!(
            models,
            [("gemma4:12b", 2, 5000, 1), ("qwen3.5:9b", 2, 2400, 600)]
        );

        // On a clock where it's still the 2nd, the day before's calls are
        // the 2nd's too.
        let west = report(&dir, NOW - 15 * 3600, Some("America/Los_Angeles"));
        assert_eq!(west.days.first().unwrap().date, "2026-11-02");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_counted_without_a_place() {
        let none = Path::new("");
        count(
            none,
            &request("/chat/completions"),
            &reply(200, OLLAMA),
            NOW,
        );
        let usage = report(none, NOW, Some("UTC"));
        assert!(!usage.counted);
        assert_eq!(usage.days.len(), REPORT_DAYS);
        assert!(usage.days.iter().all(|day| day.calls == 0));
        assert!(usage.models.is_empty());
    }

    #[test]
    fn a_count_that_cant_be_written_leaves_nothing() {
        let dir = scratch("unwritable");
        std::fs::write(&dir, "a file, not a directory").unwrap();
        count(
            &dir,
            &request("/chat/completions"),
            &reply(200, OLLAMA),
            NOW,
        );
        assert!(
            append(
                &dir.join("usage"),
                &read_reply("POST", "/chat/completions", OLLAMA.as_bytes(), NOW).unwrap()
            )
            .is_err()
        );
        let out_of_range = Call {
            at: i64::MAX,
            model: String::from("m"),
            input: None,
            output: None,
        };
        let fine = scratch("range");
        assert!(append(&fine, &out_of_range).is_err());
        let _ = std::fs::remove_file(&dir);
        let _ = std::fs::remove_dir_all(&fine);
    }

    #[test]
    fn files_and_lines_that_dont_read_are_skipped() {
        let dir = scratch("unreadable");
        count(
            &dir,
            &request("/chat/completions"),
            &reply(200, OLLAMA),
            NOW,
        );
        let file = dir.join("2026-11-03.jsonl");
        let mut text = std::fs::read_to_string(&file).unwrap();
        text.push_str("not a call\n{\"at\":1}\n");
        std::fs::write(&file, text).unwrap();
        assert_eq!(
            report(&dir, NOW, Some("UTC")).days.first().unwrap().calls,
            1
        );
        // Too large to read: the day isn't reported.
        let big = OpenOptions::new().append(true).open(&file).unwrap();
        big.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert_eq!(
            report(&dir, NOW, Some("UTC")).days.first().unwrap().calls,
            0
        );
        assert_eq!(calls(&dir, "2026-11-02"), Vec::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_days_first_call_removes_the_days_past_keeping() {
        let dir = scratch("prune");
        std::fs::create_dir_all(&dir).unwrap();
        let oldest_kept = recent_dates(NOW, Some("UTC"), KEEP_DAYS).pop().unwrap();
        let too_old = recent_dates(NOW, Some("UTC"), KEEP_DAYS + 1).pop().unwrap();
        for name in [
            format!("{oldest_kept}.jsonl"),
            format!("{too_old}.jsonl"),
            String::from("2020-01-01.jsonl"),
            String::from("notes.jsonl"),
            String::from("2020-01-01.txt"),
            String::from("2020-1-001.jsonl"),
        ] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        count(
            &dir,
            &request("/chat/completions"),
            &reply(200, OLLAMA),
            NOW,
        );
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut expected = vec![
            String::from("2020-01-01.txt"),
            String::from("2020-1-001.jsonl"),
            format!("{oldest_kept}.jsonl"),
            String::from("2026-11-03.jsonl"),
            String::from("notes.jsonl"),
        ];
        expected.sort();
        assert_eq!(left, expected);
        // A second call that day prunes nothing more.
        std::fs::write(dir.join("2020-01-01.jsonl"), "").unwrap();
        count(
            &dir,
            &request("/chat/completions"),
            &reply(200, OLLAMA),
            NOW,
        );
        assert!(dir.join("2020-01-01.jsonl").exists());
        // Nothing to prune where the call's time is out of range, or with
        // no directory.
        prune(&dir, i64::MAX);
        prune(&dir.join("missing"), NOW);
        assert!(dir.join("2020-01-01.jsonl").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_date_is_yyyy_mm_dd() {
        assert!(is_date("2026-11-03"));
        for not in [
            "2026-11-3",
            "2026-11-033",
            "2026/11/03",
            "20261-1-03",
            "2026-1a-03",
            "abcd-ef-gh",
        ] {
            assert!(!is_date(not), "{not}");
        }
    }

    #[test]
    fn a_counted_file_is_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("modes");
        count(
            &dir,
            &request("/chat/completions"),
            &reply(200, OLLAMA),
            NOW,
        );
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("2026-11-03.jsonl")), 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
