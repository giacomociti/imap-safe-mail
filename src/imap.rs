//! A deliberately narrow wrapper around the `imap` crate.
//!
//! The underlying client handles IMAP framing, literals, TLS, and protocol
//! details. This module exposes only read operations; callers never receive
//! the underlying `imap::Session`, so they cannot issue `EXPUNGE` or raw
//! `\Deleted` commands through this API.

use std::error::Error;

pub type ImapResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImapSource {
    pub account: String,
    pub mailbox: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedMessage {
    pub uid: u32,
    pub source: ImapSource,
    pub rfc822: Vec<u8>,
}

impl FetchedMessage {
    pub fn source_uri(&self) -> String {
        format!("imap://{}/{};uid={}", self.source.account, self.source.mailbox, self.uid)
    }
}

/// Authenticated, TLS-protected IMAP client restricted to mailbox discovery
/// and message retrieval.
pub struct MailReader {
    account: String,
    session: imap::Session<imap::Connection>,
}

impl MailReader {
    /// Establish a TLS connection with certificate validation and log in.
    /// Rustls is selected explicitly; invalid certificates are never accepted.
    pub fn connect(host: &str, port: u16, username: &str, password: &str) -> ImapResult<Self> {
        let client = imap::ClientBuilder::new(host, port)
            .tls_kind(imap::TlsKind::Rust)
            .connect()?;
        let session = client.login(username, password).map_err(|failure| failure.0)?;
        Ok(Self { account: username.to_owned(), session })
    }

    /// Discover server mailbox names. The implementation does not alter them.
    pub fn list_mailboxes(&mut self) -> ImapResult<Vec<String>> {
        Ok(self.session.list(Some(""), Some("*"))?
            .iter()
            .map(|mailbox| mailbox.name().to_owned())
            .collect())
    }

    /// Fetch one RFC 822 message by UID without setting `\Seen`.
    pub fn fetch_message(&mut self, mailbox: impl Into<String>, uid: u32) -> ImapResult<FetchedMessage> {
        let mailbox = mailbox.into();
        self.session.select(&mailbox)?;
        let fetched = self.session.uid_fetch(uid.to_string(), "UID RFC822.SIZE BODY.PEEK[]")?;
        let item = fetched.iter().next().ok_or_else(|| format!("UID {uid} was not found in {mailbox}"))?;
        let rfc822 = item.body().ok_or_else(|| format!("server did not return RFC 822 data for UID {uid}"))?.to_vec();
        Ok(FetchedMessage { uid, source: ImapSource { account: self.account.clone(), mailbox }, rfc822 })
    }

    /// End the authenticated TLS session cleanly.
    pub fn logout(mut self) -> ImapResult<()> {
        self.session.logout()?;
        Ok(())
    }
}
