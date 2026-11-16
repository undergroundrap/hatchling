//! The mutation loop scripts against fixtures: every refusal
//! path, not only the happy ones.
//!
//! Each test copies the scripts into a scratch git repository under
//! `/tmp` (never `std::env::temp_dir()`, which `cargo mutants` points
//! into the workspace), so a script's `ROOT` is the fixture. A fake
//! mutation command stands in for cargo-mutants; the pre-flight gate is
//! the real `hatchling-dev` binary. Every script runs under a deadline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const SCRIPTS: [&str; 5] = [
    "mutant-ledger.sh",
    "mutant-report.sh",
    "mutant-run.sh",
    "mutant-watchdog.sh",
    "stress-test.sh",
];

/// Deadline for a script that should finish at once.
const QUICK: Duration = Duration::from_secs(60);

const LIB: &str = "pub fn shipped() -> u32 {\n    1\n}\n\n#[test]\nfn shipped_is_one() {\n    assert_eq!(shipped(), 1);\n}\n";

const fn dev_binary() -> &'static str {
    env!("CARGO_BIN_EXE_hatchling-dev")
}

fn scripts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts")
}

/// A script's exit code and output.
#[derive(Debug)]
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// A scratch directory holding a fixture repository (`repo/`, with
/// the scripts in `repo/scripts/`) and files outside it.
struct Fixture {
    dir: PathBuf,
    repo: PathBuf,
    outputs: AtomicUsize,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = Path::new("/tmp").join(format!(
            "hatchling-dev-scripts-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        fs::create_dir_all(repo.join("scripts")).unwrap();
        for script in SCRIPTS {
            fs::copy(
                scripts_dir().join(script),
                repo.join("scripts").join(script),
            )
            .unwrap();
        }
        let fixture = Self {
            dir,
            repo,
            outputs: AtomicUsize::new(0),
        };
        fixture.git(&["init", "-q"]);
        // As in the workspace: run state is never tracked.
        fixture.write(".gitignore", ".mutation-state/\nmutants.out/\n");
        fixture.write("crates/demo/src/lib.rs", LIB);
        fixture.commit();
        fixture
    }

    fn write(&self, path: &str, text: &str) -> PathBuf {
        let path = self.repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    /// A file outside the repository.
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    /// An executable script outside the repository.
    fn exe(&self, name: &str, text: &str) -> PathBuf {
        let path = self.file(name, text);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn commit(&self) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "test(fixture): snapshot"]);
        self.git(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// `bash scripts/<name>` in the repository.
    fn script(&self, name: &str) -> Command {
        let mut command = Command::new("bash");
        command
            .arg(self.repo.join("scripts").join(name))
            .current_dir(&self.repo);
        command
    }

    /// Start `command` with its output going to files here.
    fn spawn(&self, command: &mut Command) -> (std::process::Child, PathBuf, PathBuf) {
        let n = self.outputs.fetch_add(1, Ordering::Relaxed);
        let out = self.dir.join(format!("stdout-{n}"));
        let err = self.dir.join(format!("stderr-{n}"));
        let child = command
            .stdin(Stdio::null())
            .stdout(fs::File::create(&out).unwrap())
            .stderr(fs::File::create(&err).unwrap())
            .spawn()
            .unwrap();
        (child, out, err)
    }

    /// Run `command` to completion within `limit`.
    fn run(&self, command: &mut Command, limit: Duration) -> Ran {
        let (child, out, err) = self.spawn(command);
        finish(child, &out, &err, limit)
    }

    /// The ledger's entries, oldest first.
    fn ledger(&self) -> Vec<Value> {
        let dir = self.repo.join(".mutation-state/ledger");
        let mut paths: Vec<_> = fs::read_dir(dir)
            .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
            .unwrap_or_default();
        paths.sort();
        paths
            .iter()
            .map(|path| serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap())
            .collect()
    }

    /// The one run's PID file, once it exists.
    fn run_pid(&self) -> String {
        let runs = self.repo.join(".mutation-state/runs");
        let mut found = None;
        let appeared = wait_for(QUICK, || {
            found = fs::read_dir(&runs).ok().and_then(|entries| {
                entries.map(|entry| entry.unwrap().path()).find(|path| {
                    path.to_string_lossy().ends_with(".pid")
                        && !path.to_string_lossy().ends_with(".watchdog.pid")
                })
            });
            found.as_ref().is_some_and(|path| {
                fs::read_to_string(path).is_ok_and(|pid| !pid.trim().is_empty())
            })
        });
        assert!(appeared, "the run never wrote its PID file");
        fs::read_to_string(found.unwrap())
            .unwrap()
            .trim()
            .to_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn finish(mut child: std::process::Child, out: &Path, err: &Path, limit: Duration) -> Ran {
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let status = status.expect("the script outlived its deadline");
    Ran {
        code: status.code(),
        stdout: fs::read_to_string(out).unwrap(),
        stderr: fs::read_to_string(err).unwrap(),
    }
}

fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// A JSON object's field, or null.
fn field(value: &Value, key: &str) -> Value {
    value.get(key).cloned().unwrap_or(Value::Null)
}

/// The `n`th ledger entry.
fn nth(ledger: &[Value], n: usize) -> &Value {
    ledger.get(n).unwrap()
}

fn signal(signal: &str, pid: &str) {
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_owned()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -{signal} {pid}");
}

fn name(n: usize) -> String {
    format!("src/lib.rs:{n}:5: replace f{n} -> u32 with 0")
}

/// An outcome report: one entry per `(mutant, summary)`, with the
/// top-level counts those entries make.
fn outcomes(entries: &[(String, &str)], total: usize) -> Value {
    let count = |summary: &str| entries.iter().filter(|(_, s)| *s == summary).count();
    let mut list = vec![json!({"scenario": "Baseline", "summary": "Success"})];
    list.extend(entries.iter().map(
        |(mutant, summary)| json!({"scenario": {"Mutant": {"name": mutant}}, "summary": summary}),
    ));
    json!({
        "total_mutants": total,
        "caught": count("CaughtMutant"),
        "missed": count("MissedMutant"),
        "timeout": count("Timeout"),
        "unviable": count("Unviable"),
        "success": 1,
        "cargo_mutants_version": "27.1.0",
        "outcomes": list,
    })
}

fn discovered(names: &[String]) -> Value {
    Value::Array(names.iter().map(|n| json!({"name": n})).collect())
}

/// Write a cargo-mutants output dir: outcomes, and mutants.json if given.
fn write_report(dir: &Path, report: &Value, listed: Option<&Value>) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("outcomes.json"), report.to_string()).unwrap();
    if let Some(listed) = listed {
        fs::write(dir.join("mutants.json"), listed.to_string()).unwrap();
    }
}

fn four() -> Vec<(String, &'static str)> {
    vec![
        (name(1), "CaughtMutant"),
        (name(2), "MissedMutant"),
        (name(3), "Timeout"),
        (name(4), "Unviable"),
    ]
}

fn names_of(entries: &[(String, &str)]) -> Vec<String> {
    entries.iter().map(|(n, _)| n.clone()).collect()
}

#[test]
fn report_names_what_a_complete_run_missed_and_timed_out() {
    let fixture = Fixture::new("report-floor");
    let out = fixture.dir.join("out");
    let entries = four();
    write_report(
        &out,
        &outcomes(&entries, 4),
        Some(&discovered(&names_of(&entries))),
    );
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(1), "{ran:?}");
    for line in [
        "progress: 4 of 4 discovered mutants tested (100%)",
        "caught 1, missed 1, timeout 1, unviable 1",
        "missed (1):",
        "timeout (1):",
        "COMPLETE, FLOOR FAILED: 1 missed, 1 timed out.",
    ] {
        assert!(ran.stdout.contains(line), "{line:?} not in {}", ran.stdout);
    }
    for n in [2, 3] {
        let line = format!("  {}", name(n));
        assert!(ran.stdout.contains(&line), "{line:?}");
    }
}

#[test]
fn report_passes_a_clean_complete_run() {
    let fixture = Fixture::new("report-clean");
    let out = fixture.dir.join("out");
    let entries = vec![(name(1), "CaughtMutant"), (name(2), "Unviable")];
    write_report(
        &out,
        &outcomes(&entries, 2),
        Some(&discovered(&names_of(&entries))),
    );
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert!(
        ran.stdout
            .contains("COMPLETE: every discovered mutant was tested")
    );
    // Without mutants.json the count is `total_mutants`.
    fs::remove_file(out.join("mutants.json")).unwrap();
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(0), "{ran:?}");
}

