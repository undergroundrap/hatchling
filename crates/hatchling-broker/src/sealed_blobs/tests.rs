use hatchling_vault::{MasterKey, Store};

use super::*;

/// A sealed store of the test's, and the directory it's in.
fn store(
    name: &str,
) -> (
    crate::api_auth::tests::Scratch,
    MasterKey,
    std::path::PathBuf,
) {
    let scratch = crate::api_auth::tests::scratch(&format!("blobs-{name}"));
    let root = scratch.0.join("store");
    (scratch, MasterKey::generate().unwrap(), root)
}

/// The names of the files in `root`, sorted.
fn files(root: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_value_of_any_size_reads_back_as_written() {
    let (_scratch, key, root) = store("sizes");
    let vault = SealedVault::open_in(&root, &key, Store::MEMORY).unwrap();
    let k = key_of(&[b"value"]);
    assert_eq!(read(&vault, &k).unwrap(), None, "nothing kept yet");
    for len in [
        0,
        1,
        MAX_SECRET_BYTES - 1,
        MAX_SECRET_BYTES,
        MAX_SECRET_BYTES + 1,
        3 * MAX_SECRET_BYTES + 7,
    ] {
        let value: Vec<u8> = (0..len).map(|n| u8::try_from(n % 251).unwrap()).collect();
        write(&vault, &k, &value).unwrap();
        assert_eq!(
            read(&vault, &k).unwrap().as_deref(),
            Some(value.as_slice()),
            "{len}"
        );
    }
}

#[test]
fn a_new_value_replaces_the_old_and_leaves_none_of_its_chunks() {
    let (_scratch, key, root) = store("replace");
    let vault = SealedVault::open_in(&root, &key, Store::MEMORY).unwrap();
    let k = key_of(&[b"value"]);
    write(&vault, &k, &vec![1; 2 * MAX_SECRET_BYTES + 1]).unwrap();
    write(&vault, &k, b"small").unwrap();
    assert_eq!(read(&vault, &k).unwrap().as_deref(), Some(&b"small"[..]));
    let chunks: Vec<String> = files(&root)
        .into_iter()
        .filter(|name| name.starts_with('d'))
        .collect();
    assert_eq!(
        chunks.len(),
        1,
        "the old generation's three are gone: {chunks:?}"
    );
    assert!(remove(&vault, &k).unwrap());
    assert_eq!(read(&vault, &k).unwrap(), None);
    assert!(!remove(&vault, &k).unwrap(), "removing nothing says so");
    assert!(
        files(&root)
            .iter()
            .all(|name| !name.starts_with('d') && !name.starts_with('h')),
        "{:?}",
        files(&root)
    );
}

#[test]
fn a_write_cut_short_before_its_head_leaves_the_old_value() {
    let (_scratch, key, root) = store("cut");
    let vault = SealedVault::open_in(&root, &key, Store::MEMORY).unwrap();
    let k = key_of(&[b"value"]);
    write(&vault, &k, b"old").unwrap();
    // The next generation's chunk written, its head not: what a crash
    // between them leaves.
    let next = chunk_name(&k, 2, 0).unwrap();
    vault
        .store_quietly(&next, &Secret::new(b"new".to_vec()).unwrap())
        .unwrap();
    assert_eq!(read(&vault, &k).unwrap().as_deref(), Some(&b"old"[..]));
    // And written again whole, the stray chunk is the new value's.
    write(&vault, &k, b"newer").unwrap();
    assert_eq!(read(&vault, &k).unwrap().as_deref(), Some(&b"newer"[..]));
}

#[test]
fn a_chunk_missing_short_or_swapped_is_refused() {
    let (_scratch, key, root) = store("broken");
    let vault = SealedVault::open_in(&root, &key, Store::MEMORY).unwrap();
    let (a, b) = (key_of(&[b"a"]), key_of(&[b"b"]));
    write(&vault, &a, &vec![1; MAX_SECRET_BYTES + 5]).unwrap();
    write(&vault, &b, b"other").unwrap();
    // The second chunk gone.
    vault.remove(&chunk_name(&a, 1, 1).unwrap()).unwrap();
    assert!(matches!(read(&vault, &a), Err(Failure::Corrupt)));
    // A head naming one generation over a first chunk that isn't full.
    write(&vault, &a, &vec![1; MAX_SECRET_BYTES + 5]).unwrap();
    vault
        .store_quietly(
            &chunk_name(&a, 2, 0).unwrap(),
            &Secret::new(vec![1; 3]).unwrap(),
        )
        .unwrap();
    assert!(matches!(read(&vault, &a), Err(Failure::Corrupt)));
    // Another value's chunk moved into this one's name doesn't open.
    std::fs::copy(
        root.join(chunk_name(&b, 1, 0).unwrap().as_str()),
        root.join(chunk_name(&a, 2, 0).unwrap().as_str()),
    )
    .unwrap();
    assert!(matches!(read(&vault, &a), Err(Failure::Vault(_))));
}

#[test]
fn too_large_a_value_is_refused_before_anything_is_written() {
    let (_scratch, key, root) = store("large");
    let vault = SealedVault::open_in(&root, &key, Store::MEMORY).unwrap();
    let k = key_of(&[b"value"]);
    let most = vec![0; MAX_CHUNKS * MAX_SECRET_BYTES];
    write(&vault, &k, &most).unwrap();
    assert_eq!(
        read(&vault, &k).unwrap().map(|value| value.len()),
        Some(most.len())
    );
    assert!(matches!(
        write(&vault, &k, &vec![0; MAX_CHUNKS * MAX_SECRET_BYTES + 1]),
        Err(Failure::TooLarge)
    ));
    assert_eq!(
        read(&vault, &k).unwrap().map(|value| value.len()),
        Some(most.len())
    );
}

#[test]
fn a_key_names_its_parts_and_no_others() {
    assert_eq!(key_of(&[b"ab", b"c"]).len(), 40);
    assert_ne!(key_of(&[b"ab", b"c"]), key_of(&[b"a", b"bc"]));
    assert_ne!(key_of(&[b"ab"]), key_of(&[b"ab", b""]));
    assert_eq!(key_of(&[b"x"]), key_of(&[b"x"]));
    assert!(key_of(&[b"x"]).bytes().all(|byte| byte.is_ascii_hexdigit()));
}
