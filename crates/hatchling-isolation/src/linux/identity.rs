//! A cell's identity on Linux: a user-namespace id map, validated for
//! the broker's privilege, and how the broker appears inside it.

use hatchling_sys::IdMap;

use crate::{CellIdentity, Error, Mapping, PlatformError};

/// The default id-map base: the nobody uid for a root broker (the
/// cell's root must be unprivileged), the broker's own uid otherwise
/// (the only map the kernel will accept unprivileged).
pub(super) const fn default_principal(euid: u32) -> u32 {
    if euid == 0 { 65534 } else { euid }
}

/// The id map a cell runs under: `principal` (or the default for this
/// broker's privilege) spanning `count` uids, validated against the
/// broker's own uid.
///
/// # Errors
///
/// Returns [`Error::Identity`] if the map is malformed or fails the
/// policy for this broker's privilege.
pub(super) fn identity(
    euid: u32,
    principal: Option<u32>,
    count: u32,
) -> Result<CellIdentity, Error> {
    let principal = principal.unwrap_or_else(|| default_principal(euid));
    let map = id_map(principal, count)?;
    map.validate_for_euid(euid)
        .map_err(|source| Error::Identity(PlatformError::new(source)))?;
    Ok(CellIdentity::direct(principal, count))
}

/// A cell's identity on the owner's subordinate ids: `ids` (the cell's
/// uid and gid, see [`hatchling_sys::subid::own_cell_ids`]), one wide,
/// granted through the helpers that `helpers` checks.
///
/// # Errors
///
/// Returns [`Error::Identity`] for a root broker, which gives each cell
/// its own uid itself, or what `ids` or `helpers` refuses.
pub(super) fn subordinate(
    euid: u32,
    ids: impl FnOnce() -> Result<(u32, u32), hatchling_sys::Error>,
    helpers: impl FnOnce() -> Result<(), hatchling_sys::Error>,
) -> Result<CellIdentity, Error> {
    let refused = |source| Error::Identity(PlatformError::new(source));
    if euid == 0 {
        return Err(refused(hatchling_sys::Error::InvalidIdMap {
            reason: String::from(
                "a root broker gives each cell its own uid itself: \
                 subordinate ids are for a broker without root",
            ),
        }));
    }
    let (principal, group) = ids().map_err(refused)?;
    helpers().map_err(refused)?;
    Ok(CellIdentity {
        principal,
        group,
        count: 1,
        mapping: Mapping::Subordinate,
    })
}

/// The id map for `principal` spanning `count` uids.
///
/// # Errors
///
/// Returns [`Error::Identity`] if `principal + count` overflows or the
/// count is zero.
pub(super) fn id_map(principal: u32, count: u32) -> Result<IdMap, Error> {
    IdMap::new(principal, count).map_err(|source| Error::Identity(PlatformError::new(source)))
}

/// Reads the kernel overflow uid: the uid that unmapped kuids appear as
/// inside a user namespace. The agent sees the broker as this uid.
///
/// # Errors
///
/// Returns [`Error::Spawn`] if the sysctl cannot be read or parsed.
/// Failing closed is correct: without it the agent's
/// `HATCHLING_BROKER_UID` would be a guess, and a guessed peer identity
/// is no identity.
fn read_overflow_uid() -> Result<u32, Error> {
    let raw =
        std::fs::read_to_string("/proc/sys/kernel/overflowuid").map_err(|source| Error::Spawn {
            reason: format!("cannot read kernel overflow uid: {source}"),
        })?;
    raw.trim().parse::<u32>().map_err(|_| Error::Spawn {
        reason: format!("kernel overflow uid is not a u32: {raw:?}"),
    })
}