#[test]
fn report_refuses_complete_unless_discovered_equals_tested() {
    let fixture = Fixture::new("report-incomplete");
    let out = fixture.dir.join("out");
    let entries = four();
    let mut all = names_of(&entries);
    all.extend([name(5), name(6)]);
    write_report(&out, &outcomes(&entries, 6), Some(&discovered(&all)));
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(2), "{ran:?}");
    assert!(ran.stdout.contains("never tested (2):"), "{}", ran.stdout);
    assert!(ran.stdout.contains("INCOMPLETE: 4 of 6"), "{}", ran.stdout);
    assert!(
        !ran.stdout.lines().any(|line| line.starts_with("COMPLETE")),
        "{}",
        ran.stdout
    );
    // By count, without mutants.json.
    fs::remove_file(out.join("mutants.json")).unwrap();
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(2), "{ran:?}");
    // A name tested twice, or tested and never discovered, is not complete.
    let mut twice = entries.clone();
    twice.push((name(1), "CaughtMutant"));
    write_report(
        &out,
        &outcomes(&twice, 5),
        Some(&discovered(all.get(..5).unwrap())),
    );
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(2), "{ran:?}");
    assert!(
        ran.stdout.contains("tested more than once (1):"),
        "{}",
        ran.stdout
    );
    let mut stray = four();
    stray.first_mut().unwrap().0 = name(9);
    write_report(
        &out,
        &outcomes(&stray, 4),
        Some(&discovered(&names_of(&entries))),
    );
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(2), "{ran:?}");
    assert!(
        ran.stdout.contains("tested but never discovered (1):"),
        "{}",
        ran.stdout
    );
}

