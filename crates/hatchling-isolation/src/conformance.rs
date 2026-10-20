//! The executable part of the owner-transparency contract.
//!
//! Every [`IsolationBackend`] must pass each check here, in its own
//! test suite, before it ships: a backend that cannot is not a hatchling
//! backend. Each check names what it found wrong, so a failing port
//! knows what to fix.
//!
//! The checks speak only the trait's vocabulary. A platform supplies
//! what the trait cannot: programs that end in a known way on it
//! ([`Witnesses`]).

use std::path::{Path, PathBuf};

use crate::{
    Cell, CellSpec, CellView, ENDPOINT_DIR, Filter, IsolationBackend, Origin, PROGRAM, ViewEntry,
};

/// Programs that end in a known way on the platform under test.
#[derive(Debug, Clone)]
pub struct Witnesses {
    /// A program that exits with code 0 and does nothing else.
    pub succeeds: PathBuf,
    /// A program that exits with code 1 and does nothing else.
    pub fails: PathBuf,
}

/// The backend grants nothing the spec did not.
///
/// The cell sees the endpoint directory only if the spec grants an
/// endpoint, the program, each granted directory exactly as granted, each
/// tool read-only at its own path, and otherwise only read-only runtime
/// support under names no grant uses. Only granted directories are
/// writable, and, in a CLI cell only, its devices. Its relay port is the
/// spec's, and it sees its own processes only in a CLI cell.
///
/// This is also the owner's view: [`IsolationBackend::view`] must
/// describe the whole cell, before it runs.
///
/// # Errors
///
/// Returns what was wrong: a widened, missing, reordered or
/// unexplained entry, or a view the backend could not give.
pub fn grants_nothing_unasked<B: IsolationBackend>(
    backend: &B,
    spec: &CellSpec,
) -> Result<(), String> {
    let view = backend
        .view(spec)
        .map_err(|error| format!("no view for the spec: {error}"))?;
    check_view(spec, &view)
}

/// The checks of [`grants_nothing_unasked`] on a view already in hand.
///
/// # Errors
///
/// As [`grants_nothing_unasked`].
pub fn check_view(spec: &CellSpec, view: &CellView) -> Result<(), String> {
    if view.relay != spec.relay {
        return Err(format!(
            "the relay is {:?}, but the spec asked for {:?}",
            view.relay, spec.relay
        ));
    }
    let cli = spec.filter.runs_processes();
    if view.processes != cli {
        return Err(String::from(
            "the cell's own processes are seen exactly when it is a CLI cell",
        ));
    }
    let mut entries = view.entries.iter();
    if let Some(endpoint) = &spec.endpoint {
        let entry = entries.next().ok_or("an empty view")?;
        entry_is_granted(
            entry,
            Origin::Endpoint,
            &endpoint.dir,
            &in_root(ENDPOINT_DIR),
            true,
            false,
        )?;
    }
    let entry = entries.next().ok_or("no program in the view")?;
    entry_is_granted(
        entry,
        Origin::Program,
        &spec.program,
        &in_root(PROGRAM),
        false,
        false,
    )?;
    for exposure in &spec.exposures {
        let entry = entries
            .next()
            .ok_or_else(|| format!("granted {} is missing", exposure.name))?;
        entry_is_granted(
            entry,
            Origin::Granted,
            &exposure.source,
            &exposure.name,
            true,
            exposure.writable,
        )?;
    }
    for tool in &spec.tools {
        let entry = entries
            .next()
            .ok_or_else(|| format!("tool {} is missing", tool.name))?;
        entry_is_granted(entry, Origin::Tool, &tool.source, &tool.name, false, false)?;
    }
    let granted: Vec<&str> = view
        .entries
        .iter()
        .filter(|entry| entry.origin != Origin::Runtime)
        .map(|entry| entry.name.as_str())
        .collect();
    for entry in entries {
        if entry.origin == Origin::Device {
            if !cli {
                return Err(format!(
                    "device {} in a cell that is not a CLI's",
                    entry.name
                ));
            }
            if granted.iter().filter(|name| **name == entry.name).count() > 1 {
                return Err(format!("device {} shadows another entry", entry.name));
            }
            continue;
        }
        if entry.origin != Origin::Runtime {
            return Err(format!("{} is not explained by the spec", entry.name));
        }
        if entry.writable {
            return Err(format!("runtime support {} is writable", entry.name));
        }
        if granted.contains(&entry.name.as_str()) {
            return Err(format!("runtime support shadows {}", entry.name));
        }
    }
    Ok(())
}

