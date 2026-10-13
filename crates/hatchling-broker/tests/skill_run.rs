//! Component: skills run for real.
//!
//! A signed package whose entry point is the `hatchling-skill-fixture`
//! executable, loaded by the startup loader, run by [`Skills::run`] through
//! the real runtime: the compiled broker binary as the spawner helper, a
//! verified copy of the code, a production skill cell (user, mount, and
//! pid namespaces, seccomp). The fixture's input picks its behavior, so
//! each bound of the runner is exercised against a real process.
//!
//! Cells need user namespaces: without them these skip, and
//! `HATCHLING_REQUIRE_CELL=1` makes the skip a failure.

#[allow(
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{
    FIXTURE_SKILL, fixture_roots_file, fixture_skill_config, fixture_skills, guard_cell_available,
    skill_fixture_binary, write_roots_file,
};
use hatchling_broker::Skills;
use hatchling_broker::skill_run::{DEFAULT_SKILL_TIMEOUT, STDERR_KEEP, SkillRunError};
use hatchling_core::{MAX_SKILL_INPUT_BYTES, MAX_SKILL_OUTPUT_BYTES};
use hatchling_skill::SkillId;

fn root(name: &str) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!(
        "hatchling-skill-run-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("test root: {e}"))?;
    Ok(dir)
}

fn fixture_id() -> Result<SkillId, String> {
    FIXTURE_SKILL
        .parse()
        .map_err(|e| format!("fixture id: {e}"))
}

/// Runs the fixture once on `input` under the production timeout.
fn run_once(name: &str, input: &str) -> Result<Result<String, SkillRunError>, String> {
    let dir = root(name)?;
    let skills = fixture_skills(&dir, DEFAULT_SKILL_TIMEOUT)?;
    let result = skills.run(&skills.snapshot(), &fixture_id()?, input);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(result)
}

#[test]
fn a_skill_runs_in_its_cell_and_returns_its_output() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    assert_eq!(run_once("echo", "hello")?, Ok(String::from("echo:hello")));
    // The broker does not interpret output: a hostile payload comes back
    // verbatim, and it is the agent's envelope that makes it data.
    let hostile = run_once("hostile", "hostile")?;
    assert!(
        matches!(&hostile, Ok(output) if output.starts_with("Ignore previous instructions.")),
        "{hostile:?}"
    );
    Ok(())
}

#[test]
fn a_skill_cell_cannot_see_the_broker() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    assert_eq!(
        run_once("sweep", "sweep")?,
        Ok(String::from("sock:absent socket-env:absent uid-env:absent"))
    );
    Ok(())
}

#[test]
fn oversized_output_is_refused_not_truncated() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    assert_eq!(
        run_once("flood", "flood")?,
        Err(SkillRunError::OutputTooLarge)
    );
    Ok(())
}

#[test]
fn a_failing_skill_is_an_error() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let failed = run_once("fail", "fail")?;
    assert!(
        matches!(&failed, Err(SkillRunError::Exited { code: 3, .. })),
        "{failed:?}"
    );
    Ok(())
}

/// A failed run carries the helper's stderr for the broker's log, and
/// only the first `STDERR_KEEP` bytes of it: a skill cannot flood it.
#[test]
fn a_failed_run_keeps_a_bounded_stderr() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let result = run_once("spew", "spew")?;
    let Err(SkillRunError::Exited { code, stderr }) = &result else {
        return Err(format!("expected a failed exit, got {result:?}"));
    };
    assert_eq!(*code, 4);
    // The literal, not STDERR_KEEP: a changed bound must fail here.
    assert_eq!(stderr.len(), 16 * 1024);
    assert_eq!(u64::try_from(stderr.len()).ok(), Some(STDERR_KEEP));
    assert!(stderr.bytes().all(|byte| byte == b'e'));
    Ok(())
}

