//! A safety-first mailbox domain model.
//!
//! The crate deliberately does not expose IMAP `EXPUNGE` or `\\Deleted` as
//! application operations. Production adapters should implement [`MailStore`]
//! with `MOVE` to/from a mailbox carrying the IMAP `\\Trash` special-use flag.

pub mod imap;
pub mod rdf;

use std::collections::BTreeMap;
use std::fmt;

/// A stable identifier supplied by a mail backend (for example an IMAP UID).
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MessageId(String);

impl MessageId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The special role of a mailbox as discovered from an IMAP server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailboxRole {
    Inbox,
    Trash,
    Archive,
    Normal,
}

/// Events emitted by safe state transitions for audit and user interfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MailEvent {
    Trashed {
        message: MessageId,
        from: String,
        to: String,
    },
    Restored {
        message: MessageId,
        from: String,
        to: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MailError {
    MessageNotFound(MessageId),
    TrashMailboxMissing,
    DestinationMissing(String),
    DestinationIsTrash(String),
    MessageAlreadyInTrash(MessageId),
}

impl fmt::Display for MailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MessageNotFound(id) => write!(f, "message {id} was not found"),
            Self::TrashMailboxMissing => f.write_str("no mailbox with the Trash role exists"),
            Self::DestinationMissing(name) => write!(f, "mailbox {name:?} does not exist"),
            Self::DestinationIsTrash(name) => write!(f, "cannot restore to Trash mailbox {name:?}"),
            Self::MessageAlreadyInTrash(id) => write!(f, "message {id} is already in Trash"),
        }
    }
}

impl std::error::Error for MailError {}

/// The only mutating operations that applications and agents receive.
pub trait MailStore {
    fn trash(&mut self, message: &MessageId) -> Result<(), MailError>;
    fn restore(&mut self, message: &MessageId, destination: &str) -> Result<(), MailError>;
    fn events(&self) -> &[MailEvent];
}

/// A deterministic reference implementation useful for tests and local prototypes.
#[derive(Default)]
pub struct InMemoryMailStore {
    mailboxes: BTreeMap<String, MailboxRole>,
    locations: BTreeMap<MessageId, String>,
    events: Vec<MailEvent>,
}

impl InMemoryMailStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_mailbox(&mut self, name: impl Into<String>, role: MailboxRole) {
        self.mailboxes.insert(name.into(), role);
    }

    pub fn add_message(&mut self, mailbox: &str, message: MessageId) -> Result<(), MailError> {
        if !self.mailboxes.contains_key(mailbox) {
            return Err(MailError::DestinationMissing(mailbox.to_owned()));
        }
        self.locations.insert(message, mailbox.to_owned());
        Ok(())
    }

    pub fn mailbox_of(&self, message: &MessageId) -> Option<&str> {
        self.locations.get(message).map(String::as_str)
    }

    fn trash_mailbox(&self) -> Option<&str> {
        self.mailboxes
            .iter()
            .find_map(|(name, role)| (*role == MailboxRole::Trash).then_some(name.as_str()))
    }
}

impl MailStore for InMemoryMailStore {
    fn trash(&mut self, message: &MessageId) -> Result<(), MailError> {
        let from = self
            .locations
            .get(message)
            .cloned()
            .ok_or_else(|| MailError::MessageNotFound(message.clone()))?;
        let to = self
            .trash_mailbox()
            .ok_or(MailError::TrashMailboxMissing)?
            .to_owned();

        if from == to {
            return Err(MailError::MessageAlreadyInTrash(message.clone()));
        }

        self.locations.insert(message.clone(), to.clone());
        self.events.push(MailEvent::Trashed {
            message: message.clone(),
            from,
            to,
        });
        Ok(())
    }

    fn restore(&mut self, message: &MessageId, destination: &str) -> Result<(), MailError> {
        let from = self
            .locations
            .get(message)
            .cloned()
            .ok_or_else(|| MailError::MessageNotFound(message.clone()))?;
        let role = self
            .mailboxes
            .get(destination)
            .copied()
            .ok_or_else(|| MailError::DestinationMissing(destination.to_owned()))?;

        if role == MailboxRole::Trash {
            return Err(MailError::DestinationIsTrash(destination.to_owned()));
        }

        self.locations
            .insert(message.clone(), destination.to_owned());
        self.events.push(MailEvent::Restored {
            message: message.clone(),
            from,
            to: destination.to_owned(),
        });
        Ok(())
    }

    fn events(&self) -> &[MailEvent] {
        &self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> InMemoryMailStore {
        let mut store = InMemoryMailStore::new();
        store.add_mailbox("INBOX", MailboxRole::Inbox);
        store.add_mailbox("Trash", MailboxRole::Trash);
        store.add_mailbox("Archive", MailboxRole::Archive);
        store.add_message("INBOX", MessageId::new("uid:7")).unwrap();
        store
    }

    #[test]
    fn trash_moves_message_to_special_mailbox_and_audits_it() {
        let mut store = store();
        let id = MessageId::new("uid:7");

        store.trash(&id).unwrap();

        assert_eq!(store.mailbox_of(&id), Some("Trash"));
        assert_eq!(
            store.events(),
            &[MailEvent::Trashed {
                message: id,
                from: "INBOX".into(),
                to: "Trash".into(),
            }]
        );
    }

    #[test]
    fn restore_moves_a_trashed_message_to_a_normal_mailbox() {
        let mut store = store();
        let id = MessageId::new("uid:7");
        store.trash(&id).unwrap();

        store.restore(&id, "Archive").unwrap();

        assert_eq!(store.mailbox_of(&id), Some("Archive"));
        assert!(matches!(
            store.events().last(),
            Some(MailEvent::Restored { .. })
        ));
    }

    #[test]
    fn restore_refuses_to_target_trash() {
        let mut store = store();
        let id = MessageId::new("uid:7");

        assert_eq!(
            store.restore(&id, "Trash"),
            Err(MailError::DestinationIsTrash("Trash".into()))
        );
    }

    #[test]
    fn trash_requires_a_discovered_trash_mailbox() {
        let mut store = InMemoryMailStore::new();
        store.add_mailbox("INBOX", MailboxRole::Inbox);
        let id = MessageId::new("uid:7");
        store.add_message("INBOX", id.clone()).unwrap();

        assert_eq!(store.trash(&id), Err(MailError::TrashMailboxMissing));
    }
}
pub mod sync;
