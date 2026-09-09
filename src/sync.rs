//! On-disk QRESYNC cursor and SPARQL Update serialization.

use crate::{
    imap::{MailboxState, MailboxSync},
    rdf::{
        RdfOptions, delete_mailbox_update, delete_message_update, delete_uid_update,
        insert_message_update,
    },
};
use std::{fs, io, path::Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncCursor {
    pub uid_validity: u32,
    pub highest_mod_sequence: u64,
}

impl SyncCursor {
    pub fn mailbox_state(self) -> MailboxState {
        MailboxState {
            uid_validity: self.uid_validity,
            highest_mod_sequence: Some(self.highest_mod_sequence),
        }
    }
}

/// Read the cursor. A missing file intentionally represents the first, full sync.
pub fn read_cursor(path: &Path) -> io::Result<Option<SyncCursor>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut uid_validity = None;
    let mut highest_mod_sequence = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("uid_validity=") {
            uid_validity = value.parse().ok();
        }
        if let Some(value) = line.strip_prefix("highest_mod_sequence=") {
            highest_mod_sequence = value.parse().ok();
        }
    }
    match (uid_validity, highest_mod_sequence) {
        (Some(uid_validity), Some(highest_mod_sequence)) => Ok(Some(SyncCursor {
            uid_validity,
            highest_mod_sequence,
        })),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid IMAP sync cursor",
        )),
    }
}

pub fn write_cursor(path: &Path, state: MailboxState) -> io::Result<()> {
    let highest = state.highest_mod_sequence.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "server did not provide HIGHESTMODSEQ; cannot safely create a QRESYNC cursor",
        )
    })?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(
        &temporary,
        format!(
            "version=1\nuid_validity={}\nhighest_mod_sequence={highest}\n",
            state.uid_validity
        ),
    )?;
    fs::rename(temporary, path)
}

/// Make a transactional SPARQL 1.1 Update document for one IMAP response.
pub fn sparql_update(
    account: &str,
    mailbox: &str,
    graph: &str,
    sync: &MailboxSync,
    options: &RdfOptions,
) -> String {
    let mut update = String::new();
    if sync.full_refresh {
        update.push_str(&delete_mailbox_update(account, mailbox, graph));
    }
    for uid in &sync.vanished {
        update.push_str(&delete_uid_update(account, mailbox, *uid, graph));
    }
    for message in &sync.changed {
        update.push_str(&delete_message_update(message, graph));
        update.push_str(&insert_message_update(message, graph, options));
    }
    update
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap::{FetchedMessage, ImapSource};

    #[test]
    fn serializes_deletions_and_insertions_in_the_selected_graph() {
        let message = FetchedMessage {
            uid: 7,
            source: ImapSource {
                account: "me@example.test".into(),
                mailbox: "INBOX".into(),
            },
            rfc822: b"Subject: hi\r\n\r\n".to_vec(),
            is_seen: false,
            mod_sequence: Some(9),
        };
        let sync = MailboxSync {
            state: MailboxState {
                uid_validity: 4,
                highest_mod_sequence: Some(9),
            },
            changed: vec![message],
            vanished: vec![6],
            used_qresync: true,
            full_refresh: false,
        };
        let update = sparql_update(
            "me@example.test",
            "INBOX",
            "urn:test",
            &sync,
            &RdfOptions::default(),
        );
        assert!(update.contains("DELETE { GRAPH <urn:test>"));
        assert!(update.contains("uid=6"));
        assert!(update.contains("INSERT DATA { GRAPH <urn:test>"));
    }
}
