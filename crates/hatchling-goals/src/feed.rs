//! The feed's query interface: one read-only path over goals,
//! tracking, and memory, where every item carries its provenance.
//!
//! 's feed reads; it never acts. [`FeedReader`] is that rule as
//! a type: its public surface is [`open`](FeedReader::open) and
//! [`query`](FeedReader::query), it holds no [`GoalsStore`] and
//! reaches the store only through the read path, and the read path
//! validates directories without creating or repairing them. A feed
//! holding a `FeedReader` has no write op to call.
//!
//! Provenance is attached here, at the store layer, never
//! reconstructed by the feed. It is derived from the record itself —
//! which store it came from and what that record says about its own
//! origin — so an item cannot be labeled with a source it did not come
//! from. Labels are transitive where the record allows: a tracking
//! entry whose source is `whatsapp` reads "from tracking (originally
//! from whatsapp)", not just "from tracking". Memory items
//! record no origin, so they say so explicitly rather than guess.
//!
//! [`GoalsStore`]: crate::GoalsStore

use std::path::Path;

use hatchling_memory::{MemoryId, MemoryItem, MemoryPrincipal, MemoryScope, MemoryStore, Tag};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::backing::{Files, GoalsSource};
use crate::name::{GoalId, SourceLabel};
use crate::read;
use crate::record::{Goal, GoalOrigin, TimeRange, TrackingEntry};

/// The shared page budget and page type: the feed pages through the
/// same pager as the memory replies (`hatchling_memory::paging`, whose
/// docs carry the budget arithmetic). Goal and tracking bounds keep any
/// single record well under the budget even when every byte needs
/// escaping.
pub use hatchling_memory::{PAGE_BUDGET_BYTES, Page as FeedPage};

/// Which store an item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceStore {
    /// The memory store.
    #[serde(rename = "memory")]
    Memory,
    /// The tracking log.
    #[serde(rename = "tracking")]
    Tracking,
    /// The goals store.
    #[serde(rename = "goal")]
    Goal,
}

impl SourceStore {
    /// The store's name as the feed shows it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Tracking => "tracking",
            Self::Goal => "goal",
        }
    }
}

/// Where the content of an item originally came from, one hop behind
/// the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// The user explicitly created it.
    #[serde(rename = "user-explicit")]
    UserExplicit,
    /// Reported from `source`, as stated when it was recorded.
    #[serde(rename = "reported")]
    Reported {
        /// The stated source.
        source: SourceLabel,
    },
    /// The record carries no origin. Said explicitly: an unlabeled
    /// hop is shown as unknown, never guessed.
    #[serde(rename = "not-recorded")]
    NotRecorded,
}

/// An item's provenance: the store it came from and, behind that, its
/// original source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The store the item was read from.
    pub store: SourceStore,
    /// Where its content originally came from.
    pub origin: Origin,
}

impl Provenance {
    /// The transitive label the feed shows, e.g. `from tracking
    /// (originally from whatsapp)`.
    #[must_use]
    pub fn label(&self) -> String {
        let origin = match &self.origin {
            Origin::UserExplicit => String::from("created by the user"),
            Origin::Reported { source } => format!("originally from {source}"),
            Origin::NotRecorded => String::from("origin not recorded"),
        };
        format!("from {} ({origin})", self.store.as_str())
    }
}

/// The record an item carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeedBody {
    /// A goal.
    #[serde(rename = "goal")]
    Goal(Goal),
    /// A tracking entry.
    #[serde(rename = "tracking")]
    Tracking(TrackingEntry),
    /// A memory item, recalled by tag, as a run is given it
    /// ([`MemoryItem::shareable`]).
    #[serde(rename = "memory")]
    Memory(Box<MemoryItem>),
}

/// One query result: a record plus the provenance derived from it.
///
/// The record is private and the provenance is computed, never stored,
/// so no item can carry a label its record does not support. On the
/// wire an item states its provenance and label next to the record;
/// [`from_json`](Self::from_json) recomputes both and refuses an item
/// whose stated provenance disagrees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedItem {
    body: FeedBody,
}

/// The wire form of a [`FeedItem`].
#[derive(Serialize, Deserialize)]
struct WireItem {
    provenance: Provenance,
    label: String,
    item: FeedBody,
}

impl FeedItem {
    /// The record.
    #[must_use]
    pub const fn body(&self) -> &FeedBody {
        &self.body
    }

    /// The provenance, derived from the record.
    #[must_use]
    pub fn provenance(&self) -> Provenance {
        match &self.body {
            FeedBody::Goal(goal) => Provenance {
                store: SourceStore::Goal,
                origin: match goal.origin {
                    GoalOrigin::UserExplicit => Origin::UserExplicit,
                },
            },
            FeedBody::Tracking(entry) => Provenance {
                store: SourceStore::Tracking,
                origin: Origin::Reported {
                    source: entry.source.clone(),
                },
            },
            FeedBody::Memory(_) => Provenance {
                store: SourceStore::Memory,
                origin: Origin::NotRecorded,
            },
        }
    }