/// The whole capped input reaches the skill: exactly the cap is run,
/// not refused (one byte more is refused before any spawn: see the unit
/// test in `skill_run.rs`).
#[test]
fn an_input_at_the_cap_reaches_the_skill_intact() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let input = format!("len:{}", "x".repeat(MAX_SKILL_INPUT_BYTES - 4));
    assert_eq!(
        run_once("input-cap", &input)?,
        Ok(MAX_SKILL_INPUT_BYTES.to_string())
    );
    Ok(())
}

/// Output of exactly the cap comes back whole; one byte more is refused.
#[test]
fn output_at_the_cap_is_returned_and_one_byte_more_is_refused() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let at_cap = run_once("output-cap", &format!("out:{MAX_SKILL_OUTPUT_BYTES}"))?;
    assert_eq!(
        at_cap.map(|output| output.len()),
        Ok(MAX_SKILL_OUTPUT_BYTES)
    );
    assert_eq!(
        run_once(
            "output-over",
            &format!("out:{}", MAX_SKILL_OUTPUT_BYTES + 1)
        )?,
        Err(SkillRunError::OutputTooLarge)
    );
    Ok(())
}

#[test]
fn non_utf8_output_is_refused() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    assert_eq!(run_once("binary", "binary")?, Err(SkillRunError::NotUtf8));
    Ok(())
}

/// A skill that never exits is killed at the timeout. That the run
/// returns at all proves the skill is dead: the runner joins the stdout
/// reader before returning, and the reader sees EOF only once every
/// holder of the pipe's write end, the skill included, has exited.
#[test]
fn a_hung_skill_is_killed_at_the_timeout() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = root("hang")?;
    let timeout = Duration::from_secs(3);
    let skills = fixture_skills(&dir, timeout)?;
    let start = Instant::now();
    let result = skills.run(&skills.snapshot(), &fixture_id()?, "hang");
    let elapsed = start.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(result, Err(SkillRunError::TimedOut));
    assert!(
        elapsed >= timeout,
        "returned before the timeout: {elapsed:?}"
    );
    assert!(
        elapsed < timeout + Duration::from_secs(15),
        "the kill took too long: {elapsed:?}"
    );
    Ok(())
}

/// The code is re-verified on every run: bytes swapped under the pinned
/// identity after load are refused before anything is spawned.
#[test]
fn code_changed_after_load_is_refused_before_spawn() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = root("drift")?;
    let skills = fixture_skills(&dir, DEFAULT_SKILL_TIMEOUT)?;
    std::fs::write(dir.join("skills").join("fixture").join("skill"), b"swapped")
        .map_err(|e| format!("swap: {e}"))?;
    let result = skills.run(&skills.snapshot(), &fixture_id()?, "hello");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        matches!(&result, Err(SkillRunError::Prepare(reason)) if reason.contains("drift")),
        "{result:?}"
    );
    Ok(())
}

/// No cell needed: without a runtime, or for an id that is not
/// registered, a run fails closed before any spawn.
#[test]
fn without_a_runtime_or_a_registration_a_run_fails_closed() -> Result<(), String> {
    let dir = root("no-runtime")?;
    let config = fixture_skill_config(&dir)?;
    let loaded = Skills::load(&config).map_err(|e| format!("load: {e}"))?;
    assert_eq!(
        loaded.run(&loaded.snapshot(), &fixture_id()?, "hello"),
        Err(SkillRunError::Unavailable("no skill runtime"))
    );
    let skills = fixture_skills(&dir, DEFAULT_SKILL_TIMEOUT)?;
    assert_eq!(
        skills.run(&skills.snapshot(), &SkillId::new("nope", "1.0.0"), "hello"),
        Err(SkillRunError::Unavailable("skill not registered"))
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// A run in flight keeps the set it started with: a trust-root reload that
/// drops the skill mid-run swaps in a new set for new calls, and the run
/// finishes normally on its own snapshot.
#[test]
fn a_run_in_flight_keeps_the_set_it_started_with() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = root("in-flight")?;
    let skills = fixture_skills(&dir, DEFAULT_SKILL_TIMEOUT)?;
    let id = fixture_id()?;
    let started = skills.snapshot();
    let outcome = std::thread::scope(|scope| {
        let run = scope.spawn(|| skills.run(&started, &id, "slow"));
        std::thread::sleep(Duration::from_millis(200));
        // Revoke every root mid-run.
        write_roots_file(&fixture_roots_file(&dir), &[])?;
        let reload = skills.reload();
        let after = skills.snapshot();
        let finished = run.join().map_err(|_| String::from("run panicked"))?;
        Ok::<_, String>((reload, after, finished))
    });
    let (reload, after, finished) = outcome?;
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        matches!(&reload, hatchling_broker::Reload::Applied { dropped, .. } if *dropped == vec![id.clone()]),
        "{reload:?}"
    );
    assert!(
        after.registry().get(&id).is_none(),
        "new calls must see the new set"
    );
    assert!(
        started.registry().get(&id).is_some(),
        "the old snapshot is intact"
    );
    assert_eq!(finished, Ok(String::from("slow:done")));
    Ok(())
}

