//! Files read out of the Library for the owner (WO-041): the app asks for
//! a file, the broker reads it back whole and to its hash once, and holds
//! that copy while the app asks for its parts in order.
//!
//! What is held is bounded for the whole broker, not a connection: at
//! most [`HELD_MAX`] copies at once, each at most a file's 8 MiB. A copy
//! goes when its last part is read, when its connection begins another or
//! ends, when its file is removed, and once it has gone [`IDLE`] without a
//! part: a sweeper looks every [`SWEEP`]. It is wiped as it goes.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use hatchling_core::ui::FileOutRefusal;
use zeroize::Zeroizing;

/// How long a copy is held without a part.
pub(crate) const IDLE: Duration = Duration::from_secs(60);

/// How often the sweeper lets go of idle copies: one is held at most this
/// past [`IDLE`].
pub(crate) const SWEEP: Duration = Duration::from_secs(5);

/// The most copies held at once, across every connection.
pub(crate) const HELD_MAX: usize = 2;

/// The bytes one part carries: `FILE_IN_PART_BYTES`.
const PART: usize = 32 * 1024;

/// A file being read out, part by part.
struct Held {
    download_id: String,
    file_id: String,
    bytes: Zeroizing<Vec<u8>>,
    next: u32,
    parts: u32,
    touched: Instant,
}

impl Held {
    /// Whether it has gone longer than `idle` without a part, at `now`.
    fn idle(&self, now: Instant, idle: Duration) -> bool {
        now.saturating_duration_since(self.touched) > idle
    }
}

/// The copies held for downloads under way.
pub(crate) struct Holding {
    held: Mutex<Vec<Held>>,
    /// Its sweeper, started with the first copy held.
    sweeper: OnceLock<()>,
    /// How long a copy is held without a part: [`IDLE`].
    idle: Duration,
    /// How often its sweeper looks: [`SWEEP`].
    every: Duration,
}

/// The broker's own.
pub(crate) static HOLDING: Holding = Holding::new();

/// How many parts a file of `bytes` is.
pub(crate) fn parts_of(bytes: usize) -> u32 {
    u32::try_from(bytes.div_ceil(PART)).unwrap_or(u32::MAX)
}

impl Holding {
    /// None held.
    pub(crate) const fn new() -> Self {
        Self::timed(IDLE, SWEEP)
    }

    /// None held: a copy is held `idle` without a part, and the sweeper
    /// looks `every` so often.
    const fn timed(idle: Duration, every: Duration) -> Self {
        Self {
            held: Mutex::new(Vec::new()),
            sweeper: OnceLock::new(),
            idle,
            every,
        }
    }

    fn held(&self) -> MutexGuard<'_, Vec<Held>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether a copy could be held at `now`, once any idle copy is let
    /// go: asked before a file is read, so a busy broker reads nothing.
    pub(crate) fn has_room(&self, now: Instant) -> bool {
        let mut held = self.held();
        held.retain(|copy| !copy.idle(now, self.idle));
        let room = held.len() < HELD_MAX;
        drop(held);
        room
    }

    /// Holds `bytes` of the file `file_id` as `download_id`, at `now`: how
    /// many parts it is. A file of no bytes is no parts, and nothing is
    /// held. When [`HELD_MAX`] others are held and none has gone [`IDLE`],
    /// it is busy.
    pub(crate) fn begin(
        &'static self,
        (download_id, file_id): (&str, &str),
        bytes: Zeroizing<Vec<u8>>,
        now: Instant,
    ) -> Result<u32, FileOutRefusal> {
        let mut held = self.held();
        held.retain(|copy| !copy.idle(now, self.idle));
        let parts = parts_of(bytes.len());
        if parts == 0 {
            return Ok(0);
        }
        if held.len() >= HELD_MAX {
            return Err(FileOutRefusal::Busy);
        }
        held.push(Held {
            download_id: download_id.to_owned(),
            file_id: file_id.to_owned(),
            bytes,
            next: 0,
            parts,
            touched: now,
        });
        drop(held);
        self.sweeper.get_or_init(|| self.start_sweeper());
        Ok(parts)
    }

    /// Starts the thread that lets go of idle copies, every [`SWEEP`].
    fn start_sweeper(&'static self) {
        let started = std::thread::Builder::new()
            .name(String::from("file-out-sweeper"))
            .spawn(move || {
                loop {
                    std::thread::sleep(self.every);
                    self.sweep(Instant::now());
                }
            });
        if let Err(error) = started {
            // Without it, an idle copy goes when the next download begins
            // or its connection ends.
            tracing::warn!(%error, "the downloads' sweeper couldn't be started");
        }
    }