    /// The resume point after this item, for the query it came from:
    /// the goal id, the tracking sequence number, or the memory id.
    #[must_use]
    pub fn cursor(&self) -> String {
        match &self.body {
            FeedBody::Goal(goal) => goal.id.to_string(),
            FeedBody::Tracking(entry) => entry.seq.to_string(),
            FeedBody::Memory(item) => item.id.to_string(),
        }
    }

    /// The wire form: provenance, label, and record.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] if the record cannot be encoded.
    pub fn to_json(&self) -> Result<String, Error> {
        let provenance = self.provenance();
        let wire = WireItem {
            label: provenance.label(),
            provenance,
            item: self.body.clone(),
        };
        serde_json::to_string(&wire).map_err(|_| Error::Corrupt)
    }

    /// Parses the wire form, refusing an item whose stated provenance
    /// or label is not the one its record implies.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] for undecodable JSON, or
    /// [`Error::ProvenanceMismatch`] for a mislabeled item.
    pub fn from_json(raw: &str) -> Result<Self, Error> {
        let wire: WireItem = serde_json::from_str(raw).map_err(|_| Error::Corrupt)?;
        let item = Self { body: wire.item };
        let provenance = item.provenance();
        if wire.label != provenance.label() || wire.provenance != provenance {
            return Err(Error::ProvenanceMismatch);
        }
        Ok(item)
    }
}

/// Which goals a goal query returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    /// Open goals only.
    Open,
    /// Closed goals only.
    Closed,
    /// Every goal.
    Any,
}

impl StatusFilter {
    const fn admits(self, goal: &Goal) -> bool {
        match self {
            Self::Open => goal.is_open(),
            Self::Closed => !goal.is_open(),
            Self::Any => true,
        }
    }
}

impl std::str::FromStr for StatusFilter {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "open" => Ok(Self::Open),
            "closed" => Ok(Self::Closed),
            "any" => Ok(Self::Any),
            other => Err(Error::InvalidName(format!(
                "goal status filter must be 'open', 'closed', or 'any', got {other:?}"
            ))),
        }
    }
}

/// One read-scoped query. Each selects one store; every result is a
/// [`FeedItem`], whichever store answered.
///
/// `after` / `after_seq` resume a paged query past the cursor of the
/// last item received ([`FeedPage::next`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedQuery {
    /// The user's goals, ordered by id.
    Goals {
        /// Which goals.
        status: StatusFilter,
        /// Only goals changed at or after this time ("what changed
        /// since the last digest").
        updated_since_secs: Option<u64>,
        /// Only goals whose id sorts after this one.
        after: Option<GoalId>,
    },
    /// The user's tracking entries, in log order.
    Tracking {
        /// Only entries recorded inside this window.
        range: TimeRange,
        /// Only entries reported from this source.
        source: Option<SourceLabel>,
        /// Only entries after this sequence number.
        after_seq: Option<u64>,
    },
    /// The user's memory items carrying `tag`, ordered by id. Tag
    /// scoped like every memory read: this is recall, not a dump.
    Memory {
        /// The tag to recall.
        tag: Tag,
        /// Only items whose id sorts after this one.
        after: Option<MemoryId>,
    },
}

/// The feed's read-only handle on the stores.
///
/// Built per query by the broker. It holds the goals root (never a
/// [`GoalsStore`](crate::GoalsStore)) and, when memory is granted, the
/// memory store it recalls from.
#[derive(Debug)]
pub struct FeedReader {
    source: Box<dyn GoalsSource>,
    memory: Option<MemoryStore>,
}

impl FeedReader {
    /// A reader over the goals store at `goals_root` and, optionally,
    /// a memory store for memory queries.
    ///
    /// Opening touches nothing: each query validates the directories
    /// it reads (a real `0700` directory owned by the current uid, at
    /// every level) and treats an absent one as empty.
    #[must_use]
    pub fn open(goals_root: &Path, memory: Option<MemoryStore>) -> Self {
        Self::open_as(goals_root, rustix::process::getuid().as_raw(), memory)
    }

    /// A reader over the goals `source` holds and, optionally, a memory
    /// store for memory queries. A source reads, and nothing more.
    #[must_use]
    pub fn with_source(source: Box<dyn GoalsSource>, memory: Option<MemoryStore>) -> Self {
        Self { source, memory }
    }

