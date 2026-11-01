//! The owner's conversation, kept across a restart (WO-026): sealed in a
//! store of its own (`HATCHLING_CONVERSATION_DIR`), written by the chat
//! and cleared by the owner alone.
//!
//! - **What's kept is what a run is given:** the newest exchanges within
//!   [`conversation::recent`]'s bounds, each kept once its answer reached
//!   the owner as written. No session op reaches this store, so a cell
//!   can't add to it, read it or clear it.
//! - **Each side is an entry of its own,** raw. A side is at most
//!   [`conversation::MAX_HISTORY_BYTES`], which fits one sealed entry; the
//!   same text as JSON might not. The index says which exchanges are the
//!   conversation, and their sides live in [`SLOTS`] slots, one more than
//!   a conversation holds, so a new exchange never overwrites one kept.
//! - **A new conversation removes every slot,** whatever the index says,
//!   before the owner is told it started. A keep checks, with the store
//!   locked, that its conversation is still the chat's, so an answer to a
//!   message sent before is never written after.
//! - **Read once, at the broker's start** ([`at_start`]): the chat resumes
//!   it, and the app is shown it, under a notice. One that can't be read
//!   is removed, and the owner is told a new conversation started.

use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};

use hatchling_channel::LocalHub;
use hatchling_core::SecretName;
use hatchling_core::conversation::{self, ChatExchange, MAX_EXCHANGES};
use hatchling_core::ui::{ChatAuthor, ChatMessage, UiEventBody};
use hatchling_scheduler::Timestamp;
use hatchling_vault::{OpensStores, SealedVault, Secret, Store};
use serde::{Deserialize, Serialize};

use crate::Config;
use crate::api_stream::Bus;
use crate::chat_run::Keeps;

/// The slots the sides are kept in: one more than a conversation holds.
pub const SLOTS: u64 = 9;

/// What the app shows above a conversation kept across a restart.
pub const KEPT: &str =
    "Kept from before the broker restarted: the agent is still given this conversation.";

/// What the app shows when the conversation kept couldn't be read.
pub const UNREADABLE: &str = "The conversation kept before the broker restarted couldn't be read, so this is a new conversation. The broker's log says why.";

/// The index's entry name.
const INDEX: &str = "index";

/// Serializes the store's writers within a broker.
static STORE: Mutex<()> = Mutex::new(());

/// What the store held when the broker started, read once.
static AT_START: OnceLock<Resumed> = OnceLock::new();

/// Which exchanges are the conversation: those numbered from
/// `next - kept` up to `next`, each in the slot its number gives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    next: u64,
    kept: u64,
}

/// The slot exchange `number` is kept in.
const fn slot(number: u64) -> u64 {
    number % SLOTS
}

/// What the broker found kept when it started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resumed {
    /// The conversation kept, oldest first; empty when there was none.
    Kept(Vec<ChatExchange>),
    /// One that couldn't be read, and was removed.
    Unreadable,
}

/// The conversation's sealed store.
pub struct ConversationStore<'k> {
    root: &'k Path,
    keys: &'k dyn OpensStores,
}

