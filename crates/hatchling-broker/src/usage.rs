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
//!
//! ## Limits
//!
//! The owner may set the most tokens in a day and in a month, on this
//! machine's clock ([`LIMITS_FILE`], beside the days). Before the broker
//! forwards a model call, it checks them ([`reached`]): once a limit is
//! reached, every model call is refused until the day or the month turns,
//! or the owner raises it. A call is checked before it goes, and its
//! tokens are known only from its answer, so the call that crosses a limit
//! finishes; the next one is refused. Calls whose server reports no usage
//! count nothing toward a limit.

use std::collections::BTreeMap;
use std::fs::{DirBuilder, OpenOptions};
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use hatchling_core::ui::{UsageDay, UsageLimits, UsageModel, UsageReport, UsageSpent};
use hatchling_egress::{EgressRequest, EgressResponse};
use hatchling_scheduler::{day_and_month_start, recent_dates};
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
/// and nothing is kept when `dir` is empty. A count that can't be written
/// is logged, and the answer goes on to the cell all the same. The tokens
/// the call read and wrote, as its server reported them, when it was a
/// model call answered with success.
pub fn count(
    dir: &Path,
    request: &EgressRequest,
    response: &EgressResponse,
    at: i64,
) -> Option<(Option<u64>, Option<u64>)> {
    if !(200..=299).contains(&response.status) {
        return None;
    }
    let call = read_reply(&request.method, &request.path, &response.body, at)?;
    if !dir.as_os_str().is_empty() {
        if let Err(error) = append(dir, &call) {
            tracing::warn!(error = %error, "a model call's usage couldn't be counted");
        }
    }
    Some((call.input, call.output))
}

/// Whether a request to `method` `path` is a model call: what's counted,
/// and what a limit refuses.
#[must_use]
pub fn is_model_call(method: &str, path: &str) -> bool {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    method == "POST" && COUNTED_ENDPOINTS.iter().any(|end| path.ends_with(end))
}

/// The call a reply to `method` `path` reports, at `at`; `None` when it
/// isn't a model call.
fn read_reply(method: &str, path: &str, body: &[u8], at: i64) -> Option<Call> {
    if !is_model_call(method, path) {
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
        limits: limits(dir),
        spent: spent(dir, now, tz).unwrap_or_default(),
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

/// The file in the usage directory that holds the owner's limits.
pub const LIMITS_FILE: &str = "limits.json";

/// The most a limit may be: far past any month's use, and exact in the
/// app's numbers.
pub const MAX_LIMIT_TOKENS: u64 = 1_000_000_000_000_000;

/// The largest limits file read.
const MAX_LIMITS_BYTES: u64 = 4096;

/// A day, in seconds.
const DAY_SECS: i64 = 86_400;

/// A quarter of an hour, in seconds: every zone's offset from UTC is a
/// whole number of them, so every day starts on one.
const QUARTER_HOUR: i64 = 900;

/// The limits the owner set in `dir`: none when there's no place, no
/// file, or it doesn't read (logged).
#[must_use]
pub fn limits(dir: &Path) -> UsageLimits {
    if dir.as_os_str().is_empty() {
        return UsageLimits::default();
    }
    let file = match std::fs::File::open(dir.join(LIMITS_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return UsageLimits::default();
        }
        Err(error) => {
            tracing::error!(error = %error, "the usage limits can't be read: none is enforced");
            return UsageLimits::default();
        }
    };
    let mut text = Vec::new();
    let read = file.take(MAX_LIMITS_BYTES + 1).read_to_end(&mut text);
    if read.is_ok() && text.is_empty() {
        return UsageLimits::default();
    }
    let whole = u64::try_from(text.len()).is_ok_and(|len| len <= MAX_LIMITS_BYTES);
    match serde_json::from_slice::<UsageLimits>(&text) {
        Ok(limits) if read.is_ok() && whole && valid(&limits) => limits,
        _ => {
            tracing::error!("the usage limits don't read: none is enforced");
            UsageLimits::default()
        }
    }
}

/// Whether each of `limits` is one the broker keeps: at least a token, at
/// most [`MAX_LIMIT_TOKENS`].
#[must_use]
pub fn valid(limits: &UsageLimits) -> bool {
    [limits.daily_tokens, limits.monthly_tokens]
        .into_iter()
        .flatten()
        .all(|limit| (1..=MAX_LIMIT_TOKENS).contains(&limit))
}

/// Why the owner's limits weren't set.
#[derive(Debug)]
pub enum NotSet {
    /// The broker counts no usage: it has no place to.
    Off,
    /// A limit is none the broker keeps ([`valid`]).
    Invalid,
    /// They couldn't be written.
    Io(std::io::Error),
}

/// Sets the owner's limits in `dir` to `limits`, replacing what was set:
/// written whole beside, then put in place, so a reader sees the old or
/// the new.
///
/// # Errors
///
/// [`NotSet`] says why not.
pub fn set_limits(dir: &Path, limits: &UsageLimits) -> Result<(), NotSet> {
    if dir.as_os_str().is_empty() {
        return Err(NotSet::Off);
    }
    if !valid(limits) {
        return Err(NotSet::Invalid);
    }
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    let write = || -> std::io::Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let next = dir.join(format!("{LIMITS_FILE}.next"));
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&next)?
            .write_all(&serde_json::to_vec(limits)?)?;
        std::fs::rename(&next, dir.join(LIMITS_FILE))
    };
    write().map_err(NotSet::Io)
}

