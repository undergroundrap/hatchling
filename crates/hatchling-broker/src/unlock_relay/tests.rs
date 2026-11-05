use std::io::{Read as _, Seek as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use hatchling_vault::MasterKey;
use zeroize::Zeroizing;

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("hatchling-unlock-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A connected pair whose reads give up after ten seconds: a side that
/// goes quiet fails the test, rather than hanging it.
fn pair() -> (UnixStream, UnixStream) {
    let (one, other) = UnixStream::pair().unwrap();
    for end in [&one, &other] {
        end.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
    }
    (one, other)
}

/// What the operator's side heard: each question, and whether it was a
/// secret one.
type Heard = Vec<(String, bool)>;

/// The operator's side on its own thread: `answers` given in turn.
fn operator(
    stream: UnixStream,
    answers: Vec<&'static str>,
) -> std::thread::JoinHandle<(Result<(), String>, Heard)> {
    // A keeper that goes quiet fails the test, rather than hanging it.
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    std::thread::spawn(move || {
        let mut heard = Vec::new();
        let mut answers = answers.into_iter();
        let result = operate(&stream, &[], &mut |prompt, secret| {
            heard.push((prompt.to_owned(), secret));
            Ok(Zeroizing::new(
                answers.next().unwrap_or_default().to_owned(),
            ))
        });
        (result, heard)
    })
}

/// A try that takes the keys, asks for a code, and opens with `good`.
fn code_try(good: &'static str) -> impl FnMut(&UnixStream) -> Result<MasterKey, Error> {
    move |stream| {
        let keys = take_keys(stream)?;
        assert!(keys.is_empty());
        let code = relay(stream, "Current code:", false)?;
        if code.as_str() == good {
            Ok(MasterKey::generate().unwrap())
        } else {
            Err(Error::Usage {
                reason: String::from("that code did not match"),
            })
        }
    }
}

#[test]
fn a_try_that_fails_is_told_why_and_the_next_one_unlocks() {
    let dir = scratch("tries");
    let path = dir.join("unlock.sock");
    let listener = listen(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let me = hatchling_sys::euid();
    let waiting = std::thread::spawn(move || wait(&listener, me, &mut code_try("123456")));

    let wrong = operator(UnixStream::connect(&path).unwrap(), vec!["000000"]);
    let (result, heard) = wrong.join().unwrap();
    assert_eq!(result, Err(String::from("that code did not match")));
    assert_eq!(heard, vec![(String::from("Current code:"), false)]);

    let right = operator(UnixStream::connect(&path).unwrap(), vec!["123456"]);
    assert_eq!(right.join().unwrap().0, Ok(()));
    assert!(waiting.join().unwrap().is_ok(), "the keeper carries on");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_socket_left_by_an_earlier_start_is_replaced_and_its_directory_made_private() {
    let dir = scratch("stale");
    let path = dir.join("fresh").join("unlock.sock");
    let first = listen(&path).unwrap();
    drop(first);
    assert!(path.exists(), "a socket outlives its listener");
    let again = listen(&path).unwrap();
    let mode = std::fs::metadata(dir.join("fresh"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
    drop(again);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_the_allowed_user_is_admitted_and_another_is_told() {
    let (keeper, operator) = pair();
    let me = hatchling_sys::euid();
    assert!(admitted(&keeper, me));
    assert!(!admitted(&keeper, me.wrapping_add(1)));
    let told: ToOperator = serde_json::from_slice(&read_frame(&operator).unwrap()).unwrap();
    assert_eq!(
        told,
        ToOperator::Failed {
            reason: String::from("only root may unlock the vault: run it with sudo")
        }
    );
}

#[test]
fn the_pin_is_asked_as_a_secret_and_answers_go_back_in_turn() {
    let (keeper, operator_end) = pair();
    let asking = operator(operator_end, vec!["1234", "yes"]);
    assert!(take_keys(&keeper).unwrap().is_empty());
    assert_eq!(
        relay(&keeper, crate::vault_keyring::PIN_PROMPT, true)
            .unwrap()
            .as_str(),
        "1234"
    );
    assert_eq!(relay(&keeper, "Go on?", false).unwrap().as_str(), "yes");
    tell(&keeper, &ToOperator::Unlocked).unwrap();
    let (result, heard) = asking.join().unwrap();
    assert_eq!(result, Ok(()));
    assert_eq!(
        heard,
        vec![
            (String::from(crate::vault_keyring::PIN_PROMPT), true),
            (String::from("Go on?"), false)
        ]
    );
}

#[test]
fn a_key_passed_arrives_open_under_its_node() {
    let (keeper, operator_end) = pair();
    let dir = scratch("keys");
    let mut file = File::options()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(dir.join("key"))
        .unwrap();
    file.write_all(b"a key").unwrap();
    file.rewind().unwrap();
    let keys = vec![(String::from("hidraw3"), file)];
    let sending = std::thread::spawn(move || {
        let frame = serde_json::to_vec(&ToKeeper::Keys {
            nodes: vec![String::from("hidraw3")],
        })
        .unwrap();
        write_frame(&operator_end, &frame).unwrap();
        for (_, key) in &keys {
            hatchling_sys::net::send_fd(operator_end.as_fd(), key.as_fd()).unwrap();
        }
    });
    let taken = take_keys(&keeper).unwrap();
    sending.join().unwrap();
    assert_eq!(taken.len(), 1);
    let (node, mut key) = taken.into_iter().next().unwrap();
    assert_eq!(node, "hidraw3");
    let mut read = String::new();
    key.read_to_string(&mut read).unwrap();
    assert_eq!(read, "a key");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keys_that_arent_ones_are_refused_before_any_is_taken() {
    let too_many: Vec<String> = (0..=MAX_KEYS).map(|n| format!("hidraw{n}")).collect();
    for nodes in [
        vec![String::from("../hidraw0")],
        vec![String::from("hidraw")],
        vec![String::from("hidraw12345")],
        vec![String::from("sda")],
        too_many,
    ] {
        let (keeper, operator_end) = pair();
        let frame = serde_json::to_vec(&ToKeeper::Keys {
            nodes: nodes.clone(),
        })
        .unwrap();
        write_frame(&operator_end, &frame).unwrap();
        // A descriptor for each, as a command would pass them: refused for
        // the names, not for keys that never came.
        let stand_in = File::open("/dev/null").unwrap();
        for _ in &nodes {
            hatchling_sys::net::send_fd(operator_end.as_fd(), stand_in.as_fd()).unwrap();
        }
        assert!(
            matches!(take_keys(&keeper), Err(Error::Keeper { .. })),
            "{nodes:?}"
        );
    }
    // The same, named as keys are, are taken.
    let (keeper, operator_end) = pair();
    let nodes: Vec<String> = (0..MAX_KEYS).map(|n| format!("hidraw{n}")).collect();
    let frame = serde_json::to_vec(&ToKeeper::Keys { nodes }).unwrap();
    write_frame(&operator_end, &frame).unwrap();
    let stand_in = File::open("/dev/null").unwrap();
    for _ in 0..MAX_KEYS {
        hatchling_sys::net::send_fd(operator_end.as_fd(), stand_in.as_fd()).unwrap();
    }
    assert_eq!(take_keys(&keeper).unwrap().len(), MAX_KEYS);
    // Something else first is no try.
    let (keeper, operator_end) = pair();
    let answer = serde_json::to_vec(&ToKeeper::Answer {
        text: String::from("x"),
    })
    .unwrap();
    write_frame(&operator_end, &answer).unwrap();
    assert!(matches!(take_keys(&keeper), Err(Error::Keeper { .. })));
}

#[test]
fn a_node_is_hidraw_and_a_number() {
    for node in ["hidraw0", "hidraw12", "hidraw9999"] {
        assert!(valid_node(node), "{node}");
    }
    for node in [
        "hidraw",
        "hidraw10000",
        "hidraw1a",
        "Hidraw1",
        "/dev/hidraw0",
        "hidraw-1",
        "",
    ] {
        assert!(!valid_node(node), "{node}");
    }
}

#[test]
fn an_answer_too_long_or_not_an_answer_ends_the_try() {
    for reply in [
        serde_json::to_vec(&ToKeeper::Answer {
            text: "9".repeat(MAX_ANSWER + 1),
        })
        .unwrap(),
        serde_json::to_vec(&ToKeeper::Keys { nodes: Vec::new() }).unwrap(),
        b"{\"kind\":\"answer\",\"text\":\"1\",\"more\":1}".to_vec(),
    ] {
        let (keeper, operator_end) = pair();
        let answering = std::thread::spawn(move || {
            let _ = read_frame(&operator_end).unwrap();
            write_frame(&operator_end, &reply).unwrap();
        });
        assert!(matches!(
            relay(&keeper, "Code:", false),
            Err(Error::Keeper { .. })
        ));
        answering.join().unwrap();
    }
    let (keeper, operator_end) = pair();
    let answering = std::thread::spawn(move || {
        let _ = read_frame(&operator_end).unwrap();
        write_frame(
            &operator_end,
            &serde_json::to_vec(&ToKeeper::Answer {
                text: "9".repeat(MAX_ANSWER),
            })
            .unwrap(),
        )
        .unwrap();
    });
    assert_eq!(relay(&keeper, "Code:", false).unwrap().len(), MAX_ANSWER);
    answering.join().unwrap();
}

#[test]
fn the_operators_side_stops_on_what_it_doesnt_read_or_a_keeper_gone() {
    let (keeper, operator_end) = pair();
    let speaking = std::thread::spawn(move || {
        let _ = read_frame(&keeper).unwrap();
        write_frame(&keeper, b"{\"kind\":\"shout\"}").unwrap();
    });
    let result = operate(&operator_end, &[], &mut |_, _| {
        Ok(Zeroizing::new(String::new()))
    });
    assert_eq!(
        result,
        Err(String::from(
            "Hatchling said something this command doesn't read"
        ))
    );
    speaking.join().unwrap();
    let (keeper, operator_end) = pair();
    drop(keeper);
    assert!(
        operate(&operator_end, &[], &mut |_, _| Ok(Zeroizing::new(
            String::new()
        )))
        .is_err_and(|why| why.starts_with("Hatchling stopped answering"))
    );
}

#[test]
fn a_vault_with_no_keyring_needs_nobody() {
    assert!(!needs_a_person(&KeyringConfig::default()));
    let missing = KeyringConfig {
        keyring: Some(PathBuf::from("/nonexistent/hatchling/keyring")),
        ..KeyringConfig::default()
    };
    assert!(!needs_a_person(&missing));
}

#[test]
fn the_socket_is_beside_the_vault_in_a_directory_of_its_own() {
    let vault = VaultConfig {
        root: PathBuf::from("/var/lib/hatchling/vault"),
        key_file: PathBuf::from("/var/lib/hatchling/vault.key/key"),
        grants: Vec::new(),
    };
    assert_eq!(
        unlock_dir(&vault),
        PathBuf::from("/var/lib/hatchling/vault.unlock")
    );
    assert_eq!(
        socket_path(&vault),
        PathBuf::from("/var/lib/hatchling/vault.unlock/unlock.sock")
    );
}

/// What a listener that nobody admits does with a try: tells it, and
/// waits on.
#[test]
fn a_refused_peer_never_reaches_the_unlock() {
    let dir = scratch("refused");
    let path = dir.join("unlock.sock");
    let listener: UnixListener = listen(&path).unwrap();
    let nobody = hatchling_sys::euid().wrapping_add(1);
    std::thread::spawn(move || {
        let _ = wait(&listener, nobody, &mut |_| {
            Err(Error::Usage {
                reason: String::from("a refused peer reached the unlock"),
            })
        });
    });
    let (result, heard) = operator(UnixStream::connect(&path).unwrap(), vec![])
        .join()
        .unwrap();
    assert_eq!(
        result,
        Err(String::from(
            "only root may unlock the vault: run it with sudo"
        ))
    );
    assert!(heard.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