impl std::fmt::Debug for ConversationStore<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationStore")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl<'k> ConversationStore<'k> {
    /// The store `config` gives the broker, opened with `keys`; `None` when
    /// the operator gave it none, and the conversation stays in memory.
    #[must_use]
    pub fn of(config: &'k Config, keys: &'k dyn OpensStores) -> Option<Self> {
        let root = config.scheduler.conversation.as_path();
        (!root.as_os_str().is_empty()).then_some(Self { root, keys })
    }

    fn vault(&self) -> Result<SealedVault<'_>, String> {
        SealedVault::open_in(self.root, self.keys, Store::CONVERSATION)
            .map_err(|error| format!("the conversation can't be opened: {error}"))
    }

    fn name(raw: &str) -> Result<SecretName, String> {
        raw.parse()
            .map_err(|_| format!("the conversation's entry name {raw:?} is invalid"))
    }

    /// The entry names of slot `slot`'s sides: what the owner said, and
    /// what the owner was shown.
    fn sides(slot: u64) -> [String; 2] {
        [format!("user-{slot}"), format!("assistant-{slot}")]
    }

    fn read_index(vault: &SealedVault<'_>) -> Result<Option<Index>, String> {
        let secret = match vault.fetch(&Self::name(INDEX)?) {
            Ok(secret) => secret,
            Err(hatchling_vault::Error::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(format!("the conversation can't be read: {error}")),
        };
        let index: Index = serde_json::from_slice(secret.as_bytes())
            .map_err(|error| format!("the conversation's index is corrupt: {error}"))?;
        let most = u64::try_from(MAX_EXCHANGES).unwrap_or(u64::MAX);
        if index.kept > most || index.kept > index.next {
            return Err(String::from(
                "the conversation's index holds more than a conversation can",
            ));
        }
        Ok(Some(index))
    }

    fn write(vault: &SealedVault<'_>, name: &str, bytes: Vec<u8>) -> Result<(), String> {
        let secret = Secret::new(bytes)
            .map_err(|error| format!("the conversation's {name} doesn't fit: {error}"))?;
        vault
            .store(&Self::name(name)?, &secret)
            .map_err(|error| format!("the conversation's {name} can't be written: {error}"))
    }

    fn write_index(vault: &SealedVault<'_>, index: Index) -> Result<(), String> {
        let bytes = serde_json::to_vec(&index)
            .map_err(|error| format!("the conversation's index doesn't encode: {error}"))?;
        Self::write(vault, INDEX, bytes)
    }

    /// Removes slot `slot`'s sides: one already gone is fine.
    fn clear_slot(vault: &SealedVault<'_>, slot: u64) -> Result<(), String> {
        for name in Self::sides(slot) {
            match vault.remove(&Self::name(&name)?) {
                Ok(()) | Err(hatchling_vault::Error::NotFound { .. }) => {}
                Err(error) => {
                    return Err(format!(
                        "the conversation's {name} can't be removed: {error}"
                    ));
                }
            }
        }
        Ok(())
    }

    fn read_side(vault: &SealedVault<'_>, name: &str) -> Result<String, String> {
        let secret = vault
            .fetch(&Self::name(name)?)
            .map_err(|error| format!("the conversation's {name} can't be read: {error}"))?;
        String::from_utf8(secret.as_bytes().to_vec())
            .map_err(|_| format!("the conversation's {name} isn't text"))
    }

    /// The conversation kept, oldest first; nothing when none is.
    ///
    /// The sides a keep wrote before its index, when the broker stopped
    /// in between, are removed: they were never part of it.
    ///
    /// # Errors
    ///
    /// Why it can't be read: the store's failure, an entry that isn't
    /// what the chat writes, or a conversation past the bounds a run is
    /// given.
    pub fn resume(&self) -> Result<Vec<ChatExchange>, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let Some(index) = Self::read_index(&vault)? else {
            return Ok(Vec::new());
        };
        Self::clear_slot(&vault, slot(index.next))?;
        let exchanges = (index.next - index.kept..index.next)
            .map(|number| {
                let [user, assistant] = Self::sides(slot(number));
                Ok(ChatExchange {
                    user: Self::read_side(&vault, &user)?,
                    assistant: Self::read_side(&vault, &assistant)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        if conversation::recent(&exchanges).len() != exchanges.len() {
            return Err(String::from(
                "the conversation kept is past the bounds a run is given",
            ));
        }
        Ok(exchanges)
    }

    /// Keeps `exchange` as the conversation's newest, so its last `kept`
    /// exchanges are the conversation, if `current` says, with the store
    /// locked, that its conversation is still the chat's. Whether it did.
    ///
    /// `kept` counts `exchange`, and is never more than the store holds
    /// with it; with `kept` 0, nothing is.
    ///
    /// # Errors
    ///
    /// Why it can't be kept: the store's failure.
    pub fn keep_if(
        &self,
        current: impl FnOnce() -> bool,
        exchange: &ChatExchange,
        kept: usize,
    ) -> Result<bool, String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        if !current() {
            return Ok(false);
        }
        let vault = self.vault()?;
        let index = Self::read_index(&vault)?.unwrap_or_default();
        let kept = u64::try_from(kept)
            .unwrap_or(u64::MAX)
            .min(index.kept.saturating_add(1));
        let number = index.next;
        if kept > 0 {
            let [user, assistant] = Self::sides(slot(number));
            Self::write(&vault, &user, exchange.user.clone().into_bytes())?;
            Self::write(&vault, &assistant, exchange.assistant.clone().into_bytes())?;
        }
        let next = number
            .checked_add(1)
            .ok_or_else(|| String::from("the conversation has no number left"))?;
        Self::write_index(&vault, Index { next, kept })?;
        // The exchanges the conversation no longer holds.
        for gone in number - index.kept..next - kept {
            Self::clear_slot(&vault, slot(gone))?;
        }
        Ok(true)
    }

    /// Starts a new conversation: every slot is removed, whatever the
    /// index says, and then `start` runs, with the store locked.
    ///
    /// # Errors
    ///
    /// Why the conversation can't be removed; `start` hasn't run.
    pub fn start_new(&self, start: impl FnOnce()) -> Result<(), String> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let next = Self::read_index(&vault)
            .ok()
            .flatten()
            .map_or(0, |index| index.next);
        Self::write_index(&vault, Index { next, kept: 0 })?;
        for slot in 0..SLOTS {
            Self::clear_slot(&vault, slot)?;
        }
        start();
        Ok(())
    }

    /// The conversation kept, or, when it can't be read, a new one: the
    /// store is cleared, and the broker's log says why.
    #[must_use]
    pub fn resume_or_start_new(&self) -> Resumed {
        match self.resume() {
            Ok(kept) => Resumed::Kept(kept),
            Err(reason) => {
                tracing::warn!(%reason, "the conversation kept can't be read: a new one starts");
                if let Err(error) = self.start_new(|| {}) {
                    tracing::error!(%error, "the conversation kept can't be removed");
                }
                Resumed::Unreadable
            }
        }
    }
}

/// What `store` held when the broker started: read the first time it's
/// asked, so the chat and the app are given the same.
pub fn at_start(store: &ConversationStore<'_>) -> &'static Resumed {
    AT_START.get_or_init(|| store.resume_or_start_new())
}

/// Shows the app what was kept across a restart: a notice, then each
/// exchange as it was, on the local chat. Nothing when none was.
pub fn show(bus: &Bus, resumed: &Resumed) {
    let push = |author: ChatAuthor, text: &str| match crate::approval::random_hex("msg-") {
        Ok(message_id) => {
            bus.push(
                Timestamp::now().to_string(),
                UiEventBody::ChatMessage(ChatMessage {
                    chat_id: String::from(crate::api_ops::LOCAL_CHAT),
                    message_id,
                    author,
                    text: text.to_owned(),
                }),
            );
        }
        Err(error) => tracing::warn!(%error, "a kept message can't be shown"),
    };
    match resumed {
        Resumed::Kept(exchanges) if exchanges.is_empty() => {}
        Resumed::Kept(exchanges) => {
            push(ChatAuthor::System, KEPT);
            for exchange in exchanges {
                push(ChatAuthor::User, &exchange.user);
                push(ChatAuthor::Agent, &exchange.assistant);
            }
        }
        Resumed::Unreadable => push(ChatAuthor::System, UNREADABLE),
    }
}

/// The chat's sealed conversation: each exchange kept while the local
/// hub's conversation is still the one it was sent in.
#[derive(Debug)]
pub struct SealedConversation<'k> {
    store: ConversationStore<'k>,
    hub: &'k LocalHub,
}

impl<'k> SealedConversation<'k> {
    /// `store`, kept for `hub`'s conversation.
    #[must_use]
    pub const fn new(store: ConversationStore<'k>, hub: &'k LocalHub) -> Self {
        Self { store, hub }
    }
}

impl Keeps for SealedConversation<'_> {
    fn keep(&self, sent_in: u64, exchange: &ChatExchange, kept: usize) {
        let current = || self.hub.conversation() == sent_in;
        if let Err(error) = self.store.keep_if(current, exchange, kept) {
            tracing::warn!(%error, "the conversation can't be kept");
        }
    }
}