/// What's been read of one day's file: which file, how far, and the
/// tokens of its calls by the quarter hour they were answered in.
#[derive(Debug, Default)]
struct Tally {
    inode: u64,
    read: u64,
    quarters: BTreeMap<i64, u64>,
}

/// Each day's file read so far: a day's file only grows, so each check
/// reads only the calls added since the last.
static TALLIES: Mutex<BTreeMap<PathBuf, Tally>> = Mutex::new(BTreeMap::new());

/// The tokens counted in `dir` today and this month, up to `now` (Unix
/// seconds), by the days of `tz` (this machine's when `None`): `None`
/// when there's no place, or `now` is out of range.
#[must_use]
pub fn spent(dir: &Path, now: i64, tz: Option<&str>) -> Option<UsageSpent> {
    if dir.as_os_str().is_empty() {
        return None;
    }
    let start = day_and_month_start(now, tz)?;
    // The month's days by UTC, and a day either side for the zone.
    let days = usize::try_from(now.saturating_sub(start.month).div_euclid(DAY_SECS))
        .ok()?
        .saturating_add(3);
    let files: Vec<PathBuf> = recent_dates(now, Some("UTC"), days)
        .into_iter()
        .map(|day| dir.join(format!("{day}.jsonl")))
        .collect();
    let mut tallies = TALLIES.lock().unwrap_or_else(PoisonError::into_inner);
    tallies.retain(|path, _| path.parent() != Some(dir) || files.contains(path));
    let (day, month) = (
        start.day.div_euclid(QUARTER_HOUR),
        start.month.div_euclid(QUARTER_HOUR),
    );
    let (mut today, mut this_month) = (0_u64, 0_u64);
    for path in files {
        let tally = tallies.entry(path.clone()).or_default();
        for (quarter, tokens) in tally.catch_up(&path).range(month..) {
            this_month = this_month.saturating_add(*tokens);
            if *quarter >= day {
                today = today.saturating_add(*tokens);
            }
        }
    }
    drop(tallies);
    Some(UsageSpent {
        today,
        month: this_month,
        month_of: start.month_of,
    })
}