#[test]
fn report_handles_the_missed_schema_once_and_refuses_a_bad_report() {
    let fixture = Fixture::new("report-schema");
    let out = fixture.dir.join("out");
    let entries = four();
    let listed = discovered(&names_of(&entries));
    // The top-level key is `missed`, the summary `MissedMutant`: a
    // report whose two disagree is inconsistent.
    let mut report = outcomes(&entries, 4);
    report
        .as_object_mut()
        .unwrap()
        .insert(String::from("missed"), json!(0));
    write_report(&out, &report, Some(&listed));
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(3), "{ran:?}");
    assert!(
        ran.stderr
            .contains("top-level 'missed' is 0, but 1 outcomes say missed"),
        "{}",
        ran.stderr
    );
    // A summary spelled like the count key is not a summary.
    let mut wrong = four();
    wrong.get_mut(1).unwrap().1 = "missed";
    write_report(&out, &outcomes(&wrong, 4), Some(&listed));
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(3), "{ran:?}");
    assert!(
        ran.stderr
            .contains("unrecognized outcome: summary 'missed'"),
        "{}",
        ran.stderr
    );
    // Unreadable, absent, or without a count.
    fs::write(out.join("outcomes.json"), "{not json").unwrap();
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(3), "{ran:?}");
    assert!(ran.stderr.contains("is unreadable"), "{}", ran.stderr);
    fs::remove_file(out.join("outcomes.json")).unwrap();
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(3), "{ran:?}");
    assert!(ran.stderr.contains("is missing"), "{}", ran.stderr);
    let mut report = outcomes(&entries, 4);
    report.as_object_mut().unwrap().remove("total_mutants");
    write_report(&out, &report, None);
    fs::remove_file(out.join("mutants.json")).unwrap();
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(3), "{ran:?}");
    write_report(&out, &outcomes(&entries, 4), None);
    fs::write(out.join("mutants.json"), "[{\"no_name\": 1}]").unwrap();
    let ran = fixture.run(fixture.script("mutant-report.sh").arg(&out), QUICK);
    assert_eq!(ran.code, Some(3), "{ran:?}");
    let ran = fixture.run(fixture.script("mutant-report.sh").args(["a", "b"]), QUICK);
    assert_eq!(ran.code, Some(64), "{ran:?}");
}

#[test]
fn report_json_is_the_machine_verdict() {
    let fixture = Fixture::new("report-json");
    let out = fixture.dir.join("out");
    let entries = four();
    let mut all = names_of(&entries);
    all.push(name(5));
    write_report(&out, &outcomes(&entries, 5), Some(&discovered(&all)));
    let ran = fixture.run(
        fixture.script("mutant-report.sh").arg("--json").arg(&out),
        QUICK,
    );
    assert_eq!(ran.code, Some(2), "{ran:?}");
    let verdict: Value = serde_json::from_str(&ran.stdout).unwrap();
    assert_eq!(field(&verdict, "complete"), json!(false));
    assert_eq!(field(&verdict, "discovered"), json!(5));
    assert_eq!(field(&verdict, "tested"), json!(4));
    assert_eq!(field(&verdict, "missed_names"), json!([name(2)]));
    assert_eq!(field(&verdict, "timeout_names"), json!([name(3)]));
    assert_eq!(field(&verdict, "untested_names"), json!([name(5)]));
}

/// Record a ledger entry with `mutant-ledger.sh record`.
fn record(fixture: &Fixture, args: &[&str]) -> Ran {
    fixture.run(
        fixture.script("mutant-ledger.sh").arg("record").args(args),
        QUICK,
    )
}

const LEDGER_ID: &str = "20261003T120000Z-0123abcd";
const LEDGER_COMMON: [&str; 4] = ["--base", "base-sha", "--log", "/logs/run.log"];

