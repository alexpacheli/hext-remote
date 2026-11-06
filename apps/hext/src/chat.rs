//! What the chat windows of the interface show. The messages live here, in
//! memory, for as long as the session and its window: they are not written
//! anywhere.

use std::collections::VecDeque;

use serde::Serialize;

/// The chat window of the session on this computer. (A viewer's is
/// `chat-viewer-<number>`.)
pub const HOST_CHAT: &str = "chat-host";
/// How many messages stay on screen: the other side cannot fill the memory.
const KEPT: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    /// This computer.
    Me,
    /// The other computer: its text is shown as text, nothing else.
    Them,
    /// The app.
    App,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    pub author: Author,
    pub text: String,
}

/// A conversation with one person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chat {
    /// Who is on the other side, as this app knows them.
    pub peer: String,
    pub lines: VecDeque<Line>,
    /// Messages can still go.
    pub live: bool,
}

impl Chat {
    pub fn new(peer: String) -> Self {
        Self {
            peer,
            lines: VecDeque::new(),
            live: true,
        }
    }

    pub fn push(&mut self, author: Author, text: String) {
        if self.lines.len() == KEPT {
            self.lines.pop_front();
        }
        self.lines.push_back(Line { author, text });
    }

    /// A session with `peer` starts. The same person coming back goes on;
    /// whether what was said can stay for someone else is not a question.
    /// Only an ID tells who it is (`identified`): a name is anyone's to type.
    pub fn stays_for(&mut self, peer: &str, identified: bool) -> bool {
        let same = identified && self.peer == peer;
        self.live = same;
        same
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flood_keeps_only_the_latest() {
        let mut chat = Chat::new("Técnico".into());
        for i in 0..KEPT + 3 {
            chat.push(Author::Them, i.to_string());
        }
        assert_eq!(chat.lines.len(), KEPT);
        assert_eq!(chat.lines[0].text, "3");
    }

    #[test]
    fn the_next_technician_does_not_read_the_last_ones_chat() {
        let mut chat = Chat::new("Ana (ID 1)".into());
        chat.live = false;
        assert!(chat.stays_for("Ana (ID 1)", true), "came back");
        assert!(chat.live);
        assert!(!chat.stays_for("Bruno (ID 2)", true));
    }

    #[test]
    fn a_name_alone_does_not_tell_who_came() {
        let mut chat = Chat::new("Ana".into());
        assert!(!chat.stays_for("Ana", false));
    }
}