    /// Split out so tests exercise the ownership check hermetically.
    fn open_as(goals_root: &Path, owner: u32, memory: Option<MemoryStore>) -> Self {
        Self::with_source(Box::new(Files::new(goals_root, owner)), memory)
    }

    /// Runs one query for the principal, returning every matching item
    /// in the query's order. Page the result with [`page`] to fit it
    /// into a session frame.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Store`] for an insecure or unreadable
    /// directory, [`Error::Corrupt`] for a damaged record,
    /// [`Error::Io`] on filesystem failure, or
    /// [`Error::MemoryUnavailable`] for a memory query on a reader
    /// holding no memory store.
    pub fn query(
        &self,
        principal: &MemoryPrincipal,
        query: &FeedQuery,
        now_secs: u64,
    ) -> Result<Vec<FeedItem>, Error> {
        let uid = principal.peer_uid;
        match query {
            FeedQuery::Goals {
                status,
                updated_since_secs,
                after,
            } => Ok(read::goals(self.source.as_ref(), uid)?
                .into_iter()
                .filter(|goal| status.admits(goal))
                .filter(|goal| updated_since_secs.is_none_or(|since| goal.updated_at_secs >= since))
                .filter(|goal| after.as_ref().is_none_or(|after| goal.id > *after))
                .map(|goal| FeedItem {
                    body: FeedBody::Goal(goal),
                })
                .collect()),
            FeedQuery::Tracking {
                range,
                source,
                after_seq,
            } => {
                let entries = read::tracking(self.source.as_ref(), uid, |entry| {
                    range.contains(entry.at_secs)
                        && source.as_ref().is_none_or(|source| entry.source == *source)
                        && after_seq.is_none_or(|after| entry.seq > after)
                })?;
                Ok(entries
                    .into_iter()
                    .map(|entry| FeedItem {
                        body: FeedBody::Tracking(entry),
                    })
                    .collect())
            }
            FeedQuery::Memory { tag, after } => {
                let Some(memory) = &self.memory else {
                    return Err(Error::MemoryUnavailable);
                };
                Ok(memory
                    .recall_by_tag(principal, &MemoryScope::User, tag, now_secs)?
                    .into_iter()
                    .filter(|item| {
                        after
                            .as_ref()
                            .is_none_or(|after| item.id.as_str() > after.as_str())
                    })
                    .map(|item| FeedItem {
                        body: FeedBody::Memory(Box::new(item.shareable())),
                    })
                    .collect())
            }
        }
    }
}