impl Tally {
    /// Reads the whole lines added to `path` since the last read, starting
    /// over if it's another file or shorter than what was read; the tokens
    /// by quarter hour, as now read.
    fn catch_up(&mut self, path: &Path) -> &BTreeMap<i64, u64> {
        let Ok(mut file) = std::fs::File::open(path) else {
            *self = Self::default();
            return &self.quarters;
        };
        let Ok(meta) = file.metadata() else {
            return &self.quarters;
        };
        if meta.ino() != self.inode || meta.len() < self.read {
            *self = Self {
                inode: meta.ino(),
                ..Self::default()
            };
        }
        if meta.len() == self.read || file.seek(SeekFrom::Start(self.read)).is_err() {
            return &self.quarters;
        }
        let mut lines = BufReader::new(file.take(meta.len() - self.read));
        let mut line = Vec::new();
        // A line still being written is read when it's whole.
        while lines.read_until(b'\n', &mut line).is_ok() && line.last() == Some(&b'\n') {
            self.read = self
                .read
                .saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX));
            if let Ok(call) = serde_json::from_slice::<Call>(&line) {
                let tokens = call
                    .input
                    .unwrap_or(0)
                    .saturating_add(call.output.unwrap_or(0));
                let quarter = self
                    .quarters
                    .entry(call.at.div_euclid(QUARTER_HOUR))
                    .or_insert(0);
                *quarter = quarter.saturating_add(tokens);
            }
            line.clear();
        }
        &self.quarters
    }
}

/// Which limit was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    /// The day's.
    Day,
    /// The month's.
    Month,
}

/// A limit the owner set, reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reached {
    /// Which.
    pub span: Span,
    /// The tokens counted toward it.
    pub spent: u64,
    /// The limit.
    pub limit: u64,
}

impl Reached {
    /// What the owner is told: which limit, how far, when it turns, and
    /// what they can do.
    #[must_use]
    pub fn told(&self) -> String {
        let (which, when, until) = match self.span {
            Span::Day => ("daily", "today", "midnight"),
            Span::Month => ("monthly", "this month", "the 1st"),
        };
        format!(
            "You've reached the {which} limit you set: {} of {} tokens {when}. Hatchling won't call the model again until {until}, unless you raise the limit in Settings, under Usage.",
            grouped(self.spent),
            grouped(self.limit)
        )
    }
}

/// The limit the owner set in `dir` that's reached at `now` (Unix
/// seconds) on this machine's clock, if one is: the month's first, which
/// raising the day's wouldn't help.
#[must_use]
pub fn reached(dir: &Path, now: i64) -> Option<Reached> {
    reached_on(dir, now, None)
}

/// [`reached`] by the days of `tz`.
fn reached_on(dir: &Path, now: i64, tz: Option<&str>) -> Option<Reached> {
    let limits = limits(dir);
    if limits.daily_tokens.is_none() && limits.monthly_tokens.is_none() {
        return None;
    }
    let spent = spent(dir, now, tz)?;
    let over = |span, spent, limit: Option<u64>| {
        limit
            .filter(|limit| spent >= *limit)
            .map(|limit| Reached { span, spent, limit })
    };
    over(Span::Month, spent.month, limits.monthly_tokens)
        .or_else(|| over(Span::Day, spent.today, limits.daily_tokens))
}