/// A fixture with a complete two-mutant run recorded as `LEDGER_ID`,
/// and that run's output dir.
fn recorded_ledger(label: &str) -> (Fixture, PathBuf) {
    let fixture = Fixture::new(label);
    let out = fixture.dir.join("out");
    let out_arg = out.to_string_lossy().into_owned();
    let entries = vec![(name(1), "CaughtMutant"), (name(2), "MissedMutant")];
    write_report(
        &out,
        &outcomes(&entries, 2),
        Some(&discovered(&names_of(&entries))),
    );
    let head = [
        "--id",
        LEDGER_ID,
        "--status",
        "complete",
        "--exit",
        "2",
        "--started",
        "2026-10-03T12:00:00Z",
        "--out",
        &out_arg,
    ];
    let ran = record(
        &fixture,
        &[&head[..], &LEDGER_COMMON[..], &["--", "a.rs", "b.rs"]].concat(),
    );
    assert_eq!(ran.code, Some(0), "{ran:?}");
    let entry_path = fixture
        .repo
        .join(".mutation-state/ledger")
        .join(format!("{LEDGER_ID}.json"));
    assert_eq!(ran.stdout.trim(), entry_path.to_string_lossy());
    (fixture, out)
}

#[test]
fn the_ledger_records_a_run_with_every_key_and_its_report() {
    let (fixture, out) = recorded_ledger("ledger-record");
    let ledger = fixture.ledger();
    let entry = nth(&ledger, 0);
    let head = fixture.git(&["rev-parse", "HEAD"]);
    for (key, value) in [
        ("schema", json!(1)),
        ("run_id", json!(LEDGER_ID)),
        ("status", json!("complete")),
        ("started_at", json!("2026-10-03T12:00:00Z")),
        ("exit_status", json!(2)),
        ("base", json!("base-sha")),
        ("head", json!(head.trim())),
        ("dirty", json!(false)),
        ("files", json!(["a.rs", "b.rs"])),
        ("discovered", json!(2)),
        ("tested", json!(2)),
        ("caught", json!(1)),
        ("missed", json!(1)),
        ("timeout", json!(0)),
        ("unviable", json!(0)),
        ("complete", json!(true)),
        ("missed_names", json!([name(2)])),
        ("timeout_names", json!([])),
        ("log", json!("/logs/run.log")),
    ] {
        assert_eq!(field(entry, key), value, "{key}");
    }
    let kept = field(entry, "outcomes");
    let kept = PathBuf::from(kept.as_str().unwrap());
    assert_eq!(
        fs::read(&kept).unwrap(),
        fs::read(out.join("outcomes.json")).unwrap()
    );
    let entry_path = fixture
        .repo
        .join(".mutation-state/ledger")
        .join(format!("{LEDGER_ID}.json"));
    for written in [&entry_path, &kept] {
        let mode = fs::metadata(written).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o444, "{}", written.display());
    }
    let tree = fixture.run(fixture.script("mutant-ledger.sh").arg("tree-hash"), QUICK);
    assert_eq!(field(entry, "tree_hash"), json!(tree.stdout.trim()));
    assert_eq!(tree.stdout.trim().len(), 64);
}

#[test]
fn the_ledger_never_rewrites_and_records_only_true_completes() {
    let (fixture, out) = recorded_ledger("ledger-refusals");
    let out_arg = out.to_string_lossy().into_owned();
    let again = record(
        &fixture,
        &[
            &["--id", LEDGER_ID, "--status", "partial"][..],
            &LEDGER_COMMON[..],
        ]
        .concat(),
    );
    assert_eq!(again.code, Some(1), "{again:?}");
    assert!(again.stderr.contains("never rewritten"), "{}", again.stderr);
    // `complete` only when the report says so.
    let entries = vec![(name(1), "CaughtMutant")];
    let all = [name(1), name(3)];
    write_report(&out, &outcomes(&entries, 2), Some(&discovered(&all)));
    let false_complete = "20261003T120001Z-0123abcd";
    let head = [
        "--id",
        false_complete,
        "--status",
        "complete",
        "--out",
        &out_arg,
    ];
    let ran = record(&fixture, &[&head[..], &LEDGER_COMMON[..]].concat());
    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert!(ran.stderr.contains("complete only when"), "{}", ran.stderr);
    let kept = format!(".mutation-state/runs/{false_complete}.outcomes.json");
    assert!(
        !fixture.repo.join(kept).exists(),
        "a refused record keeps nothing"
    );
    // A refused run has no report: its counts are null.
    let refused = "20261003T120002Z-0123abcd";
    let ran = record(
        &fixture,
        &[
            &["--id", refused, "--status", "refused"][..],
            &LEDGER_COMMON[..],
        ]
        .concat(),
    );
    assert_eq!(ran.code, Some(0), "{ran:?}");
    let ledger = fixture.ledger();
    let refused_entry = nth(&ledger, 1);
    assert_eq!(field(refused_entry, "status"), json!("refused"));
    assert_eq!(field(refused_entry, "tested"), Value::Null);
    assert_eq!(field(refused_entry, "exit_status"), Value::Null);
    assert_eq!(field(refused_entry, "outcomes"), Value::Null);
    assert_eq!(field(refused_entry, "files"), json!([]));
    // Bad ids, statuses and options are usage errors.
    for bad in [
        &["--id", "today", "--status", "partial"][..],
        &["--id", refused, "--status", "done"][..],
        &["--id", refused, "--status", "partial", "--exit", "x"][..],
        &["--bogus", "1"][..],
    ] {
        let ran = record(&fixture, &[bad, &LEDGER_COMMON[..]].concat());
        assert_eq!(ran.code, Some(64), "{bad:?}: {ran:?}");
    }
    let ran = record(&fixture, &["--id", refused, "--status", "partial"]);
    assert_eq!(ran.code, Some(64), "no --base or --log: {ran:?}");
}