/// One view entry is what the spec granted: its origin, host path,
/// name, kind and writability.
fn entry_is_granted(
    entry: &ViewEntry,
    origin: Origin,
    source: &Path,
    name: &str,
    dir: bool,
    writable: bool,
) -> Result<(), String> {
    let wanted = std::fs::canonicalize(source)
        .map_err(|error| format!("cannot resolve {}: {error}", source.display()))?;
    if entry.origin != origin || entry.name != name {
        return Err(format!(
            "expected {origin:?} at {name}, found {:?} at {}",
            entry.origin, entry.name
        ));
    }
    if entry.source != wanted {
        return Err(format!(
            "{name} is {}, not the granted {}",
            entry.source.display(),
            wanted.display()
        ));
    }
    if entry.dir != dir {
        return Err(format!("{name} is the wrong kind of path"));
    }
    if entry.writable && !writable {
        return Err(format!("{name} is writable, but was granted read-only"));
    }
    if writable && !entry.writable {
        return Err(format!("{name} was granted writable, but is read-only"));
    }
    Ok(())
}

/// A cell-layout path as a name relative to the cell's root.
fn in_root(path: &str) -> String {
    path.trim_start_matches('/').to_owned()
}

/// The owner reads every exit faithfully: a cell running the succeeding
/// witness ends with code 0, the failing one with code 1, and both are
/// reaped by the wait.
///
/// # Errors
///
/// Returns what was wrong: a cell that could not be built or waited
/// on, or a code that is not the program's.
pub fn exits_reach_the_owner<B: IsolationBackend>(
    backend: &B,
    spec: &CellSpec,
    witnesses: &Witnesses,
) -> Result<(), String> {
    for (program, wanted) in [(&witnesses.succeeds, 0), (&witnesses.fails, 1)] {
        let mut run = spec.clone();
        run.program.clone_from(program);
        let cell = backend
            .isolate(&run)
            .map_err(|error| format!("cannot isolate {}: {error}", program.display()))?;
        let exit = cell
            .wait()
            .map_err(|error| format!("cannot wait on {}: {error}", program.display()))?;
        if exit.code != wanted {
            return Err(format!(
                "{} ended with {}, not {wanted}",
                program.display(),
                exit.code
            ));
        }
    }
    Ok(())
}