/// `n` with its thousands set apart: `1,234,567`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-03 14:13:20 UTC.
    const NOW: i64 = 1_793_715_200;
    const DAY: i64 = 86_400;

    fn scratch(name: &str) -> PathBuf {
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

    /// 2026-11-01 00:00:00 UTC.
    const NOVEMBER: i64 = 1_793_491_200;

    const fn limited(daily: Option<u64>, monthly: Option<u64>) -> UsageLimits {
        UsageLimits {
            daily_tokens: daily,
            monthly_tokens: monthly,
        }
    }

    fn call_at(dir: &Path, at: i64) {
        count(dir, &request("/chat/completions"), &reply(200, OLLAMA), at);
    }

    #[test]
    fn limits_are_set_whole_and_read_back() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("limits");
        assert_eq!(limits(&dir), UsageLimits::default());
        set_limits(&dir, &limited(Some(5000), None)).unwrap();
        assert_eq!(limits(&dir), limited(Some(5000), None));
        set_limits(&dir, &limited(None, Some(MAX_LIMIT_TOKENS))).unwrap();
        assert_eq!(limits(&dir), limited(None, Some(MAX_LIMIT_TOKENS)));
        set_limits(&dir, &UsageLimits::default()).unwrap();
        assert_eq!(limits(&dir), UsageLimits::default());
        assert!(!dir.join(format!("{LIMITS_FILE}.next")).exists());
        let mode = std::fs::metadata(dir.join(LIMITS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_limit_is_a_token_at_least_and_bounded() {
        let dir = scratch("invalid");
        for bad in [
            limited(Some(0), None),
            limited(None, Some(0)),
            limited(Some(MAX_LIMIT_TOKENS + 1), None),
            limited(Some(10), Some(MAX_LIMIT_TOKENS + 1)),
        ] {
            assert!(!valid(&bad), "{bad:?}");
            assert!(matches!(set_limits(&dir, &bad), Err(NotSet::Invalid)));
        }
        assert!(valid(&limited(Some(1), Some(1))));
        assert!(!dir.exists(), "nothing is written for a refused limit");
        assert!(matches!(
            set_limits(Path::new(""), &UsageLimits::default()),
            Err(NotSet::Off)
        ));
        assert_eq!(limits(Path::new("")), UsageLimits::default());
        std::fs::write(&dir, "a file, not a directory").unwrap();
        assert!(matches!(
            set_limits(&dir, &limited(Some(5), None)),
            Err(NotSet::Io(_))
        ));
        let _ = std::fs::remove_file(&dir);
    }

    #[test]
    fn limits_that_dont_read_are_none() {
        let dir = scratch("unread-limits");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(LIMITS_FILE);
        for text in [
            String::new(),
            String::from("not json"),
            String::from(r#"{"daily_tokens":0}"#),
            String::from(r#"{"hourly_tokens":5}"#),
            format!(r#"{{"daily_tokens":5}}{}"#, " ".repeat(5000)),
        ] {
            std::fs::write(&file, &text).unwrap();
            assert_eq!(limits(&dir), UsageLimits::default(), "{text:?}");
        }
        std::fs::write(&file, r#"{"daily_tokens":5}"#).unwrap();
        assert_eq!(limits(&dir), limited(Some(5), None));
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert_eq!(limits(&dir), UsageLimits::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spent_is_today_and_this_month_on_a_zones_clock() {
        let dir = scratch("spent");
        call_at(&dir, NOW);
        call_at(&dir, NOW - DAY);
        // 2026-10-31 22:00 in New York: November by UTC only.
        call_at(&dir, NOVEMBER + 2 * 3600);
        // October everywhere.
        call_at(&dir, NOVEMBER - 6 * 3600);
        assert_eq!(
            spent(&dir, NOW, Some("UTC")),
            Some(UsageSpent {
                today: 1500,
                month: 4500,
                month_of: String::from("2026-11"),
            })
        );
        let east = spent(&dir, NOW, Some("America/New_York")).unwrap();
        assert_eq!((east.today, east.month), (1500, 3000));
        // On the 1st, the day is the month.
        let first = spent(&dir, NOVEMBER + 3 * 3600, Some("UTC")).unwrap();
        assert_eq!((first.today, first.month), (1500, 1500));
        assert_eq!(spent(Path::new(""), NOW, Some("UTC")), None);
        assert_eq!(spent(&dir, i64::MAX, Some("UTC")), None);
        assert_eq!(spent(&dir, NOW, Some("Mars/Olympus")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spent_reads_only_whole_lines_and_starts_over_on_a_new_file() {
        let dir = scratch("tally");
        let today = |dir: &Path| spent(dir, NOW, Some("UTC")).unwrap().today;
        assert_eq!(today(&dir), 0);
        call_at(&dir, NOW);
        assert_eq!(today(&dir), 1500);
        call_at(&dir, NOW + 60);
        assert_eq!(today(&dir), 3000);
        let file = dir.join("2026-11-03.jsonl");
        let mut half = OpenOptions::new().append(true).open(&file).unwrap();
        half.write_all(format!(r#"{{"at":{NOW},"model":"m","in":7"#).as_bytes())
            .unwrap();
        assert_eq!(today(&dir), 3000, "a line still being written waits");
        half.write_all(b"}\nnot a call\n").unwrap();
        assert_eq!(today(&dir), 3007);
        // Another file in its place is read from its start.
        let other = dir.join("other");
        std::fs::write(
            &other,
            format!("{{\"at\":{NOW},\"model\":\"m\",\"out\":4}}\n"),
        )
        .unwrap();
        std::fs::rename(&other, &file).unwrap();
        assert_eq!(today(&dir), 4);
        // So is one cut shorter, and one removed counts nothing.
        std::fs::write(&file, "").unwrap();
        assert_eq!(today(&dir), 0);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(today(&dir), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_limit_is_reached_at_it_and_the_months_is_told_first() {
        let dir = scratch("reached");
        call_at(&dir, NOW);
        assert_eq!(reached_on(&dir, NOW, Some("UTC")), None, "no limits");
        set_limits(&dir, &limited(Some(3000), None)).unwrap();
        assert_eq!(reached_on(&dir, NOW, Some("UTC")), None);
        call_at(&dir, NOW + 1);
        let day = reached_on(&dir, NOW + 2, Some("UTC")).unwrap();
        assert_eq!(
            day,
            Reached {
                span: Span::Day,
                spent: 3000,
                limit: 3000
            }
        );
        // The next day, the day's limit has turned.
        assert_eq!(reached_on(&dir, NOW + DAY, Some("UTC")), None);
        set_limits(&dir, &limited(Some(3000), Some(2999))).unwrap();
        let month = reached_on(&dir, NOW + 2, Some("UTC")).unwrap();
        assert_eq!(
            (month.span, month.spent, month.limit),
            (Span::Month, 3000, 2999)
        );
        assert_eq!(
            reached_on(&dir, NOW + DAY, Some("UTC")).map(|r| r.span),
            Some(Span::Month)
        );
        assert_eq!(
            month.told(),
            "You've reached the monthly limit you set: 3,000 of 2,999 tokens this month. Hatchling won't call the model again until the 1st, unless you raise the limit in Settings, under Usage."
        );
        assert!(
            day.told()
                .contains("daily limit you set: 3,000 of 3,000 tokens today")
        );
        assert!(day.told().contains("until midnight"));
        // On this machine's clock, both calls are November's too.
        assert_eq!(reached(&dir, NOW + 2).map(|r| r.span), Some(Span::Month));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_number_is_grouped_by_thousands() {
        for (n, text) in [
            (0, "0"),
            (999, "999"),
            (1000, "1,000"),
            (12_345, "12,345"),
            (1_234_567, "1,234,567"),
            (u64::MAX, "18,446,744,073,709,551,615"),
        ] {
            assert_eq!(grouped(n), text);
        }
    }

    #[test]
    fn the_report_carries_the_limits_and_whats_spent() {
        let dir = scratch("report-limits");
        call_at(&dir, NOW);
        set_limits(&dir, &limited(None, Some(10_000))).unwrap();
        let usage = report(&dir, NOW, Some("UTC"));
        assert_eq!(usage.limits, limited(None, Some(10_000)));
        assert_eq!((usage.spent.today, usage.spent.month), (1500, 1500));
        assert_eq!(usage.spent.month_of, "2026-11");
        let none = report(Path::new(""), NOW, Some("UTC"));
        assert_eq!(none.limits, UsageLimits::default());
        assert_eq!(none.spent, UsageSpent::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
