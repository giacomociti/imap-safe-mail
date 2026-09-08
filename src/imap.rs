//! A deliberately narrow wrapper around the `imap` crate.
//!
//! The underlying client handles IMAP framing, literals, TLS, and protocol
//! details. This module exposes only read operations; callers never receive
//! the underlying `imap::Session`, so they cannot issue `EXPUNGE` or raw
//! `\Deleted` commands through this API.

use std::error::Error;

pub type ImapResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Authentication mechanisms exposed by the safe client.
///
/// `XOAUTH2` is the widely deployed OAuth SASL variant used by Gmail and many
/// Exchange deployments. `OAUTHBEARER` is defined by RFC 7628.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authentication {
    Login,
    Plain,
    XOAuth2,
    OAuthBearer,
}

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
    /// State returned by the server in the same fetch response.
    pub is_seen: bool,
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
        Self::connect_with_auth(host, port, username, password, Authentication::Login)
    }

    /// Establish a TLS connection and authenticate using the selected SASL
    /// mechanism. `secret` is a password for `Login`/`Plain` or an OAuth access
    /// token for `XOAuth2`/`OAuthBearer`.
    pub fn connect_with_auth(host: &str, port: u16, username: &str, secret: &str, authentication: Authentication) -> ImapResult<Self> {
        Self::connect_inner(host, port, username, secret, authentication, false)
    }

    fn connect_inner(host: &str, port: u16, username: &str, secret: &str, authentication: Authentication, skip_tls_verify: bool) -> ImapResult<Self> {
        let client = imap::ClientBuilder::new(host, port)
            .tls_kind(imap::TlsKind::Rust)
            // Do not infer TLS from the port: local test servers and some
            // providers expose implicit TLS on a non-standard port.
            .mode(imap::ConnectionMode::Tls)
            .danger_skip_tls_verify(skip_tls_verify)
            .connect()?;
        let session = match authentication {
            Authentication::Login => client.login(username, secret).map_err(|failure| failure.0)?,
            Authentication::Plain => client.authenticate("PLAIN", &SaslResponse::plain(username, secret)).map_err(|failure| failure.0)?,
            Authentication::XOAuth2 => client.authenticate("XOAUTH2", &SaslResponse::xoauth2(username, secret)).map_err(|failure| failure.0)?,
            Authentication::OAuthBearer => client.authenticate("OAUTHBEARER", &SaslResponse::oauth_bearer(username, secret, host, port)).map_err(|failure| failure.0)?,
        };
        Ok(Self { account: username.to_owned(), session })
    }

    /// Connect to the local GreenMail test container, whose built-in certificate
    /// is intentionally self-signed. This method does not exist in normal builds.
    #[cfg(feature = "greenmail-tests")]
    pub fn connect_unverified_greenmail(host: &str, port: u16, username: &str, password: &str) -> ImapResult<Self> {
        Self::connect_inner(host, port, username, password, Authentication::Login, true)
    }

    /// Discover server mailbox names. The implementation does not alter them.
    pub fn list_mailboxes(&mut self) -> ImapResult<Vec<String>> {
        Ok(self.session.list(Some(""), Some("*"))?
            .iter()
            .map(|mailbox| mailbox.name().to_owned())
            .collect())
    }

    /// Return the newest message UIDs in a mailbox, newest first.
    pub fn recent_uids(&mut self, mailbox: &str, limit: usize) -> ImapResult<Vec<u32>> {
        self.session.select(mailbox)?;
        let mut uids: Vec<u32> = self.session.uid_search("ALL")?.into_iter().collect();
        uids.sort_unstable_by(|left, right| right.cmp(left));
        uids.truncate(limit);
        Ok(uids)
    }

    /// Fetch one RFC 822 message by UID without setting `\Seen`.
    pub fn fetch_message(&mut self, mailbox: impl Into<String>, uid: u32) -> ImapResult<FetchedMessage> {
        let mailbox = mailbox.into();
        self.session.select(&mailbox)?;
        // `uid_fetch` supplies the IMAP `UID FETCH` command itself; the query
        // only contains requested message data items.
        let fetched = self.session.uid_fetch(uid.to_string(), "(FLAGS RFC822.SIZE BODY.PEEK[])")?;
        let item = fetched.iter().next().ok_or_else(|| format!("UID {uid} was not found in {mailbox}"))?;
        let rfc822 = item.body().ok_or_else(|| format!("server did not return RFC 822 data for UID {uid}"))?.to_vec();
        let is_seen = item.flags().iter().any(|flag| matches!(flag, imap::types::Flag::Seen));
        Ok(FetchedMessage { uid, source: ImapSource { account: self.account.clone(), mailbox }, rfc822, is_seen })
    }

    /// End the authenticated TLS session cleanly.
    pub fn logout(mut self) -> ImapResult<()> {
        self.session.logout()?;
        Ok(())
    }
}

struct SaslResponse(Vec<u8>);

impl SaslResponse {
    fn plain(username: &str, password: &str) -> Self {
        Self(format!("\0{username}\0{password}").into_bytes())
    }

    fn xoauth2(username: &str, token: &str) -> Self {
        Self(format!("user={username}\x01auth=Bearer {token}\x01\x01").into_bytes())
    }

    fn oauth_bearer(username: &str, token: &str, host: &str, port: u16) -> Self {
        Self(format!("n,a={username},\x01host={host}\x01port={port}\x01auth=Bearer {token}\x01\x01").into_bytes())
    }
}

impl imap::Authenticator for SaslResponse {
    type Response = Vec<u8>;

    fn process(&self, _challenge: &[u8]) -> Self::Response {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_standard_sasl_authentication_responses() {
        assert_eq!(SaslResponse::plain("alice", "secret").0, b"\0alice\0secret");
        assert_eq!(SaslResponse::xoauth2("alice@example.org", "token").0, b"user=alice@example.org\x01auth=Bearer token\x01\x01");
        assert_eq!(SaslResponse::oauth_bearer("alice@example.org", "token", "imap.example.org", 993).0, b"n,a=alice@example.org,\x01host=imap.example.org\x01port=993\x01auth=Bearer token\x01\x01");
    }
}