    /// Lets go of every copy idle at `now`.
    pub(crate) fn sweep(&self, now: Instant) {
        self.held().retain(|copy| !copy.idle(now, self.idle));
    }

    /// Part `seq` of `download_id`, in base64, at `now`. The last part lets
    /// the copy go. A part out of order lets it go too, and the download is
    /// begun again; one after the copy went idle, or after its last part,
    /// is a download not known.
    pub(crate) fn part(
        &self,
        download_id: &str,
        seq: u32,
        now: Instant,
    ) -> Result<String, FileOutRefusal> {
        let mut held = self.held();
        held.retain(|copy| !copy.idle(now, self.idle));
        let Some(at) = held.iter().position(|copy| copy.download_id == download_id) else {
            return Err(FileOutRefusal::UnknownDownload);
        };
        let Some(copy) = held.get_mut(at) else {
            return Err(FileOutRefusal::UnknownDownload);
        };
        if seq != copy.next {
            held.remove(at);
            return Err(FileOutRefusal::OutOfStep);
        }
        let from = usize::try_from(seq)
            .unwrap_or(usize::MAX)
            .saturating_mul(PART);
        let to = from.saturating_add(PART).min(copy.bytes.len());
        let data = copy
            .bytes
            .get(from..to)
            .map(|part| base64::engine::general_purpose::STANDARD.encode(part))
            .unwrap_or_default();
        copy.next = seq.saturating_add(1);
        copy.touched = now;
        if copy.next == copy.parts {
            held.remove(at);
        }
        drop(held);
        Ok(data)
    }

    /// Lets go of `download_id`, if it is held.
    pub(crate) fn let_go(&self, download_id: &str) {
        self.held().retain(|copy| copy.download_id != download_id);
    }

    /// Lets go of every copy of the file `file_id`: it was removed.
    pub(crate) fn forget_file(&self, file_id: &str) {
        self.held().retain(|copy| copy.file_id != file_id);
    }

    /// How many copies are held.
    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.held().len()
    }

    /// Whether its sweeper was started.
    #[cfg(test)]
    pub(crate) fn sweeping(&self) -> bool {
        self.sweeper.get().is_some()
    }
}

/// A holding of a test's own, apart from the broker's.
#[cfg(test)]
pub(crate) fn own() -> &'static Holding {
    Box::leak(Box::new(Holding::new()))
}

#[cfg(test)]
mod tests {
    use hatchling_core::ui::FILE_IN_PART_BYTES;

    use super::*;

    fn decoded(data: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap()
    }

    fn bytes(len: usize) -> Zeroizing<Vec<u8>> {
        Zeroizing::new((0..len).map(|at| u8::try_from(at % 251).unwrap()).collect())
    }

    fn begin(
        holding: &'static Holding,
        id: &str,
        len: usize,
        now: Instant,
    ) -> Result<u32, FileOutRefusal> {
        holding.begin((id, "file-1"), bytes(len), now)
    }

    #[test]
    fn a_file_is_read_out_in_order_whole_and_let_go_after_its_last_part() {
        let holding = own();
        let now = Instant::now();
        assert_eq!(begin(holding, "out-1", PART * 2 + 5, now), Ok(3));
        let mut read = Vec::new();
        for seq in 0..3 {
            read.extend(decoded(&holding.part("out-1", seq, now).unwrap()));
        }
        assert_eq!(read, *bytes(PART * 2 + 5));
        assert_eq!(holding.count(), 0, "the copy was kept after its last part");
        assert_eq!(
            holding.part("out-1", 3, now),
            Err(FileOutRefusal::UnknownDownload)
        );
        assert!(holding.sweeping(), "a copy was held with no sweeper");
    }

    #[test]
    fn parts_are_whole_parts_but_the_last() {
        assert_eq!(u32::try_from(PART), Ok(FILE_IN_PART_BYTES));
        assert_eq!(parts_of(0), 0);
        assert_eq!(parts_of(1), 1);
        assert_eq!(parts_of(PART), 1);
        assert_eq!(parts_of(PART + 1), 2);
        assert_eq!(parts_of(8 * 1024 * 1024), 256);
        let holding = own();
        let now = Instant::now();
        assert_eq!(begin(holding, "out-1", PART, now), Ok(1));
        assert_eq!(decoded(&holding.part("out-1", 0, now).unwrap()).len(), PART);
    }