#[test]
fn the_ledger_lists_and_shows_its_entries() {
    let (fixture, _) = recorded_ledger("ledger-list");
    let refused = "20261003T120002Z-0123abcd";
    let ran = record(
        &fixture,
        &[
            &["--id", refused, "--status", "refused"][..],
            &LEDGER_COMMON[..],
        ]
        .concat(),
    );
    assert_eq!(ran.code, Some(0), "{ran:?}");
    let list = fixture.run(fixture.script("mutant-ledger.sh").arg("list"), QUICK);
    assert_eq!(list.code, Some(0), "{list:?}");
    let complete_line = format!("{LEDGER_ID}  complete  tested 2/2  caught 1 missed 1 timeout 0");
    let refused_line = format!("{refused}  refused   tested -/-  caught - missed - timeout -");
    assert_eq!(
        list.stdout.lines().collect::<Vec<_>>(),
        [complete_line.as_str(), refused_line.as_str()]
    );
    let show = fixture.run(
        fixture.script("mutant-ledger.sh").args(["show", LEDGER_ID]),
        QUICK,
    );
    assert_eq!(show.code, Some(0), "{show:?}");
    let shown: Value = serde_json::from_str(&show.stdout).unwrap();
    assert_eq!(field(&shown, "run_id"), json!(LEDGER_ID));
    let missing = fixture.run(
        fixture
            .script("mutant-ledger.sh")
            .args(["show", "20261003T120009Z-0123abcd"]),
        QUICK,
    );
    assert_eq!(missing.code, Some(1), "{missing:?}");
    let bad = fixture.run(
        fixture.script("mutant-ledger.sh").args(["show", "today"]),
        QUICK,
    );
    assert_eq!(bad.code, Some(64), "{bad:?}");
    let empty = Fixture::new("ledger-empty");
    let list = empty.run(empty.script("mutant-ledger.sh").arg("list"), QUICK);
    assert_eq!(list.stdout.trim(), "no runs recorded");
}

/// A fake cargo-mutants: records its arguments, writes `report` and
/// `listed` as its output, then sleeps `hang` seconds.
fn fake_mutants(
    fixture: &Fixture,
    report: &Value,
    listed: &Value,
    hang: u32,
) -> (PathBuf, PathBuf) {
    let args = fixture.dir.join("fake-args");
    let report_file = fixture.file("fake-outcomes.json", &report.to_string());
    let listed_file = fixture.file("fake-mutants.json", &listed.to_string());
    let fake = fixture.exe(
        "fake-mutants",
        &format!(
            "#!/bin/bash\nprintf '%s\\n' \"$@\" > '{}'\nmkdir -p mutants.out\ncp '{}' mutants.out/outcomes.json\ncp '{}' mutants.out/mutants.json\nsleep {hang}\necho '{} mutants tested in 1s'\n",
            args.display(),
            report_file.display(),
            listed_file.display(),
            listed.as_array().map_or(0, Vec::len),
        ),
    );
    (fake, args)
}

fn wrapper(fixture: &Fixture, fake: &Path, base: &str, args: &[&str]) -> Command {
    let mut command = fixture.script("mutant-run.sh");
    command
        .env("HATCHLING_DEV_BIN", dev_binary())
        .env("HATCHLING_MUTANTS_CMD", fake)
        .env("MUTANT_WATCHDOG_INTERVAL", "1")
        .args(["--base", base])
        .args(args);
    command
}

#[test]
fn the_wrapper_refuses_to_launch_when_preflight_finds_anything() {
    let fixture = Fixture::new("run-refused");
    let base = fixture.git(&["rev-parse", "HEAD"]).trim().to_owned();
    fixture.write(
        "crates/demo/src/lib.rs",
        &format!("pub fn delegate() -> u32 {{\n    shipped()\n}}\n\n{LIB}"),
    );
    let entries = four();
    let (fake, args) = fake_mutants(
        &fixture,
        &outcomes(&entries, 4),
        &discovered(&names_of(&entries)),
        0,
    );
    let ran = fixture.run(
        &mut wrapper(&fixture, &fake, &base, &["crates/demo/src/lib.rs"]),
        QUICK,
    );
    assert_eq!(ran.code, Some(4), "{ran:?}");
    assert!(
        ran.stdout.contains("[delegation] crates/demo/src/lib.rs:1: `delegate` changed, but no test calls it directly"),
        "{}",
        ran.stdout
    );
    assert!(
        ran.stderr.contains("REFUSED by pre-flight"),
        "{}",
        ran.stderr
    );
    assert!(!args.exists(), "the mutation command must never start");
    let ledger = fixture.ledger();
    assert_eq!(ledger.len(), 1);
    assert_eq!(field(nth(&ledger, 0), "status"), json!("refused"));
    assert_eq!(
        field(nth(&ledger, 0), "files"),
        json!(["crates/demo/src/lib.rs"])
    );
    assert!(
        field(nth(&ledger, 0), "log")
            .as_str()
            .unwrap()
            .ends_with(".preflight.log")
    );
}

