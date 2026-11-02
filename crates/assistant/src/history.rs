//! The conversations kept across restarts: each one's turns, its title and time, saved as JSON
//! (the interface reads and writes the preferences; nothing here does I/O).

use crate::agent::Entry;
use crate::compact;
use crate::provider::Turn;
use serde::{Deserialize, Serialize};

/// Conversations kept; the oldest goes when a new one makes more.
pub const MAX_CHATS: usize = 20;

/// The characters a history row shows of a title.
const TITLE_CHARS: usize = 40;

/// One conversation, as saved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedChat {
    pub id: String,
    /// The first line of the first message, for the history list.
    pub title: String,
    /// Unix milliseconds of its last message: the list is newest first.
    pub updated: i64,
    #[serde(default)]
    pub turns: Vec<Turn>,
    /// Commands approved for the rest of the conversation ([`crate::agent::Agent`]).
    #[serde(default)]
    pub always: Vec<String>,
    /// The map context the model last saw.
    #[serde(default)]
    pub last_context: String,
}

/// Every saved conversation under one preference key.
#[derive(Debug, Serialize, Deserialize)]
pub struct Chats {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default)]
    pub chats: Vec<SavedChat>,
}

fn version() -> u32 {
    1
}

impl Default for Chats {
    fn default() -> Self {
        Chats { version: version(), chats: Vec::new() }
    }
}

impl Chats {
    /// Reads the saved conversations; a damaged value is no history rather than an error.
    pub fn parse(json: &str) -> Option<Chats> {
        let mut chats: Chats = serde_json::from_str(json).ok()?;
        chats.chats.sort_by_key(|c| std::cmp::Reverse(c.updated));
        chats.chats.truncate(MAX_CHATS);
        Some(chats)
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| e.to_string())
    }

    /// Adds or replaces a conversation by id, newest first, keeping [`MAX_CHATS`].
    pub fn upsert(&mut self, chat: SavedChat) {
        self.chats.retain(|c| c.id != chat.id);
        self.chats.push(chat);
        self.chats.sort_by_key(|c| std::cmp::Reverse(c.updated));
        self.chats.truncate(MAX_CHATS);
    }

    pub fn get(&self, id: &str) -> Option<&SavedChat> {
        self.chats.iter().find(|c| c.id == id)
    }
}

/// The title a history list shows: the first line of the first user message, cut short. `None`
/// when the user hasn't said anything yet.
pub fn title(turns: &[Turn]) -> Option<String> {
    // A summary-only user turn (see [`compact::apply`]) has no message of the user's: the title
    // comes from the first one that has text.
    let text = turns
        .iter()
        .filter_map(|t| match t {
            Turn::User(text) => Some(user_text(text)),
            _ => None,
        })
        .find(|text| !text.trim().is_empty())?;
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    let mut title: String = line.chars().take(TITLE_CHARS).collect();
    if line.chars().count() > TITLE_CHARS {
        title.push('…');
    }
    Some(title)
}

/// A user turn's text as the user wrote it: without the map context and the summary block that
/// go before it (see [`compact`]).
pub fn user_text(text: &str) -> &str {
    let mut rest = text;
    if let Some(after) = rest.strip_prefix("<map>\n")
        && let Some(i) = after.find("\n</map>\n\n")
    {
        rest = after.get(i + "\n</map>\n\n".len()..).unwrap_or(rest);
    }
    while let Some(end) = compact::summary_len(rest) {
        match rest.get(end..) {
            Some(after) => rest = after,
            None => break,
        }
    }
    rest
}

/// The transcript a restored conversation shows: its messages, and its calls with the results
/// that followed them. A call whose result never came (the run was stopped) shows nothing, as
/// the notes and errors of the earlier run are not restored either.
pub fn transcript(turns: &[Turn]) -> Vec<Entry> {
    let mut out = Vec::new();
    for (i, turn) in turns.iter().enumerate() {
        match turn {
            Turn::User(text) => out.push(Entry::User(user_text(text).to_string())),
            Turn::Assistant { text, calls, .. } => {
                if !text.trim().is_empty() {
                    out.push(Entry::Assistant(text.trim().to_string()));
                }
                let results = match turns.get(i + 1) {
                    Some(Turn::Results(results)) => results.as_slice(),
                    _ => &[],
                };
                for call in calls {
                    let Some(result) = results.iter().find(|r| r.id == call.id).or_else(|| results.iter().find(|r| r.name == call.name))
                    else {
                        continue;
                    };
                    out.push(Entry::Tool {
                        name: call.name.clone(),
                        summary: crate::tools::summary(call),
                        result: result.content.clone(),
                        error: result.error,
                    });
                }
            }
            Turn::Results(_) => {}
        }
    }
    out
}