    #[test]
    fn a_file_of_no_bytes_is_no_parts_and_holds_nothing() {
        let holding = own();
        assert_eq!(begin(holding, "out-1", 0, Instant::now()), Ok(0));
        assert_eq!(holding.count(), 0);
        assert!(!holding.sweeping());
    }

    #[test]
    fn a_part_out_of_order_lets_the_copy_go() {
        let now = Instant::now();
        for (seq, before) in [(1, 0), (0, 1), (2, 1)] {
            let holding = own();
            begin(holding, "out-1", PART * 3, now).unwrap();
            for asked in 0..before {
                holding.part("out-1", asked, now).unwrap();
            }
            assert_eq!(
                holding.part("out-1", seq, now),
                Err(FileOutRefusal::OutOfStep),
                "{seq} after {before}"
            );
            assert_eq!(holding.count(), 0, "{seq} after {before}");
        }
        let holding = own();
        begin(holding, "out-1", 3, now).unwrap();
        assert_eq!(
            holding.part("out-2", 0, now),
            Err(FileOutRefusal::UnknownDownload)
        );
        assert_eq!(holding.count(), 1, "another download's id let this one go");
    }

    #[test]
    fn so_many_copies_are_held_at_once_and_an_idle_one_makes_room() {
        let holding = own();
        let now = Instant::now();
        for at in 0..HELD_MAX {
            begin(holding, &format!("out-{at}"), 3, now).unwrap();
        }
        assert!(!holding.has_room(now));
        assert_eq!(begin(holding, "out-x", 3, now), Err(FileOutRefusal::Busy));
        // Just at the bound it is still held.
        assert_eq!(
            begin(holding, "out-x", 3, now + IDLE),
            Err(FileOutRefusal::Busy)
        );
        let later = now + IDLE + Duration::from_secs(1);
        assert!(holding.has_room(later));
        assert_eq!(begin(holding, "out-x", 3, later), Ok(1));
        assert_eq!(holding.count(), 1, "the idle copies were kept");
    }

    #[test]
    fn a_part_keeps_a_copy_from_going_idle_and_an_idle_one_is_let_go() {
        let holding = own();
        let now = Instant::now();
        begin(holding, "out-1", PART * 3, now).unwrap();
        let soon = now + IDLE;
        holding.part("out-1", 0, soon).unwrap();
        let after = soon + IDLE;
        holding.part("out-1", 1, after).unwrap();
        let late = after + IDLE + Duration::from_secs(1);
        assert_eq!(
            holding.part("out-1", 2, late),
            Err(FileOutRefusal::UnknownDownload)
        );
        assert_eq!(holding.count(), 0);
    }

    /// The sweeper itself lets go of a copy left idle, with no other call.
    #[test]
    fn the_sweeper_lets_go_of_a_copy_left_idle() {
        let holding: &'static Holding = Box::leak(Box::new(Holding::timed(
            Duration::from_millis(20),
            Duration::from_millis(10),
        )));
        holding
            .begin(("out-1", "file-1"), bytes(3), Instant::now())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while holding.count() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(holding.count(), 0, "the sweeper didn't let the copy go");
    }

    #[test]
    fn a_sweep_lets_go_of_idle_copies_alone() {
        let holding = own();
        let now = Instant::now();
        begin(holding, "out-1", 3, now).unwrap();
        begin(holding, "out-2", 3, now + Duration::from_secs(30)).unwrap();
        holding.sweep(now + IDLE);
        assert_eq!(holding.count(), 2, "a copy at the bound was let go");
        holding.sweep(now + IDLE + Duration::from_secs(1));
        assert_eq!(holding.count(), 1);
        assert_eq!(
            holding.part("out-2", 0, now + IDLE + Duration::from_secs(1)),
            Ok(base64::engine::general_purpose::STANDARD.encode(&bytes(3)[..]))
        );
    }

    #[test]
    fn a_download_let_go_or_its_file_removed_is_let_go() {
        let holding = own();
        let now = Instant::now();
        begin(holding, "out-1", 3, now).unwrap();
        holding.let_go("out-1");
        assert_eq!(holding.count(), 0);
        // A file removed is let go, whoever was reading it.
        holding.begin(("out-5", "gone"), bytes(3), now).unwrap();
        holding.begin(("out-6", "kept"), bytes(3), now).unwrap();
        holding.forget_file("gone");
        assert_eq!(
            holding.part("out-5", 0, now),
            Err(FileOutRefusal::UnknownDownload)
        );
        assert!(holding.part("out-6", 0, now).is_ok());
    }
}