/// A program a run leaves behind doesn't hold the runner's lock: the next
/// run starts (#415). The mutation command here leaves a `sleep` running,
/// as a test's timed-out child can; with the lock's descriptor inherited,
/// the second run was refused.
#[test]
fn a_program_a_run_leaves_behind_doesnt_hold_its_lock() {
    let fixture = Fixture::new("run-leftover");
    let base = fixture.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let entries = vec![(name(1), "CaughtMutant")];
    let report = fixture.file("fake-outcomes.json", &outcomes(&entries, 1).to_string());
    let listed = fixture.file(
        "fake-mutants.json",
        &discovered(&names_of(&entries)).to_string(),
    );
    let left = fixture.dir.join("left.pids");
    let fake = fixture.exe(
        "fake-mutants",
        &format!(
            "#!/bin/bash\nmkdir -p mutants.out\ncp '{}' mutants.out/outcomes.json\ncp '{}' mutants.out/mutants.json\nsleep 30 &\necho $! >> '{}'\necho '1 mutants tested in 1s'\n",
            report.display(),
            listed.display(),
            left.display(),
        ),
    );
    let run = || {
        fixture.run(
            &mut wrapper(&fixture, &fake, &base, &["crates/demo/src/lib.rs"]),
            QUICK,
        )
    };
    let first = run();
    let second = run();
    for pid in fs::read_to_string(&left).unwrap_or_default().lines() {
        signal("KILL", pid);
    }
    assert_eq!(first.code, Some(0), "{first:?}");
    assert!(
        !second.stderr.contains("another run holds"),
        "a leftover held the lock: {}",
        second.stderr
    );
    assert_eq!(second.code, Some(0), "{second:?}");
}

#[test]
fn the_wrapper_prefixes_bare_paths_and_records_a_complete_run() {
    let fixture = Fixture::new("run-complete");
    let base = fixture.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let entries = vec![(name(1), "CaughtMutant"), (name(2), "Unviable")];
    let (fake, args) = fake_mutants(
        &fixture,
        &outcomes(&entries, 2),
        &discovered(&names_of(&entries)),
        0,
    );
    let ran = fixture.run(
        &mut wrapper(
            &fixture,
            &fake,
            &base,
            &[
                "--",
                "crates/demo/src/lib.rs",
                "-j",
                "2",
                "--exclude-re",
                "x\\.rs",
                "-f",
                "kept.rs",
                "--file=crates/demo/src/b.rs",
                "--no-shuffle",
            ],
        ),
        QUICK,
    );
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert_eq!(
        fs::read_to_string(&args)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "--file",
            "crates/demo/src/lib.rs",
            "-j",
            "2",
            "--exclude-re",
            "x\\.rs",
            "-f",
            "kept.rs",
            "--file=crates/demo/src/b.rs",
            "--no-shuffle"
        ]
    );
    let ledger = fixture.ledger();
    assert_eq!(ledger.len(), 1, "{ledger:?}");
    let entry = nth(&ledger, 0);
    assert_eq!(field(entry, "status"), json!("complete"));
    assert_eq!(field(entry, "base"), json!(base));
    assert_eq!(field(entry, "exit_status"), json!(0));
    assert_eq!(
        field(entry, "files"),
        json!(["crates/demo/src/lib.rs", "kept.rs", "crates/demo/src/b.rs"])
    );
    assert_eq!(
        (field(entry, "tested"), field(entry, "discovered")),
        (json!(2), json!(2))
    );
    // The run id names the log, the PID files and the entry alike.
    let run_id = field(entry, "run_id");
    let id = run_id.as_str().unwrap();
    let runs = fixture.repo.join(".mutation-state/runs");
    for suffix in [
        "log",
        "pid",
        "watchdog.pid",
        "watchdog.log",
        "preflight.log",
        "outcomes.json",
    ] {
        let file = runs.join(format!("{id}.{suffix}"));
        assert!(file.exists(), "{id}.{suffix}");
    }
    let log = format!("{id}.log");
    assert!(field(entry, "log").as_str().unwrap().ends_with(&log));
    assert!(
        ran.stdout
            .contains("COMPLETE: every discovered mutant was tested"),
        "{}",
        ran.stdout
    );
}