#[cfg(test)]
mod tests {
    use hatchling_core::conversation::MAX_HISTORY_BYTES;
    use hatchling_core::ui::UiEvent;
    use hatchling_vault::MasterKey;

    use super::*;

    /// A config whose conversation is kept in a scratch directory.
    fn kept(name: &str) -> (Config, crate::api_auth::tests::Scratch, MasterKey) {
        let scratch = crate::api_auth::tests::scratch(&format!("conversation-{name}"));
        let mut config = crate::api_auth::tests::owner_config(None);
        config.scheduler.conversation = scratch.0.join("conversation");
        (config, scratch, MasterKey::generate().unwrap())
    }

    fn exchange(user: &str, assistant: &str) -> ChatExchange {
        ChatExchange {
            user: user.to_owned(),
            assistant: assistant.to_owned(),
        }
    }

    /// Keeps each of `exchanges` as the chat would: the conversation is
    /// what a run would be given after it.
    fn keep_all(store: &ConversationStore<'_>, exchanges: &[ChatExchange]) {
        let mut so_far = Vec::new();
        for exchange in exchanges {
            so_far.push(exchange.clone());
            let given = conversation::recent(&so_far).len();
            assert!(store.keep_if(|| true, exchange, given).unwrap());
        }
    }

    /// The slots holding a side.
    fn held(config: &Config, key: &MasterKey) -> Vec<u64> {
        let vault =
            SealedVault::open_in(&config.scheduler.conversation, key, Store::CONVERSATION).unwrap();
        (0..SLOTS)
            .filter(|&slot| {
                ConversationStore::sides(slot)
                    .iter()
                    .any(|side| vault.exists(&side.parse().unwrap()))
            })
            .collect()
    }