/// Packs `items` into one page of at most `budget_bytes`, counting each
/// item at its size once encoded inside the reply.
///
/// The shared pager ([`hatchling_memory::page`]) over each item's wire
/// form ([`FeedItem::to_json`]) and cursor ([`FeedItem::cursor`]). The
/// first item is always included, so a page always makes progress; a
/// memory item can exceed the budget and then travels alone, bounded by
/// its encoded size (`hatchling_memory::MAX_ENCODED_ITEM_BYTES`), so
/// even alone it fits a frame.
///
/// # Errors
///
/// Returns [`Error::Corrupt`] if an item cannot be encoded.
pub fn page(items: &[FeedItem], budget_bytes: usize) -> Result<FeedPage, Error> {
    hatchling_memory::page(
        items
            .iter()
            .map(|item| Ok((item.to_json()?, item.cursor()))),
        budget_bytes,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::PathBuf;
    use std::str::FromStr as _;

    use hatchling_memory::{RetentionPolicy, SessionId};

    use super::*;
    use crate::read::GOALS_DIR;
    use crate::record::{
        GoalOutcome, GoalStatus, MAX_DETAIL_BYTES, MAX_TITLE_BYTES, MAX_TRACKING_TEXT_BYTES,
    };
    use crate::store::{GoalEdit, GoalsStore};

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-goals-feed-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn principal(uid: u32) -> MemoryPrincipal {
        MemoryPrincipal {
            peer_uid: uid,
            session_id: SessionId::from_str("sess-feed").unwrap(),
        }
    }

    fn label(raw: &str) -> SourceLabel {
        raw.parse().unwrap()
    }

    const fn all_goals() -> FeedQuery {
        FeedQuery::Goals {
            status: StatusFilter::Any,
            updated_since_secs: None,
            after: None,
        }
    }

    const fn all_tracking() -> FeedQuery {
        FeedQuery::Tracking {
            range: TimeRange::ALL,
            source: None,
            after_seq: None,
        }
    }

    fn memory_query(tag: &str) -> FeedQuery {
        FeedQuery::Memory {
            tag: tag.parse().unwrap(),
            after: None,
        }
    }

    /// A goals root and a memory root holding one of everything for
    /// user 7: an open goal, a closed goal, two tracking entries from
    /// two sources, and a tagged memory item.
    struct Fixture {
        goals_root: PathBuf,
        memory_root: PathBuf,
        open: Goal,
        closed: Goal,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let goals_root = test_root(&format!("{name}-goals"));
            let memory_root = test_root(&format!("{name}-memory"));
            let store = GoalsStore::open(&goals_root).unwrap();
            let p = principal(7);
            let open = store.create_goal(&p, "run a 5k", "by spring", 100).unwrap();
            let closed = store.create_goal(&p, "learn chess", "", 110).unwrap();
            let crate::store::GoalChange::Changed(closed) = store
                .close_goal(&p, &closed.id, GoalOutcome::Abandoned, 120)
                .unwrap()
            else {
                unreachable!("the goal exists and is open");
            };
            store
                .append_tracking(&p, &label("user"), "ran 3k", 200)
                .unwrap();
            store
                .append_tracking(&p, &label("whatsapp"), "friend: see you at the race", 210)
                .unwrap();
            let memory = MemoryStore::open(&memory_root).unwrap();
            memory
                .write(
                    &p,
                    &MemoryScope::User,
                    &"pref-1".parse().unwrap(),
                    "prefers mornings",
                    &["feed".parse().unwrap()],
                    RetentionPolicy::Keep,
                    50,
                )
                .unwrap();
            Self {
                goals_root,
                memory_root,
                open,
                closed,
            }
        }

        fn reader(&self) -> FeedReader {
            FeedReader::open(
                &self.goals_root,
                Some(MemoryStore::open(&self.memory_root).unwrap()),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.goals_root);
            let _ = std::fs::remove_dir_all(&self.memory_root);
        }
    }

    fn goals(items: &[FeedItem]) -> Vec<Goal> {
        items
            .iter()
            .filter_map(|item| match item.body() {
                FeedBody::Goal(goal) => Some(goal.clone()),
                FeedBody::Tracking(_) | FeedBody::Memory(_) => None,
            })
            .collect()
    }

    fn seqs(items: &[FeedItem]) -> Vec<u64> {
        items
            .iter()
            .filter_map(|item| match item.body() {
                FeedBody::Tracking(entry) => Some(entry.seq),
                FeedBody::Goal(_) | FeedBody::Memory(_) => None,
            })
            .collect()
    }

    fn sorted(mut goals: Vec<Goal>) -> Vec<Goal> {
        goals.sort_by(|a, b| a.id.cmp(&b.id));
        goals
    }

    #[test]
    fn one_query_path_labels_every_item_from_every_store() {
        // Acceptance: memory, tracking, and goal items all come back
        // through `query`, and each carries its source from the store
        // layer — the feed's provenance rule is satisfiable from these
        // results alone.
        let fx = Fixture::new("labels");
        let reader = fx.reader();
        let p = principal(7);
        let mut labels = Vec::new();
        for query in [all_goals(), all_tracking(), memory_query("feed")] {
            for item in reader.query(&p, &query, 300).unwrap() {
                let json = item.to_json().unwrap();
                let wire: serde_json::Value = serde_json::from_str(&json).unwrap();
                let provenance = item.provenance();
                let stated_store = wire
                    .get("provenance")
                    .and_then(|p| p.get("store"))
                    .and_then(serde_json::Value::as_str);
                assert_eq!(stated_store, Some(provenance.store.as_str()));
                let stated_label = wire.get("label").and_then(serde_json::Value::as_str);
                assert_eq!(stated_label, Some(provenance.label().as_str()));
                assert_eq!(FeedItem::from_json(&json).unwrap(), item);
                labels.push(provenance.label());
            }
        }
        labels.sort();
        assert_eq!(
            labels,
            vec![
                "from goal (created by the user)",
                "from goal (created by the user)",
                "from memory (origin not recorded)",
                "from tracking (originally from user)",
                "from tracking (originally from whatsapp)",
            ]
        );
    }

    #[test]
    fn provenance_is_derived_from_the_record() {
        let fx = Fixture::new("derived");
        let reader = fx.reader();
        let p = principal(7);
        let tracking = reader.query(&p, &all_tracking(), 300).unwrap();
        let whatsapp = tracking.get(1).unwrap();
        assert_eq!(
            whatsapp.provenance(),
            Provenance {
                store: SourceStore::Tracking,
                origin: Origin::Reported {
                    source: label("whatsapp")
                },
            }
        );
        let memory = reader.query(&p, &memory_query("feed"), 300).unwrap();
        assert_eq!(
            memory.first().unwrap().provenance(),
            Provenance {
                store: SourceStore::Memory,
                origin: Origin::NotRecorded,
            }
        );
        let goal = reader.query(&p, &all_goals(), 300).unwrap();
        assert_eq!(
            goal.first().unwrap().provenance(),
            Provenance {
                store: SourceStore::Goal,
                origin: Origin::UserExplicit,
            }
        );
    }

    #[test]
    fn a_mislabeled_item_is_refused_on_parse() {
        let fx = Fixture::new("mislabel");
        let reader = fx.reader();
        let items = reader.query(&principal(7), &all_tracking(), 300).unwrap();
        let json = items.get(1).unwrap().to_json().unwrap();
        // Launder the WhatsApp entry: claim the user typed it.
        let laundered_label = json.replace(
            "from tracking (originally from whatsapp)",
            "from tracking (originally from user)",
        );
        assert_ne!(laundered_label, json);
        assert!(matches!(
            FeedItem::from_json(&laundered_label),
            Err(Error::ProvenanceMismatch)
        ));
        // Or keep the label and change the structured origin.
        let laundered_origin = json.replace(
            r#"{"reported":{"source":"whatsapp"}}"#,
            r#""user-explicit""#,
        );
        assert_ne!(laundered_origin, json);
        assert!(matches!(
            FeedItem::from_json(&laundered_origin),
            Err(Error::ProvenanceMismatch)
        ));
        // Or claim another store.
        let laundered_store = json.replace(r#""store":"tracking""#, r#""store":"goal""#);
        assert_ne!(laundered_store, json);
        assert!(matches!(
            FeedItem::from_json(&laundered_store),
            Err(Error::ProvenanceMismatch)
        ));
        assert!(matches!(FeedItem::from_json("{"), Err(Error::Corrupt)));
    }

    #[test]
    fn goal_queries_filter_by_status_change_time_and_cursor() {
        let fx = Fixture::new("goal-filters");
        let reader = fx.reader();
        let p = principal(7);
        let query = |status, updated_since_secs, after: Option<&GoalId>| FeedQuery::Goals {
            status,
            updated_since_secs,
            after: after.cloned(),
        };
        let open = goals(
            &reader
                .query(&p, &query(StatusFilter::Open, None, None), 0)
                .unwrap(),
        );
        assert_eq!(open, vec![fx.open.clone()]);
        let closed = goals(
            &reader
                .query(&p, &query(StatusFilter::Closed, None, None), 0)
                .unwrap(),
        );
        assert_eq!(closed, vec![fx.closed.clone()]);
        let any = goals(
            &reader
                .query(&p, &query(StatusFilter::Any, None, None), 0)
                .unwrap(),
        );
        assert_eq!(any, sorted(vec![fx.open.clone(), fx.closed.clone()]));
        // Changed since: the open goal was last touched at 100, the
        // closed one at 120; the bound is inclusive.
        let since = goals(
            &reader
                .query(&p, &query(StatusFilter::Any, Some(120), None), 0)
                .unwrap(),
        );
        assert_eq!(since, vec![fx.closed.clone()]);
        let since = goals(
            &reader
                .query(&p, &query(StatusFilter::Any, Some(121), None), 0)
                .unwrap(),
        );
        assert!(since.is_empty());
        // The cursor is exclusive.
        let first = any.first().unwrap();
        let rest = goals(
            &reader
                .query(&p, &query(StatusFilter::Any, None, Some(&first.id)), 0)
                .unwrap(),
        );
        assert_eq!(rest, any.get(1..).unwrap().to_vec());
    }

    #[test]
    fn tracking_queries_filter_by_range_source_and_cursor() {
        let fx = Fixture::new("tracking-filters");
        let reader = fx.reader();
        let p = principal(7);
        let query = |since, until, source: Option<&str>, after_seq| FeedQuery::Tracking {
            range: TimeRange::new(since, until).unwrap(),
            source: source.map(label),
            after_seq,
        };
        // Entries sit at 200 (user) and 210 (whatsapp).
        assert_eq!(
            seqs(&reader.query(&p, &query(None, None, None, None), 0).unwrap()),
            vec![0, 1]
        );
        assert_eq!(
            seqs(
                &reader
                    .query(&p, &query(Some(200), None, None, None), 0)
                    .unwrap()
            ),
            vec![0, 1]
        );
        assert_eq!(
            seqs(
                &reader
                    .query(&p, &query(Some(201), None, None, None), 0)
                    .unwrap()
            ),
            vec![1]
        );
        assert_eq!(
            seqs(
                &reader
                    .query(&p, &query(None, Some(210), None, None), 0)
                    .unwrap()
            ),
            vec![0]
        );
        assert_eq!(
            seqs(
                &reader
                    .query(&p, &query(None, Some(211), None, None), 0)
                    .unwrap()
            ),
            vec![0, 1]
        );
        assert_eq!(
            seqs(
                &reader
                    .query(&p, &query(None, None, Some("whatsapp"), None), 0)
                    .unwrap()
            ),
            vec![1]
        );
        assert_eq!(
            seqs(
                &reader
                    .query(&p, &query(None, None, Some("user"), None), 0)
                    .unwrap()
            ),
            vec![0]
        );
        assert!(
            seqs(
                &reader
                    .query(&p, &query(None, None, Some("sms"), None), 0)
                    .unwrap()
            )
            .is_empty()
        );
        assert_eq!(
            seqs(
                &reader
                    .query(&p, &query(None, None, None, Some(0)), 0)
                    .unwrap()
            ),
            vec![1]
        );
        assert!(
            seqs(
                &reader
                    .query(&p, &query(None, None, None, Some(1)), 0)
                    .unwrap()
            )
            .is_empty()
        );
    }

    #[test]
    fn memory_queries_are_tag_scoped_and_paged_by_id() {
        let fx = Fixture::new("memory");
        let memory = MemoryStore::open(&fx.memory_root).unwrap();
        let p = principal(7);
        memory
            .write(
                &p,
                &MemoryScope::User,
                &"pref-2".parse().unwrap(),
                "likes trails",
                &["feed".parse().unwrap()],
                RetentionPolicy::Keep,
                60,
            )
            .unwrap();
        memory
            .write(
                &p,
                &MemoryScope::User,
                &"secret-1".parse().unwrap(),
                "not for the feed",
                &["private".parse().unwrap()],
                RetentionPolicy::Keep,
                60,
            )
            .unwrap();
        let reader = fx.reader();
        let ids = |query: &FeedQuery| -> Vec<String> {
            reader
                .query(&p, query, 300)
                .unwrap()
                .iter()
                .map(FeedItem::cursor)
                .collect()
        };
        assert_eq!(ids(&memory_query("feed")), vec!["pref-1", "pref-2"]);
        let after = FeedQuery::Memory {
            tag: "feed".parse().unwrap(),
            after: Some("pref-1".parse().unwrap()),
        };
        assert_eq!(ids(&after), vec!["pref-2"]);
        // Untagged items never reach the feed.
        assert!(ids(&memory_query("nothing")).is_empty());
    }

    #[test]
    fn a_reader_without_memory_refuses_memory_queries() {
        let fx = Fixture::new("no-memory");
        let reader = FeedReader::open(&fx.goals_root, None);
        assert!(matches!(
            reader.query(&principal(7), &memory_query("feed"), 0),
            Err(Error::MemoryUnavailable)
        ));
        // Goals and tracking still work.
        assert_eq!(
            reader.query(&principal(7), &all_goals(), 0).unwrap().len(),
            2
        );
    }

    #[test]
    fn one_principal_cannot_read_anothers_goals_or_tracking() {
        let fx = Fixture::new("isolation");
        let reader = fx.reader();
        let other = principal(8);
        assert!(reader.query(&other, &all_goals(), 0).unwrap().is_empty());
        assert!(reader.query(&other, &all_tracking(), 0).unwrap().is_empty());
        assert!(
            reader
                .query(&other, &memory_query("feed"), 0)
                .unwrap()
                .is_empty()
        );
        // Even naming user 7's goal id as the cursor reveals nothing.
        let after = FeedQuery::Goals {
            status: StatusFilter::Any,
            updated_since_secs: None,
            after: Some(fx.open.id.clone()),
        };
        assert!(reader.query(&other, &after, 0).unwrap().is_empty());
    }

    #[test]
    fn a_fresh_store_reads_as_empty() {
        let root = test_root("fresh");
        let reader = FeedReader::open(&root, None);
        assert!(
            reader
                .query(&principal(7), &all_goals(), 0)
                .unwrap()
                .is_empty()
        );
        assert!(
            reader
                .query(&principal(7), &all_tracking(), 0)
                .unwrap()
                .is_empty()
        );
        assert!(!root.exists(), "a reader must never create the store");
    }

    #[test]
    fn a_reader_fails_closed_on_a_tampered_tree() {
        let fx = Fixture::new("tampered");
        let user_dir = fx.goals_root.join(GOALS_DIR).join("7");
        std::fs::set_permissions(&user_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            fx.reader().query(&principal(7), &all_goals(), 0),
            Err(Error::Store {
                source: hatchling_memory::Error::InsecurePermissions { .. }
            })
        ));
        // And it did not repair the mode on the way.
        assert_eq!(std::fs::metadata(&user_dir).unwrap().mode() & 0o777, 0o755);
        std::fs::set_permissions(&user_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        // A reader demanding another owner refuses the whole tree.
        let foreign = FeedReader::open_as(&fx.goals_root, u32::MAX, None);
        assert!(matches!(
            foreign.query(&principal(7), &all_tracking(), 0),
            Err(Error::Store {
                source: hatchling_memory::Error::InsecurePermissions { .. }
            })
        ));
    }

    /// Per path: mode (with type bits), owner, modification time
    /// (seconds, nanoseconds), and file bytes.
    type Snapshot = BTreeMap<PathBuf, (u32, u32, i64, i64, Option<Vec<u8>>)>;

    /// Everything observable about a tree except access and change
    /// times.
    fn snapshot(root: &Path) -> Snapshot {
        let mut out = BTreeMap::new();
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let meta = std::fs::symlink_metadata(&path).unwrap();
                let bytes = if meta.is_file() {
                    Some(std::fs::read(&path).unwrap())
                } else {
                    dirs.push(path.clone());
                    None
                };
                out.insert(
                    path,
                    (
                        meta.mode(),
                        meta.uid(),
                        meta.mtime(),
                        meta.mtime_nsec(),
                        bytes,
                    ),
                );
            }
        }
        out
    }

    #[test]
    fn no_query_changes_the_store() {
        // Negative acceptance, behavioral half: every query shape, run
        // against a populated store, leaves both trees exactly as they
        // were — no file, mode, or modification time changes.
        let fx = Fixture::new("readonly");
        let goals_before = snapshot(&fx.goals_root);
        let memory_before = snapshot(&fx.memory_root);
        let reader = fx.reader();
        for uid in [7, 8] {
            let p = principal(uid);
            for query in [
                all_goals(),
                FeedQuery::Goals {
                    status: StatusFilter::Open,
                    updated_since_secs: Some(1),
                    after: Some(fx.open.id.clone()),
                },
                all_tracking(),
                FeedQuery::Tracking {
                    range: TimeRange::new(Some(1), Some(1_000)).unwrap(),
                    source: Some(label("whatsapp")),
                    after_seq: Some(0),
                },
                memory_query("feed"),
            ] {
                let items = reader.query(&p, &query, 300).unwrap();
                let _ = page(&items, PAGE_BUDGET_BYTES).unwrap();
            }
        }
        assert_eq!(snapshot(&fx.goals_root), goals_before);
        // The memory tree gains only user 8's (empty) scope directory:
        // 's recall creates the principal's scope on first use.
        let mut memory_after = snapshot(&fx.memory_root);
        memory_after.retain(|path, _| !path.starts_with(fx.memory_root.join("user").join("8")));
        let user_dir = fx.memory_root.join("user");
        memory_after.remove(&user_dir);
        let mut memory_before = memory_before;
        memory_before.remove(&user_dir);
        assert_eq!(memory_after, memory_before);
    }

    #[test]
    fn the_feed_reader_exposes_no_write_surface() {
        // Negative acceptance, structural half: the reader's public
        // surface is exactly `open`, `with_source` and `query`, and neither the feed
        // module nor the read path it shares contains a mutating call.
        // Adding one breaks this test and forces a WO-level decision.
        const MUTATING: &[&str] = &[
            "GoalsStore",
            "create_goal",
            "update_goal",
            "close_goal",
            "append_tracking",
            "ensure_chain",
            "ensure_owner_only_dir",
            "write_atomic",
            "OpenOptions",
            "set_permissions",
            "create_dir",
            "remove_file",
            "remove_dir",
            "rename(",
            "write_all",
            ".write(",
            ".remove(",
            "purge_expired",
        ];
        let feed = include_str!("feed.rs");
        let read = include_str!("read.rs");
        let code = |source: &'static str| source.split("#[cfg(test)]").next().unwrap_or(source);
        let (feed, read) = (code(feed), code(read));

        let reader_impl = feed
            .split("impl FeedReader {")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .unwrap();
        let public: Vec<&str> = reader_impl
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub fn "))
            .filter_map(|rest| rest.split('(').next())
            .collect();
        assert_eq!(public, vec!["open", "with_source", "query"]);

        for (name, source) in [("feed.rs", feed), ("read.rs", read)] {
            for call in MUTATING {
                // The module docs name `GoalsStore` to say the reader
                // never holds one; only code lines count.
                let hit = source
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .any(|line| line.contains(call));
                assert!(!hit, "{name} contains a mutating call: `{call}`");
            }
        }
    }

    #[test]
    fn paging_splits_at_the_budget_and_resumes_at_the_cursor() {
        let fx = Fixture::new("paging");
        let store = GoalsStore::open(&fx.goals_root).unwrap();
        let p = principal(7);
        for n in 0..20 {
            store
                .append_tracking(
                    &p,
                    &label("user"),
                    &format!("entry {n} {}", "x".repeat(900)),
                    300,
                )
                .unwrap();
        }
        let reader = fx.reader();
        let items = reader.query(&p, &all_tracking(), 0).unwrap();
        assert_eq!(items.len(), 22);
        // A budget that fits a few entries at a time.
        let budget = 4 * 1024;
        let mut seen = Vec::new();
        let mut after_seq = None;
        let mut finished = false;
        // Bounded: a cursor that stops advancing fails the test instead
        // of looping forever.
        for _ in 0..30 {
            let query = FeedQuery::Tracking {
                range: TimeRange::ALL,
                source: None,
                after_seq,
            };
            let page = page(&reader.query(&p, &query, 0).unwrap(), budget).unwrap();
            let used: usize = page
                .items_json
                .iter()
                .map(|json| serde_json::to_string(json).unwrap().len() + 1)
                .sum();
            assert!(used <= budget, "a page exceeded its budget: {used}");
            assert!(!page.items_json.is_empty(), "a page must make progress");
            for json in &page.items_json {
                seen.push(FeedItem::from_json(json).unwrap().cursor());
            }
            let Some(next) = page.next else {
                finished = true;
                break;
            };
            after_seq = Some(next.parse::<u64>().unwrap());
        }
        assert!(finished, "paging never reached the end");
        let expected: Vec<String> = (0..22).map(|n: u64| n.to_string()).collect();
        assert_eq!(seen, expected, "every entry exactly once, in order");
    }

    #[test]
    fn paging_counts_the_encoded_size_exactly() {
        let fx = Fixture::new("paging-exact");
        let reader = fx.reader();
        let items = reader.query(&principal(7), &all_tracking(), 0).unwrap();
        let costs: Vec<usize> = items
            .iter()
            .map(|item| {
                serde_json::to_string(&item.to_json().unwrap())
                    .unwrap()
                    .len()
                    + 1
            })
            .collect();
        let (a, b) = (*costs.first().unwrap(), *costs.get(1).unwrap());
        // Exactly both fit: one page, no cursor.
        let whole = page(&items, a + b).unwrap();
        assert_eq!(whole.items_json.len(), 2);
        assert_eq!(whole.next, None);
        // One byte short: the second waits for the next page.
        let split = page(&items, a + b - 1).unwrap();
        assert_eq!(split.items_json.len(), 1);
        assert_eq!(split.next.as_deref(), Some("0"));
        // A budget smaller than the first item still makes progress.
        let tiny = page(&items, 1).unwrap();
        assert_eq!(tiny.items_json.len(), 1);
        assert_eq!(tiny.next.as_deref(), Some("0"));
        assert_eq!(
            page(&[], 1).unwrap(),
            FeedPage {
                items_json: vec![],
                next: None
            }
        );
    }

    #[test]
    fn the_largest_goal_and_entry_fit_a_page_with_room_to_spare() {
        // Worst case for escaping: every byte a quote, which doubles
        // inside the item JSON and doubles again inside the reply.
        let quotes = |n: usize| "\"".repeat(n);
        let goal = Goal {
            id: "a".repeat(128).parse().unwrap(),
            title: quotes(MAX_TITLE_BYTES),
            detail: quotes(MAX_DETAIL_BYTES),
            status: GoalStatus::Closed {
                outcome: GoalOutcome::Abandoned,
                closed_at_secs: u64::MAX,
            },
            origin: GoalOrigin::UserExplicit,
            created_at_secs: u64::MAX,
            updated_at_secs: u64::MAX,
        };
        let entry = TrackingEntry {
            seq: u64::MAX,
            at_secs: u64::MAX,
            source: "s".repeat(64).parse().unwrap(),
            text: quotes(MAX_TRACKING_TEXT_BYTES),
        };
        for item in [
            FeedItem {
                body: FeedBody::Goal(goal),
            },
            FeedItem {
                body: FeedBody::Tracking(entry),
            },
        ] {
            let cost = serde_json::to_string(&item.to_json().unwrap())
                .unwrap()
                .len();
            assert!(cost * 2 < PAGE_BUDGET_BYTES, "worst-case item costs {cost}");
        }
    }

    #[test]
    fn status_filters_parse_exactly() {
        assert_eq!(StatusFilter::from_str("open").unwrap(), StatusFilter::Open);
        assert_eq!(
            StatusFilter::from_str("closed").unwrap(),
            StatusFilter::Closed
        );
        assert_eq!(StatusFilter::from_str("any").unwrap(), StatusFilter::Any);
        assert!(StatusFilter::from_str("all").is_err());
        assert!(StatusFilter::from_str("Open").is_err());
    }

    #[test]
    fn the_store_names_are_stable() {
        assert_eq!(SourceStore::Memory.as_str(), "memory");
        assert_eq!(SourceStore::Tracking.as_str(), "tracking");
        assert_eq!(SourceStore::Goal.as_str(), "goal");
        let edit = GoalEdit::default();
        assert!(edit.title.is_none() && edit.detail.is_none());
    }
}