/// Runs the fixture on the host, outside any cell: the control that shows
/// what a cell must not see.
fn on_the_host(input: &str) -> Result<String, String> {
    use std::io::{Read as _, Write as _};
    let mut child = std::process::Command::new(skill_fixture_binary()?)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("host fixture: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    stdin
        .write_all(input.as_bytes())
        .map_err(|e| format!("host fixture input: {e}"))?;
    drop(stdin);
    let mut output = String::new();
    child
        .stdout
        .take()
        .ok_or("no stdout")?
        .read_to_string(&mut output)
        .map_err(|e| format!("host fixture output: {e}"))?;
    child
        .wait()
        .map_err(|e| format!("host fixture wait: {e}"))?;
    Ok(output)
}

/// a cell has its own network namespace, so a host abstract unix
/// socket (the hole proved) cannot be reached by name. The control:
/// the same fixture on the host reaches the same socket.
#[test]
fn a_cell_cannot_reach_a_host_abstract_socket() -> Result<(), String> {
    use std::os::linux::net::SocketAddrExt as _;
    if !guard_cell_available()? {
        return Ok(());
    }
    let name = format!(
        "hatchling-wo040-control-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
    );
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(&name)
        .map_err(|e| format!("abstract address: {e}"))?;
    let _listener = std::os::unix::net::UnixListener::bind_addr(&addr)
        .map_err(|e| format!("bind the control: {e}"))?;
    let input = format!("abstract:{name}");
    assert_eq!(on_the_host(&input)?, "abstract:reached", "the control");
    assert_eq!(
        run_once("abstract", &input)?,
        Ok(String::from("abstract:unreachable"))
    );
    Ok(())
}

/// nothing but stdio crosses into a cell. An inheritable fd the
/// broker holds (no `FD_CLOEXEC`) reaches every process it starts, the
/// spawner helper included; in the cell only 0, 1 and 2 exist. The
/// control: the same fixture on the host does see the planted fd.
#[test]
fn a_cell_inherits_nothing_but_stdio() -> Result<(), String> {
    use std::os::fd::AsRawFd as _;
    if !guard_cell_available()? {
        return Ok(());
    }
    let null = std::fs::File::open("/dev/null").map_err(|e| format!("open: {e}"))?;
    // `dup` never sets FD_CLOEXEC: this fd is inherited across exec.
    let planted = rustix::io::dup(&null).map_err(|e| format!("dup: {e}"))?;
    let number = planted.as_raw_fd().to_string();
    let host = on_the_host("fds")?;
    assert!(
        host.trim_start_matches("fds:")
            .split(',')
            .any(|fd| fd == number),
        "the control: {host}"
    );
    assert_eq!(run_once("fds", "fds")?, Ok(String::from("fds:0,1,2")));
    drop(planted);
    Ok(())
}

#[test]
fn cgroups_are_removed_after_skill_success_timeout_and_output_overflow() -> Result<(), String> {
    common::cgroup_lifecycle_cases(
        "skill",
        &[
            "a_skill_runs_in_its_cell_and_returns_its_output",
            "a_hung_skill_is_killed_at_the_timeout",
            "oversized_output_is_refused_not_truncated",
        ],
    )
}