/// The uid the broker appears as inside a cell with `identity`.
///
/// The kernel translates the broker's host uid through the cell's uid
/// map: a host uid inside `[base, base + count)` appears in-cell as
/// `uid - base`. A host uid outside the map is unmapped and appears as
/// the kernel's overflow uid. `IdMap::new` rejects maps whose
/// `base + count` overflows `u32`, so the range check below is exact.
///
/// The common cases: an unprivileged broker maps only its own uid
/// (`base == euid`), so the agent sees the broker as uid 0; a root
/// broker with base 65534 leaves its own uid 0 unmapped, so the agent
/// sees the overflow uid.
///
/// # Errors
///
/// Returns [`Error::Identity`] for a malformed identity, or
/// [`Error::Spawn`] if the broker is unmapped and the kernel's overflow
/// uid cannot be read.
pub(super) fn broker_in_cell(euid: u32, identity: CellIdentity) -> Result<u32, Error> {
    let map = id_map(identity.principal, identity.count)?;
    // Mapped: the kernel reports euid - base inside the cell. The
    // subtraction cannot underflow: contains() guarantees euid >= base.
    if map.contains(euid) {
        return Ok(euid - map.base());
    }
    // Unmapped: the kernel reports the overflow uid.
    read_overflow_uid()
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn cell(principal: u32, count: u32) -> CellIdentity {
        CellIdentity::direct(principal, count)
    }

    #[test]
    fn a_subordinate_identity_is_the_owners_cell_ids_through_the_helpers() {
        let refusal = |reason: &str| {
            let reason = reason.to_owned();
            move || Err(hatchling_sys::Error::InvalidIdMap { reason })
        };
        assert_eq!(
            subordinate(1000, || Ok((100_000, 200_000)), || Ok(())).unwrap(),
            CellIdentity {
                principal: 100_000,
                group: 200_000,
                count: 1,
                mapping: Mapping::Subordinate,
            }
        );
        let root = subordinate(0, || Ok((100_000, 200_000)), || Ok(())).unwrap_err();
        assert!(
            matches!(&root, Error::Identity(_)) && root.to_string().contains("root broker"),
            "{root}"
        );
        let no_range = subordinate(1000, refusal("no range"), || Ok(())).unwrap_err();
        assert!(no_range.to_string().contains("no range"), "{no_range}");
        let bad_helper = subordinate(1000, || Ok((100_000, 200_000)), refusal("not setuid"))
            .unwrap_err();
        assert!(bad_helper.to_string().contains("not setuid"), "{bad_helper}");
    }

    #[test]
    fn default_principal_depends_on_privilege() {
        // Mutant: default_principal replaced with 1. Root gets the
        // nobody uid (65534); unprivileged gets its own uid.
        assert_eq!(default_principal(0), 65534);
        assert_eq!(default_principal(1000), 1000);
        assert_eq!(default_principal(65534), 65534);
    }

    #[test]
    fn an_identity_defaults_to_the_privilege_default_and_keeps_its_count() {
        let euid = hatchling_sys::euid();
        assert_eq!(
            identity(euid, None, 1).unwrap(),
            cell(default_principal(euid), 1)
        );
        // An unprivileged broker can only map itself.
        assert_eq!(identity(1000, Some(1000), 1).unwrap(), cell(1000, 1));
    }

    #[test]
    fn an_identity_the_kernel_would_refuse_is_refused() {
        // A zero count is malformed; an unprivileged broker cannot map
        // a foreign uid, nor more than itself; a root broker cannot map
        // the cell's root to host root.
        for (euid, principal, count) in
            [(1000, 1000, 0), (1000, 2000, 1), (1000, 1000, 2), (0, 0, 1)]
        {
            assert!(
                matches!(
                    identity(euid, Some(principal), count),
                    Err(Error::Identity(_))
                ),
                "euid {euid} must not map {principal}+{count}"
            );
        }
    }

    #[test]
    fn an_id_map_is_built_or_refused_as_hatchling_sys_does() {
        assert_eq!(id_map(1000, 10).unwrap().base(), 1000);
        assert!(matches!(id_map(u32::MAX, 2), Err(Error::Identity(_))));
    }

    #[test]
    fn read_overflow_uid_matches_the_kernel() {
        // Mutant: read_overflow_uid replaced with Ok(1). Compare
        // against the raw sysctl content.
        let raw = std::fs::read_to_string("/proc/sys/kernel/overflowuid")
            .expect("overflowuid sysctl must be readable");
        let expected: u32 = raw.trim().parse().expect("overflowuid must parse");
        assert_eq!(read_overflow_uid().unwrap(), expected);
        // And it must not be the mutant's constant unless the kernel
        // actually says 1 (it doesn't on standard kernels: 65534).
        if expected != 1 {
            assert_ne!(read_overflow_uid().unwrap(), 1);
        }
    }

    #[test]
    fn broker_in_cell_maps_unprivileged_broker_to_zero() {
        // The unprivileged-namespace case: an unprivileged broker maps only its own uid,
        // so the kernel reports it in-cell as uid 0. Mutants: >= to >,
        // && to ||, or deleting the branch all change this to the
        // overflow uid.
        assert_eq!(broker_in_cell(1000, cell(1000, 1)).unwrap(), 0);
    }

    #[test]
    fn broker_in_cell_translates_inside_a_wider_map() {
        // A root broker mapping several uids: host uid 1005 appears
        // in-cell as 5. Mutant: `euid - base` replaced with `euid`.
        let map = cell(1000, 10);
        assert_eq!(broker_in_cell(1005, map).unwrap(), 5);
        assert_eq!(broker_in_cell(1000, map).unwrap(), 0);
        assert_eq!(broker_in_cell(1009, map).unwrap(), 9);
    }

    #[test]
    fn broker_in_cell_falls_back_to_overflow_uid() {
        // Unmapped in both directions: below the base, and at/past
        // base + count (the < to <= mutant would map 1010). The kernel
        // reports the overflow uid.
        let map = cell(1000, 10);
        let overflow = read_overflow_uid().unwrap();
        assert_eq!(broker_in_cell(999, map).unwrap(), overflow);
        assert_eq!(broker_in_cell(1010, map).unwrap(), overflow);
        // The root-broker default: base 65534/count 1 leaves host uid 0
        // unmapped.
        assert_eq!(broker_in_cell(0, cell(65534, 1)).unwrap(), overflow);
    }

    #[test]
    fn a_malformed_identity_has_no_broker_uid() {
        assert!(matches!(
            broker_in_cell(1000, cell(u32::MAX, 2)),
            Err(Error::Identity(_))
        ));
    }
}