#[test]
fn the_wrapper_records_a_failed_floor_as_complete_and_never_uses_pkill() {
    let fixture = Fixture::new("run-floor");
    let base = fixture.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let entries = four();
    let (fake, _) = fake_mutants(
        &fixture,
        &outcomes(&entries, 4),
        &discovered(&names_of(&entries)),
        0,
    );
    let ran = fixture.run(&mut wrapper(&fixture, &fake, &base, &[]), QUICK);
    assert_eq!(ran.code, Some(1), "{ran:?}");
    assert!(
        ran.stdout.contains("COMPLETE, FLOOR FAILED"),
        "{}",
        ran.stdout
    );
    let ledger = fixture.ledger();
    assert_eq!(ledger.len(), 1);
    assert_eq!(field(nth(&ledger, 0), "status"), json!("complete"));
    assert_eq!(field(nth(&ledger, 0), "files"), json!([]));
    // No script finds processes by name.
    for script in SCRIPTS {
        let text = fs::read_to_string(scripts_dir().join(script)).unwrap();
        let code = text
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .any(|line| line.contains("pkill"));
        assert!(!code, "{script} runs pkill");
    }
}

#[test]
fn the_wrapper_records_a_killed_run_as_partial_and_the_watchdog_speaks() {
    let fixture = Fixture::new("run-killed");
    let base = fixture.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let entries = vec![(name(1), "CaughtMutant"), (name(2), "CaughtMutant")];
    let mut all = names_of(&entries);
    all.extend([name(3), name(4), name(5)]);
    let (fake, _) = fake_mutants(&fixture, &outcomes(&entries, 5), &discovered(&all), 30);
    let (child, out, err) = fixture.spawn(&mut wrapper(
        &fixture,
        &fake,
        &base,
        &["crates/demo/src/lib.rs"],
    ));
    let pid = fixture.run_pid();
    // The run is alive and its outcomes are not moving: the watchdog
    // reports a stall after 10 checks.
    let runs = fixture.repo.join(".mutation-state/runs");
    let stalled = wait_for(Duration::from_secs(40), || {
        fs::read_dir(&runs).unwrap().any(|entry| {
            let path = entry.unwrap().path();
            path.to_string_lossy().ends_with(".watchdog.log")
                && fs::read_to_string(&path).is_ok_and(|text| text.contains("stalled"))
        })
    });
    assert!(stalled, "the watchdog never reported the stall");
    signal("KILL", &pid);
    let ran = finish(child, &out, &err, QUICK);
    assert_eq!(ran.code, Some(2), "{ran:?}");
    assert!(ran.stderr.contains("PARTIAL"), "{}", ran.stderr);
    assert!(ran.stdout.contains("INCOMPLETE: 2 of 5"), "{}", ran.stdout);
    assert!(ran.stdout.contains("watchdog:"), "{}", ran.stdout);
    let ledger = fixture.ledger();
    assert_eq!(ledger.len(), 1);
    assert_eq!(field(nth(&ledger, 0), "status"), json!("partial"));
    assert_eq!(field(nth(&ledger, 0), "exit_status"), json!(137));
    assert_eq!(
        (
            field(nth(&ledger, 0), "tested"),
            field(nth(&ledger, 0), "discovered")
        ),
        (json!(2), json!(5))
    );
}

#[test]
fn an_interrupted_wrapper_stops_its_run_and_records_it_and_holds_the_lock() {
    let fixture = Fixture::new("run-interrupted");
    let base = fixture.git(&["rev-parse", "HEAD"]).trim().to_owned();
    let entries = vec![(name(1), "CaughtMutant")];
    let mut all = names_of(&entries);
    all.push(name(2));
    let (fake, _) = fake_mutants(&fixture, &outcomes(&entries, 2), &discovered(&all), 30);
    let (child, out, err) = fixture.spawn(&mut wrapper(&fixture, &fake, &base, &[]));
    let pid = fixture.run_pid();
    // One run at a time.
    let second = fixture.run(&mut wrapper(&fixture, &fake, &base, &[]), QUICK);
    assert_eq!(second.code, Some(75), "{second:?}");
    assert!(
        second.stderr.contains("another run holds"),
        "{}",
        second.stderr
    );
    signal("TERM", &child.id().to_string());
    let ran = finish(child, &out, &err, QUICK);
    assert_eq!(ran.code, Some(2), "{ran:?}");
    assert!(ran.stderr.contains("wrapper interrupted"), "{}", ran.stderr);
    let ledger = fixture.ledger();
    assert_eq!(ledger.len(), 1);
    assert_eq!(field(nth(&ledger, 0), "status"), json!("partial"));
    assert_eq!(field(nth(&ledger, 0), "exit_status"), json!(143));
    // The run's whole group is gone.
    let alive = Command::new("kill").args(["-0", &pid]).status().unwrap();
    assert!(!alive.success(), "the run outlived its wrapper");
    // Usage.
    let ran = fixture.run(
        fixture
            .script("mutant-run.sh")
            .arg("crates/demo/src/lib.rs"),
        QUICK,
    );
    assert_eq!(ran.code, Some(64), "{ran:?}");
    let ran = fixture.run(
        fixture
            .script("mutant-run.sh")
            .args(["--base", "x", "--expected", "many"]),
        QUICK,
    );
    assert_eq!(ran.code, Some(64), "{ran:?}");
}