    fn plant(config: &Config, key: &MasterKey, name: &str, bytes: &[u8]) {
        let vault =
            SealedVault::open_in(&config.scheduler.conversation, key, Store::CONVERSATION).unwrap();
        vault
            .store(
                &name.parse().unwrap(),
                &Secret::new(bytes.to_vec()).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn there_is_one_slot_more_than_a_conversation_holds() {
        assert_eq!(SLOTS, u64::try_from(MAX_EXCHANGES).unwrap() + 1);
        // Exchanges go round the slots in order.
        assert_eq!(
            [slot(0), slot(8), slot(9), slot(13), slot(18)],
            [0, 8, 0, 4, 0]
        );
    }

    /// Each entry is written, read back, and removed as it was: the index,
    /// a side, and a slot's two sides; what isn't one is refused.
    #[test]
    fn the_stores_entries_are_written_read_and_removed() {
        let (config, _scratch, key) = kept("entries");
        let store = ConversationStore::of(&config, &key).unwrap();
        let vault = store.vault().unwrap();
        assert!(ConversationStore::name("user-3").is_ok());
        assert!(ConversationStore::name("user 3").is_err());
        assert_eq!(ConversationStore::read_index(&vault).unwrap(), None);
        let index = Index { next: 12, kept: 3 };
        ConversationStore::write_index(&vault, index).unwrap();
        assert_eq!(ConversationStore::read_index(&vault).unwrap(), Some(index));
        ConversationStore::write(&vault, "user-4", b"said".to_vec()).unwrap();
        ConversationStore::write(&vault, "assistant-4", Vec::new()).unwrap();
        assert_eq!(
            ConversationStore::read_side(&vault, "user-4").unwrap(),
            "said"
        );
        assert_eq!(
            ConversationStore::read_side(&vault, "assistant-4").unwrap(),
            ""
        );
        let too_big = vec![b'x'; hatchling_core::MAX_SECRET_BYTES + 1];
        let reason = ConversationStore::write(&vault, "user-5", too_big).unwrap_err();
        assert!(reason.contains("doesn't fit"), "{reason}");
        ConversationStore::write(&vault, "user-5", vec![0xff]).unwrap();
        let reason = ConversationStore::read_side(&vault, "user-5").unwrap_err();
        assert!(reason.contains("isn't text"), "{reason}");
        // A slot is emptied whole, and an empty one is emptied again.
        ConversationStore::clear_slot(&vault, 4).unwrap();
        ConversationStore::clear_slot(&vault, 4).unwrap();
        assert!(ConversationStore::read_side(&vault, "user-4").is_err());
        assert!(ConversationStore::read_side(&vault, "assistant-4").is_err());
        // Slot 5 is left as it was.
        assert_eq!(held(&config, &key), vec![5_u64]);
    }

    /// The store is read once a broker: the chat and the app are given the
    /// same, even after it changes.
    #[test]
    fn what_was_kept_at_the_start_is_read_once() {
        let (config, _scratch, key) = kept("at-start");
        let store = ConversationStore::of(&config, &key).unwrap();
        keep_all(&store, &[exchange("q0", "a0")]);
        let first = at_start(&store);
        assert_eq!(first, &Resumed::Kept(vec![exchange("q0", "a0")]));
        store.start_new(|| {}).unwrap();
        assert!(std::ptr::eq(at_start(&store), first));
    }

    /// No store, nothing kept: a restart starts a new conversation.
    #[test]
    fn without_a_store_the_conversation_stays_in_memory() {
        let config = crate::api_auth::tests::owner_config(None);
        let key = MasterKey::generate().unwrap();
        assert!(ConversationStore::of(&config, &key).is_none());
    }

    /// What's kept comes back as the owner said it and was shown it, even
    /// text that as JSON wouldn't fit one sealed entry.
    #[test]
    fn the_conversation_kept_is_resumed_as_it_was() {
        let (config, _scratch, key) = kept("resume");
        let store = ConversationStore::of(&config, &key).unwrap();
        assert_eq!(store.resume().unwrap(), []);
        // Each control character is six bytes as JSON.
        let wide = "\u{1}".repeat(MAX_HISTORY_BYTES / 2 - 100);
        let exchanges = [
            exchange("Remember the word heron.", "Noted."),
            exchange(&wide, &wide),
            exchange("", "An empty message, answered."),
        ];
        keep_all(&store, &exchanges);
        assert_eq!(store.resume().unwrap(), exchanges);
        assert_eq!(held(&config, &key), [0, 1, 2]);
    }

    /// Only what a run is given stays: past the bound, the oldest go, and
    /// their slots are emptied; one too big to give ends it there.
    #[test]
    fn only_what_a_run_is_given_is_kept() {
        let (config, _scratch, key) = kept("bounds");
        let store = ConversationStore::of(&config, &key).unwrap();
        let many: Vec<_> = (0..12)
            .map(|n| exchange(&format!("q{n}"), &format!("a{n}")))
            .collect();
        keep_all(&store, &many);
        assert_eq!(store.resume().unwrap(), many.get(4..).unwrap());
        // Exchanges 4 to 11, in slots 4 to 8 and 0 to 2: slot 3 is next.
        assert_eq!(held(&config, &key), [0, 1, 2, 4, 5, 6, 7, 8]);
        let huge = "x".repeat(MAX_HISTORY_BYTES);
        assert!(store.keep_if(|| true, &exchange("big", &huge), 0).unwrap());
        assert_eq!(store.resume().unwrap(), []);
        assert!(held(&config, &key).is_empty());
        // And the conversation goes on from there.
        let first = many.first().unwrap();
        assert!(store.keep_if(|| true, first, 1).unwrap());
        assert_eq!(store.resume().unwrap(), std::slice::from_ref(first));
    }

    /// A keep never claims an exchange the store doesn't hold.
    #[test]
    fn a_keep_claims_no_more_than_the_store_holds() {
        let (config, _scratch, key) = kept("claims");
        let store = ConversationStore::of(&config, &key).unwrap();
        assert!(store.keep_if(|| true, &exchange("q0", "a0"), 5).unwrap());
        assert_eq!(store.resume().unwrap(), [exchange("q0", "a0")]);
    }

    /// An answer to a message from before a new conversation writes
    /// nothing.
    #[test]
    fn a_keep_for_a_conversation_moved_on_writes_nothing() {
        let (config, _scratch, key) = kept("moved");
        let store = ConversationStore::of(&config, &key).unwrap();
        keep_all(&store, &[exchange("q0", "a0")]);
        assert!(!store.keep_if(|| false, &exchange("q1", "a1"), 2).unwrap());
        assert_eq!(store.resume().unwrap(), [exchange("q0", "a0")]);
        assert_eq!(held(&config, &key), [0]);
    }

    /// A new conversation removes every slot, one the index doesn't name
    /// too, before it starts.
    #[test]
    fn a_new_conversation_removes_everything_kept() {
        let (config, _scratch, key) = kept("new");
        let store = ConversationStore::of(&config, &key).unwrap();
        keep_all(&store, &[exchange("q0", "a0"), exchange("q1", "a1")]);
        plant(&config, &key, "assistant-6", b"left behind");
        let mut started = false;
        store.start_new(|| started = true).unwrap();
        assert!(started);
        assert_eq!(store.resume().unwrap(), []);
        assert!(held(&config, &key).is_empty());
        keep_all(&store, &[exchange("q2", "a2")]);
        assert_eq!(store.resume().unwrap(), [exchange("q2", "a2")]);
    }

    /// The sides a keep wrote before the broker stopped, without its
    /// index, never come back.
    #[test]
    fn sides_written_without_their_index_are_never_resumed() {
        let (config, _scratch, key) = kept("crash");
        let store = ConversationStore::of(&config, &key).unwrap();
        keep_all(&store, &[exchange("q0", "a0")]);
        plant(&config, &key, "user-1", b"q1");
        plant(&config, &key, "assistant-1", b"a1");
        assert_eq!(store.resume().unwrap(), [exchange("q0", "a0")]);
        assert_eq!(held(&config, &key), [0]);
    }

    /// What the chat never writes isn't read as a conversation, and one
    /// that can't be read starts a new one, emptied.
    #[test]
    fn a_conversation_that_cant_be_read_starts_a_new_one() {
        let (config, _scratch, key) = kept("unreadable");
        let store = ConversationStore::of(&config, &key).unwrap();
        let unreadable = |index: &[u8], error: &str| {
            plant(&config, &key, INDEX, index);
            let reason = store.resume().unwrap_err();
            assert!(reason.contains(error), "{reason}");
        };
        unreadable(b"[]", "corrupt");
        unreadable(br#"{"next":9,"kept":1,"more":1}"#, "corrupt");
        unreadable(br#"{"next":9,"kept":9}"#, "more than");
        unreadable(br#"{"next":1,"kept":2}"#, "more than");
        unreadable(br#"{"next":1,"kept":1}"#, "user-0");
        plant(&config, &key, "user-0", &[0xff]);
        unreadable(br#"{"next":1,"kept":1}"#, "isn't text");
        let huge = "x".repeat(MAX_HISTORY_BYTES / 2 + 1);
        plant(&config, &key, "user-0", huge.as_bytes());
        plant(&config, &key, "assistant-0", huge.as_bytes());
        unreadable(br#"{"next":1,"kept":1}"#, "past the bounds");
        // Sealed under another key, nothing opens.
        keep_all(&store, &[exchange("q0", "a0")]);
        let other = MasterKey::generate().unwrap();
        let reason = ConversationStore::of(&config, &other)
            .unwrap()
            .resume()
            .unwrap_err();
        assert!(reason.contains("can't be"), "{reason}");
        // One that can't be read is emptied, and a new one starts.
        plant(&config, &key, INDEX, b"[]");
        assert_eq!(store.resume_or_start_new(), Resumed::Unreadable);
        assert_eq!(store.resume_or_start_new(), Resumed::Kept(Vec::new()));
        assert!(held(&config, &key).is_empty());
    }

    /// The chat's keeps follow the local hub's conversation.
    #[test]
    fn the_chat_keeps_only_while_its_conversation_is_the_hubs() {
        let (config, _scratch, key) = kept("hub");
        let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
        let sealed = SealedConversation::new(ConversationStore::of(&config, &key).unwrap(), &hub);
        sealed.keep(0, &exchange("q0", "a0"), 1);
        hub.new_conversation();
        sealed.keep(0, &exchange("q1", "a1"), 2);
        sealed.keep(1, &exchange("q2", "a2"), 2);
        assert_eq!(
            ConversationStore::of(&config, &key)
                .unwrap()
                .resume()
                .unwrap(),
            [exchange("q0", "a0"), exchange("q2", "a2")]
        );
    }

    fn chats(bus: &Bus) -> Vec<(ChatAuthor, String)> {
        bus.after(None)
            .into_iter()
            .filter_map(|event: UiEvent| {
                if let UiEventBody::ChatMessage(chat) = event.body {
                    Some((chat.author, chat.text))
                } else {
                    None
                }
            })
            .collect()
    }

    /// The app is shown what the agent is still given, under Hatchling's
    /// notice, or told a new conversation started.
    #[test]
    fn the_app_is_shown_what_was_kept() {
        let bus = Bus::starting_at(1);
        show(&bus, &Resumed::Kept(Vec::new()));
        assert_eq!(chats(&bus), []);
        show(
            &bus,
            &Resumed::Kept(vec![exchange("q0", "a0"), exchange("q1", "a1")]),
        );
        let said = |author, text: &str| (author, text.to_owned());
        assert_eq!(
            chats(&bus),
            [
                said(ChatAuthor::System, KEPT),
                said(ChatAuthor::User, "q0"),
                said(ChatAuthor::Agent, "a0"),
                said(ChatAuthor::User, "q1"),
                said(ChatAuthor::Agent, "a1"),
            ]
        );
        let bus = Bus::starting_at(1);
        show(&bus, &Resumed::Unreadable);
        assert_eq!(chats(&bus), [said(ChatAuthor::System, UNREADABLE)]);
    }
}
