//! What a command in the Room is asked as and comes back as: the checks
//! on what the spawner sends the confined supervisor, and on what it takes
//! back from it.

use std::path::Path;

use serde::{Deserialize, Serialize};

const OUTPUT_LIMIT: usize = 4 * 1024;

/// Termination reported by the confined Room supervisor.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoomForegroundStatus {
    /// The command returned this exit code.
    Exited(u8),
    /// The command was terminated by this Linux signal.
    Signaled(u8),
    /// The supervisor could not start the requested program.
    SpawnFailed,
}

/// Untrusted output of one installed Room command. The Room it ran in is
/// kept for the next command.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoomForegroundResult {
    /// Termination reported by the supervisor.
    pub status: RoomForegroundStatus,
    /// The first 4 KiB of standard output, as bytes.
    pub stdout: Vec<u8>,
    /// The first 4 KiB of standard error, as bytes.
    pub stderr: Vec<u8>,
    /// Standard output exceeded its byte limit.
    pub stdout_truncated: bool,
    /// Standard error exceeded its byte limit.
    pub stderr_truncated: bool,
}

pub(super) fn validate_command(argv: &[String]) -> Result<(), String> {
    let Some(program) = argv.first() else {
        return Err(String::from("Room foreground execution requires a program"));
    };
    if !Path::new(program).is_absolute() {
        return Err(String::from(
            "Room foreground execution requires an absolute program path",
        ));
    }
    if argv.len() > 64
        || argv.iter().map(String::len).sum::<usize>() > 8 * 1024
        || argv.iter().any(|argument| argument.contains('\0'))
    {
        return Err(String::from("invalid or oversized Room command arguments"));
    }
    Ok(())
}

pub(super) fn decode_result(bytes: &[u8], nonce: &str) -> Result<RoomForegroundResult, String> {
    let (echo, result): (String, RoomForegroundResult) = serde_json::from_slice(bytes)
        .map_err(|_| String::from("the Room supervisor sent an invalid foreground result"))?;
    if echo != nonce {
        return Err(String::from(
            "the Room supervisor sent a result for a different dispatch",
        ));
    }
    let valid_output = |output: &[u8], truncated: bool| {
        output.len() <= OUTPUT_LIMIT && (!truncated || output.len() == OUTPUT_LIMIT)
    };
    let valid_status = match result.status {
        RoomForegroundStatus::Exited(_) => true,
        RoomForegroundStatus::Signaled(signal) => (1..=64).contains(&signal),
        RoomForegroundStatus::SpawnFailed => result.stdout.is_empty() && result.stderr.is_empty(),
    };
    if !valid_status
        || !valid_output(&result.stdout, result.stdout_truncated)
        || !valid_output(&result.stderr, result.stderr_truncated)
    {
        return Err(String::from(
            "the Room supervisor sent an invalid foreground result",
        ));
    }
    Ok(result)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const EXITED: &[u8] =
        br#"{"status":{"exited":23},"stdout":[255],"stderr":[],"stdout_truncated":false,"stderr_truncated":false}"#;

    pub(crate) fn result_frame(nonce: &str, payload: &[u8]) -> Vec<u8> {
        let payload: serde_json::Value = serde_json::from_slice(payload).unwrap();
        serde_json::to_vec(&(nonce, payload)).unwrap()
    }

    pub(crate) fn different_nonce(nonce: &str) -> String {
        let digit = u8::from_str_radix(nonce.get(..1).unwrap(), 16).unwrap() ^ 1;
        let suffix = nonce.get(1..).unwrap();
        format!("{digit:x}{suffix}")
    }

    fn fresh_nonce() -> String {
        use std::fmt::Write as _;

        let bytes = ring::rand::generate::<[u8; 32]>(&ring::rand::SystemRandom::new())
            .unwrap()
            .expose();
        let mut value = String::with_capacity(64);
        for byte in bytes {
            write!(&mut value, "{byte:02x}").unwrap();
        }
        value
    }

    #[test]
    fn validate_command_requires_bounded_absolute_argv() {
        let mut args = vec![String::from("/bin/true"); 64];
        assert!(validate_command(&args).is_ok());
        args.push(String::new());
        assert!(validate_command(&args).is_err());
        let mut long = String::from("/");
        long.push_str(&"x".repeat(8191));
        assert!(validate_command(&[long.clone()]).is_ok());
        assert!(validate_command(&[long, String::from("x")]).is_err());
        assert!(validate_command(&[String::from("/bin/true"), String::from("\0")]).is_err());
        assert!(validate_command(&[String::from("bin/true")]).is_err());
        assert!(validate_command(&[]).is_err());
    }

    #[test]
    fn decode_result_checks_both_output_bounds_and_termination() {
        let nonce = fresh_nonce();
        assert_eq!(nonce.len(), 64);
        let base = decode_result(&result_frame(&nonce, EXITED), &nonce).unwrap();
        assert_eq!(base.status, RoomForegroundStatus::Exited(23));
        assert_eq!(base.stdout, [255]);
        let mut value: serde_json::Value = serde_json::from_slice(EXITED).unwrap();
        for stream in ["stdout", "stderr"] {
            let mut over = value.clone();
            *over.get_mut(stream).unwrap() = serde_json::json!(vec![255_u8; 4097]);
            assert!(decode_result(&serde_json::to_vec(&(&nonce, over)).unwrap(), &nonce).is_err());
            let mut wrong_marker = value.clone();
            *wrong_marker.get_mut(format!("{stream}_truncated")).unwrap() = true.into();
            assert!(
                decode_result(
                    &serde_json::to_vec(&(&nonce, wrong_marker)).unwrap(),
                    &nonce
                )
                .is_err()
            );
        }
        for status in [
            serde_json::json!({"signaled": 0}),
            serde_json::json!({"signaled": 65}),
            serde_json::json!("spawn-failed"),
            serde_json::json!({"exited": 256}),
        ] {
            *value.get_mut("status").unwrap() = status;
            assert!(
                decode_result(&serde_json::to_vec(&(&nonce, &value)).unwrap(), &nonce).is_err()
            );
        }
        assert!(decode_result(b"bad", &nonce).is_err());
        let earlier = different_nonce(&nonce);
        assert_eq!(earlier.len(), nonce.len());
        assert_ne!(earlier, nonce);
        assert!(decode_result(&result_frame(&earlier, EXITED), &nonce).is_err());
        let at_cap = RoomForegroundResult {
            status: RoomForegroundStatus::Signaled(15),
            stdout: vec![255; 4096],
            stderr: vec![255; 4096],
            stdout_truncated: true,
            stderr_truncated: false,
        };
        let encoded = serde_json::to_vec(&(&nonce, &at_cap)).unwrap();
        assert_eq!(decode_result(&encoded, &nonce).unwrap(), at_cap);
    }
}