#[test]
fn the_watchdog_tells_a_finished_run_from_a_dead_one() {
    let fixture = Fixture::new("watchdog");
    let gone = Command::new("true").spawn().unwrap();
    let pid = gone.id().to_string();
    let mut gone = gone;
    gone.wait().unwrap();
    let pidfile = fixture.file("run.pid", &pid);
    let finished = fixture.file("finished.log", "16 mutants tested in 2m: 16 caught\n");
    let unfinished = fixture.file("unfinished.log", "building\n");
    let watch = |log: &Path| {
        let mut command = fixture.script("mutant-watchdog.sh");
        command
            .arg("16")
            .env("MUTANT_ROOT", &fixture.repo)
            .env("MUTANT_PIDFILE", &pidfile)
            .env("MUTANT_LOG", log)
            .env("MUTANT_WATCHDOG_INTERVAL", "1");
        fixture.run(&mut command, QUICK)
    };
    let ran = watch(&finished);
    assert!(
        ran.stdout.contains("shows a completed summary line"),
        "{ran:?}"
    );
    let ran = watch(&unfinished);
    assert!(
        ran.stdout.contains("GONE with no completion signal"),
        "{ran:?}"
    );
    assert!(ran.stdout.contains("of ~16"), "{ran:?}");
}

#[test]
fn stress_counts_passes_and_failures_and_refuses_a_test_that_never_ran() {
    let fixture = Fixture::new("stress");
    let counter = fixture.dir.join("count");
    let flaky = fixture.exe(
        "flaky",
        &format!(
            "#!/bin/bash\nn=$(( $(cat '{0}' 2>/dev/null || echo 0) + 1 ))\necho \"$n\" > '{0}'\necho \"running $1\"\nif [ $((n % 3)) -eq 0 ]; then echo 'test result: FAILED. 0 passed; 1 failed'; exit 101; fi\necho 'test result: ok. 1 passed; 0 failed'\n",
            counter.display()
        ),
    );
    let stress = |cmd: &Path, args: &[&str]| {
        let mut command = fixture.script("stress-test.sh");
        command.env("STRESS_CMD", cmd).args(args);
        fixture.run(&mut command, QUICK)
    };
    let ran = stress(&flaky, &["--load", "1", "tests::racy", "6"]);
    assert_eq!(ran.code, Some(1), "{ran:?}");
    let failed = |round: u32| {
        format!(
            "round {round}: FAIL (output: {}/.mutation-state/stress/tests__racy.{round}.log)",
            fixture.repo.display()
        )
    };
    let (third, sixth) = (failed(3), failed(6));
    assert_eq!(
        ran.stdout.lines().collect::<Vec<_>>(),
        [
            "round 1: PASS",
            "round 2: PASS",
            third.as_str(),
            "round 4: PASS",
            "round 5: PASS",
            sixth.as_str(),
            "tests::racy: passed 4 of 6, failed 2 (under 1 busy loops)",
        ]
    );
    let kept: Vec<_> = fs::read_dir(fixture.repo.join(".mutation-state/stress"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    let first = fs::read_to_string(
        fixture
            .repo
            .join(".mutation-state/stress/tests__racy.3.log"),
    )
    .unwrap();
    assert!(first.contains("running tests::racy"), "{first}");
    // Every round passing: a clean exit, and old failures cleared.
    // Rounds 100 and 101: neither a multiple of 3.
    fs::write(&counter, "99").unwrap();
    let ran = stress(&flaky, &["tests::racy", "2"]);
    assert_eq!(ran.code, Some(0), "{ran:?}");
    assert_eq!(
        ran.stdout.lines().last(),
        Some("tests::racy: passed 2 of 2, failed 0")
    );
    assert_eq!(
        fs::read_dir(fixture.repo.join(".mutation-state/stress"))
            .unwrap()
            .count(),
        0
    );
    // A name that matches nothing passes vacuously: refused.
    let nothing = fixture.exe(
        "nothing",
        "#!/bin/bash\necho 'test result: ok. 0 passed; 0 failed'\n",
    );
    let ran = stress(&nothing, &["tests::typo", "3"]);
    assert_eq!(ran.code, Some(64), "{ran:?}");
    assert!(
        ran.stderr.contains("no test named 'tests::typo' ran"),
        "{}",
        ran.stderr
    );
    for bad in [
        &["t", "0"][..],
        &["t"][..],
        &["--load", "x", "t", "1"][..],
        &["-q", "t", "1"][..],
    ] {
        let ran = stress(&nothing, bad);
        assert_eq!(ran.code, Some(64), "{bad:?}: {ran:?}");
    }
}
