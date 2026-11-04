//! The owner's Feed page: what their scheduled runs wrote for it, kept.
//!
//! A schedule the owner points at [`FEED_PAGE`] (the app's feed prompt,
//! every morning or when they ask for one now) is an agent run like any
//! other, with the grants they gave it; its task is their prompt, framed by
//! [`task`] so the answer reads as one post. Its answer isn't sent anywhere:
//! the output gate judges it as for the chat, and it's kept here, a line per
//! post beside the schedule store (`<scheduler dir>.feed`), the newest 200.
//! The app reads them with `ReadFeed`.
//!
//! Only the owner may point a schedule here: sending to the page is a grant
//! the broker holds for the owner's own schedules
//! ([`crate::schedule_ops::owner_schedule_grants`]), never one an agent's
//! session holds, so an agent can't fill the owner's page with its own
//! schedules.

use std::path::Path;

pub use hatchling_core::ui::FEED_PAGE;
use hatchling_core::ui::FeedPost;

/// The prefix of a post's id.
const POST_ID_PREFIX: &str = "post-";

/// What a feed run is told: write one post, from the owner's `prompt`.
#[must_use]
pub fn task(prompt: &str) -> String {
    format!(
        "Write today's post for the owner's Feed page. They asked for this:\n\n{prompt}\n\nUse your tools to read what you can about them first: their goals, and what they've asked you to remember. Write only what those or your tools support; don't invent news or events. Begin with a short title on its own line, then the post, in Markdown, under 250 words. The post is your whole answer: it's kept on the page as you write it, and the owner reads it later."
    )
}

/// Keeps `text`, a feed run's answer as the gate cleared it, as a post on
/// the page at `path`, written now.
///
/// # Errors
///
/// Returns why it wasn't kept, in the broker's words.
pub fn keep(path: &Path, text: &str) -> Result<(), String> {
    let post_id = crate::approval::random_hex(POST_ID_PREFIX)
        .map_err(|error| format!("no post id: {error}"))?;
    let post = FeedPost {
        post_id,
        written_at: hatchling_scheduler::Timestamp::now().to_string(),
        text: text.to_owned(),
    };
    crate::kept_log::append(path, &post).map_err(|error| {
        tracing::warn!(%error, "a feed post wasn't kept");
        String::from("the Feed page couldn't keep it; the broker's log says why")
    })
}

/// The posts on the page at `path`, the newest first.
#[must_use]
pub fn list(path: &Path) -> Vec<FeedPost> {
    crate::kept_log::list(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_post_is_kept_with_an_id_and_when_and_listed_newest_first() {
        let dir = std::env::temp_dir().join(format!("hatchling-feed-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("schedules.feed");
        assert!(list(&path).is_empty());
        keep(&path, "Monday\nTwo goals moved.").unwrap();
        keep(&path, "Tuesday\nOne goal closed.").unwrap();
        let posts = list(&path);
        assert_eq!(
            posts
                .iter()
                .map(|post| post.text.as_str())
                .collect::<Vec<_>>(),
            ["Tuesday\nOne goal closed.", "Monday\nTwo goals moved."]
        );
        let (newest, oldest) = (posts.first().unwrap(), posts.get(1).unwrap());
        assert!(newest.post_id.starts_with(POST_ID_PREFIX));
        assert_ne!(newest.post_id, oldest.post_id);
        assert!(
            newest
                .written_at
                .parse::<hatchling_scheduler::Timestamp>()
                .is_ok()
        );
        // A page that can't be written says so, in the broker's words.
        let refused = keep(&dir.join("missing").join("schedules.feed"), "x").unwrap_err();
        assert!(refused.contains("couldn't keep it"), "{refused}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_task_carries_the_prompt_and_asks_for_one_post() {
        let task = task("What moved on my goals, and one thing to try.");
        assert!(task.contains("\n\nWhat moved on my goals, and one thing to try.\n\n"));
        assert!(task.starts_with("Write today's post for the owner's Feed page."));
        assert!(task.contains("short title on its own line"));
        assert!(task.contains("don't invent news"));
    }
}