/// The owner can see who a cell runs as: the identity the backend
/// grants for the default is a host principal, never the owner's own
/// privileged one, and the broker's place in it is known.
///
/// # Errors
///
/// Returns what was wrong.
pub fn identity_is_visible<B: IsolationBackend>(backend: &B) -> Result<(), String> {
    let identity = backend
        .identity(None, 1)
        .map_err(|error| format!("no default identity: {error}"))?;
    if identity.count != 1 {
        return Err(format!("asked for 1 principal, got {}", identity.count));
    }
    if backend.owner() == 0 && identity.principal == 0 {
        return Err(String::from(
            "a privileged owner's cells run as its own root",
        ));
    }
    backend
        .broker_in_cell(&identity)
        .map_err(|error| format!("the broker's place in the cell is unknown: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CellIdentity, Endpoint, Error, Exit, Exposure, Filter, SuperviseError, Supervised,
    };

    /// A scratch directory with `code`, `work` and `sock` subdirectories.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hatchling-isolation-conformance-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            for sub in ["code", "work", "sock"] {
                std::fs::create_dir_all(dir.join(sub)).unwrap();
            }
            std::fs::write(dir.join("program"), b"").unwrap();
            Self(dir)
        }

        fn canonical(&self, name: &str) -> PathBuf {
            std::fs::canonicalize(self.0.join(name)).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn spec(scratch: &Scratch) -> CellSpec {
        CellSpec {
            identity: CellIdentity::direct(1000, 1),
            program: scratch.0.join("program"),
            argv: Vec::new(),
            env: Vec::new(),
            workdir: None,
            endpoint: Some(Endpoint {
                socket: scratch.0.join("sock").join("broker.sock"),
                dir: scratch.0.join("sock"),
            }),
            connection: false,
            exposures: vec![
                Exposure {
                    source: scratch.0.join("code"),
                    name: String::from("skill"),
                    writable: false,
                    label: "code",
                },
                Exposure {
                    source: scratch.0.join("work"),
                    name: String::from("work"),
                    writable: true,
                    label: "work",
                },
            ],
            tools: Vec::new(),
            relay: None,
            filter: Filter::WorkTree,
            limits: crate::Limits::default(),
            cgroup: None,
        }
    }

    fn entry(source: PathBuf, name: &str, dir: bool, writable: bool, origin: Origin) -> ViewEntry {
        ViewEntry {
            source,
            name: String::from(name),
            dir,
            writable,
            origin,
        }
    }

    /// The faithful view of [`spec`], with one read-only library.
    fn faithful(scratch: &Scratch) -> CellView {
        CellView {
            relay: None,
            processes: false,
            entries: vec![
                entry(
                    scratch.canonical("sock"),
                    "sock",
                    true,
                    false,
                    Origin::Endpoint,
                ),
                entry(
                    scratch.canonical("program"),
                    "agent",
                    false,
                    false,
                    Origin::Program,
                ),
                entry(
                    scratch.canonical("code"),
                    "skill",
                    true,
                    false,
                    Origin::Granted,
                ),
                entry(
                    scratch.canonical("work"),
                    "work",
                    true,
                    true,
                    Origin::Granted,
                ),
                entry(
                    PathBuf::from("/lib/x.so"),
                    "lib/x.so",
                    false,
                    false,
                    Origin::Runtime,
                ),
            ],
        }
    }

    /// A backend that reports a fixed view and runs nothing: what a
    /// port under test would look like to these checks.
    #[derive(Debug)]
    struct Fake {
        view: Option<CellView>,
        owner: u32,
        principal: u32,
        code: i32,
    }

    #[derive(Debug)]
    struct FakeCell(i32);

    impl Cell for FakeCell {
        fn pid(&self) -> i32 {
            2
        }

        fn wait(self) -> Result<Exit, Error> {
            Ok(Exit {
                code: self.0,
                status: 0,
            })
        }
        fn relay(&mut self) -> Option<std::net::TcpListener> {
            None
        }

        fn connection(&mut self) -> Option<std::os::unix::net::UnixStream> {
            None
        }
    }

    #[derive(Debug)]
    struct FakeProcess;

    impl Supervised for FakeProcess {
        fn try_wait(&self) -> Result<Option<Exit>, Error> {
            Ok(None)
        }

        fn wait_deadline(&self, _: std::time::Duration) -> Result<Exit, Error> {
            Ok(Exit { code: 0, status: 0 })
        }

        fn terminate(&self) -> Result<(), Error> {
            Ok(())
        }
    }

    fn refused() -> Error {
        Error::Spawn {
            reason: String::from("refused"),
        }
    }

    impl IsolationBackend for Fake {
        type Cell = FakeCell;
        type Process = FakeProcess;

        fn owner(&self) -> u32 {
            self.owner
        }

        fn identity(&self, _: Option<u32>, count: u32) -> Result<CellIdentity, Error> {
            Ok(CellIdentity::direct(self.principal, count))
        }

        fn broker_in_cell(&self, _: &CellIdentity) -> Result<u32, Error> {
            Ok(0)
        }

        fn view(&self, _: &CellSpec) -> Result<CellView, Error> {
            self.view.clone().ok_or_else(refused)
        }

        fn isolate(&self, spec: &CellSpec) -> Result<FakeCell, Error> {
            // The "program" is its own exit code, bent by `code`.
            let wanted = i32::from(spec.program.ends_with("fails"));
            Ok(FakeCell(wanted + self.code))
        }

        fn supervise(&self, _: &mut std::process::Child) -> Result<FakeProcess, SuperviseError> {
            Ok(FakeProcess)
        }
    }

    const fn fake(view: Option<CellView>) -> Fake {
        Fake {
            view,
            owner: 1000,
            principal: 1000,
            code: 0,
        }
    }

    fn verdict(scratch: &Scratch, bend: impl FnOnce(&mut CellView)) -> Result<(), String> {
        let mut view = faithful(scratch);
        bend(&mut view);
        grants_nothing_unasked(&fake(Some(view)), &spec(scratch))
    }

    #[test]
    fn a_faithful_view_passes() {
        let scratch = Scratch::new("faithful");
        verdict(&scratch, |_| {}).unwrap();
        // Without an endpoint, the program comes first.
        let mut no_endpoint = spec(&scratch);
        no_endpoint.endpoint = None;
        let mut view = faithful(&scratch);
        view.entries.remove(0);
        check_view(&no_endpoint, &view).unwrap();
    }

    #[test]
    fn an_entry_is_granted_only_as_granted() {
        let scratch = Scratch::new("entry");
        let code = entry(
            scratch.canonical("code"),
            "skill",
            true,
            false,
            Origin::Granted,
        );
        let source = scratch.0.join("code");
        let work = scratch.0.join("work");
        entry_is_granted(&code, Origin::Granted, &source, "skill", true, false).unwrap();
        for (origin, source, name, dir, writable) in [
            (Origin::Runtime, &source, "skill", true, false),
            (Origin::Granted, &source, "other", true, false),
            (Origin::Granted, &work, "skill", true, false),
            (Origin::Granted, &source, "skill", false, false),
            (Origin::Granted, &source, "skill", true, true),
        ] {
            assert!(
                entry_is_granted(&code, origin, source, name, dir, writable).is_err(),
                "{origin:?} {name} {dir} {writable}"
            );
        }
    }

    #[test]
    fn a_backend_that_widens_or_hides_a_grant_fails() {
        type Bend = Box<dyn FnOnce(&mut CellView)>;
        let scratch = Scratch::new("widen");
        let cases: [(&str, Bend); 11] = [
            (
                "a writable read-only grant",
                Box::new(|view| set(view, 2, |e| e.writable = true)),
            ),
            (
                "a read-only writable grant",
                Box::new(|view| set(view, 3, |e| e.writable = false)),
            ),
            (
                "a writable endpoint",
                Box::new(|view| set(view, 0, |e| e.writable = true)),
            ),
            (
                "a writable program",
                Box::new(|view| set(view, 1, |e| e.writable = true)),
            ),
            (
                "another source",
                Box::new(|view| set(view, 2, |e| e.source = PathBuf::from("/etc"))),
            ),
            (
                "another name",
                Box::new(|view| set(view, 2, |e| e.name = String::from("etc"))),
            ),
            (
                "a file for a dir",
                Box::new(|view| set(view, 2, |e| e.dir = false)),
            ),
            (
                "a writable library",
                Box::new(|view| set(view, 4, |e| e.writable = true)),
            ),
            (
                "a library shadowing a grant",
                Box::new(|view| set(view, 4, |e| e.name = String::from("work"))),
            ),
            (
                "an unexplained grant",
                Box::new(|view| set(view, 4, |e| e.origin = Origin::Granted)),
            ),
            ("a missing grant", Box::new(|view| view.entries.truncate(3))),
        ];
        for (what, bend) in cases {
            assert!(verdict(&scratch, bend).is_err(), "{what} must fail");
        }
        assert!(verdict(&scratch, |view| view.entries.clear()).is_err());
        assert!(verdict(&scratch, |view| view.entries.truncate(1)).is_err());
        assert!(
            verdict(&scratch, |view| view.entries.swap(0, 1)).is_err(),
            "a reordered view must fail"
        );
        let refused = grants_nothing_unasked(&fake(None), &spec(&scratch)).unwrap_err();
        assert!(
            refused.starts_with("no view for the spec: refused"),
            "{refused}"
        );
    }

    #[test]
    fn only_a_cli_cell_may_see_a_device() {
        let scratch = Scratch::new("device");
        let refused = verdict(&scratch, |view| {
            view.entries.push(entry(
                PathBuf::from("/dev/null"),
                "dev/null",
                false,
                false,
                Origin::Device,
            ));
        })
        .unwrap_err();
        assert!(
            refused.contains("device dev/null in a cell that is not a CLI's"),
            "{refused}"
        );
        // In a CLI cell a device is seen, but never under another entry's
        // name.
        let mut cli = spec(&scratch);
        cli.filter = Filter::Cli;
        let mut view = faithful(&scratch);
        view.processes = true;
        view.entries.push(entry(
            PathBuf::from("/dev/null"),
            "dev/null",
            false,
            false,
            Origin::Device,
        ));
        check_view(&cli, &view).unwrap();
        view.entries.push(entry(
            PathBuf::from("/dev/zero"),
            "skill",
            false,
            false,
            Origin::Device,
        ));
        assert_eq!(
            check_view(&cli, &view).unwrap_err(),
            "device skill shadows another entry"
        );
    }

    #[test]
    fn a_grant_that_does_not_exist_cannot_be_checked() {
        let scratch = Scratch::new("missing");
        let mut gone = spec(&scratch);
        if let Some(code) = gone.exposures.first_mut() {
            code.source = scratch.0.join("gone");
        }
        let error = check_view(&gone, &faithful(&scratch)).unwrap_err();
        assert!(error.starts_with("cannot resolve "), "{error}");
    }

    fn set(view: &mut CellView, index: usize, bend: impl FnOnce(&mut ViewEntry)) {
        if let Some(entry) = view.entries.get_mut(index) {
            bend(entry);
        }
    }

    fn witnesses() -> Witnesses {
        Witnesses {
            succeeds: PathBuf::from("/w/succeeds"),
            fails: PathBuf::from("/w/fails"),
        }
    }

    #[test]
    fn exits_must_reach_the_owner_unbent() {
        let scratch = Scratch::new("exits");
        exits_reach_the_owner(&fake(None), &spec(&scratch), &witnesses()).unwrap();
        for bend in [1, -1] {
            let bent = Fake {
                code: bend,
                ..fake(None)
            };
            let error = exits_reach_the_owner(&bent, &spec(&scratch), &witnesses()).unwrap_err();
            assert!(error.contains("ended with"), "{error}");
        }
    }

    #[test]
    fn a_cell_that_cannot_be_built_or_waited_on_fails_the_exit_check() {
        // The Linux backend refuses an endpoint whose socket does not
        // exist, before it builds anything: the refusal comes from isolate.
        #[cfg(target_os = "linux")]
        {
            let scratch = Scratch::new("unbuildable");
            let mut unbuildable = spec(&scratch);
            // An identity the backend grants, so the endpoint is what's
            // refused.
            unbuildable.identity =
                IsolationBackend::identity(&crate::linux::Linux, None, 1).unwrap();
            let witnesses = Witnesses {
                succeeds: PathBuf::from("/bin/true"),
                fails: crate::linux::false_witness(),
            };
            let error =
                exits_reach_the_owner(&crate::linux::Linux, &unbuildable, &witnesses).unwrap_err();
            assert!(
                error.starts_with("cannot isolate /bin/true: cannot hand the broker socket "),
                "{error}"
            );
        }
    }

    #[test]
    fn the_cells_identity_must_be_visible_and_unprivileged() {
        identity_is_visible(&fake(None)).unwrap();
        let root_cells = Fake {
            owner: 0,
            principal: 0,
            ..fake(None)
        };
        assert!(identity_is_visible(&root_cells).is_err());
        let root_owner = Fake {
            owner: 0,
            principal: 65534,
            ..fake(None)
        };
        identity_is_visible(&root_owner).unwrap();
        let unprivileged_self = Fake {
            owner: 1000,
            principal: 0,
            ..fake(None)
        };
        identity_is_visible(&unprivileged_self).unwrap();
    }

    #[test]
    fn layout_names_are_relative() {
        assert_eq!(in_root("/sock"), "sock");
    }

    /// The Linux backend passes every check: the backend
    /// hatchling ships today is held to its own contract.
    #[cfg(target_os = "linux")]
    mod linux_backend {
        use super::*;
        use crate::linux::Linux;

        fn linux_spec(scratch: &Scratch) -> CellSpec {
            let mut spec = spec(scratch);
            spec.identity = Linux.identity(None, 1).unwrap();
            spec.program = PathBuf::from("/bin/true");
            spec
        }

        #[test]
        fn the_linux_view_grants_nothing_unasked() {
            let scratch = Scratch::new("linux-view");
            grants_nothing_unasked(&Linux, &linux_spec(&scratch)).unwrap();
        }

        #[test]
        fn the_linux_cli_view_grants_nothing_unasked() {
            // A CLI cell: its tools, its relay, its own processes
            // and its devices are all explained by the spec.
            let scratch = Scratch::new("linux-cli-view");
            let mut spec = linux_spec(&scratch);
            spec.filter = Filter::Cli;
            spec.relay = Some(8787);
            spec.tools = vec![crate::Tool {
                source: PathBuf::from("/bin/sh"),
                name: String::from("bin/sh"),
            }];
            grants_nothing_unasked(&Linux, &spec).unwrap();
        }

        #[test]
        fn the_linux_identity_is_visible() {
            identity_is_visible(&Linux).unwrap();
        }

        #[test]
        fn linux_exits_reach_the_owner() {
            if !crate::linux::cell_available() {
                return;
            }
            let scratch = Scratch::new("linux-exits");
            let mut spec = linux_spec(&scratch);
            spec.endpoint = None;
            spec.filter = Filter::Program;
            let witnesses = Witnesses {
                succeeds: PathBuf::from("/bin/true"),
                fails: crate::linux::false_witness(),
            };
            exits_reach_the_owner(&Linux, &spec, &witnesses).unwrap();
        }
    }
}
