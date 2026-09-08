//! Read-only IMAP primitives for retrieving raw RFC 822 messages.
//!
//! `ImapSession` deliberately owns no connection setup or authentication.
//! Give it an already-authenticated, TLS-protected `Read + Write` transport;
//! this keeps credentials and TLS policy outside the domain API.

use std::io::{self, BufRead, BufReader, Read, Write};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImapSource { pub account: String, pub mailbox: String }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedMessage { pub uid: u32, pub source: ImapSource, pub rfc822: Vec<u8> }

impl FetchedMessage {
    pub fn source_uri(&self) -> String {
        format!("imap://{}/{};uid={}", self.source.account, self.source.mailbox, self.uid)
    }
}

/// A minimal protocol client for listing mailboxes and fetching one message by UID.
/// It sends only `LIST`, `SELECT`, and `UID FETCH ... BODY.PEEK[]` commands.
pub struct ImapSession<T: Read + Write> { transport: BufReader<T>, next_tag: u32 }

impl<T: Read + Write> ImapSession<T> {
    pub fn new(transport: T) -> Self { Self { transport: BufReader::new(transport), next_tag: 1 } }

    pub fn list_mailboxes(&mut self) -> io::Result<Vec<String>> {
        let lines = self.command("LIST \"\" \"*\"")?;
        Ok(lines.into_iter().filter(|line| line.starts_with("* LIST")).filter_map(|line| mailbox_name(&line)).collect())
    }

    pub fn fetch_message(&mut self, source: ImapSource, uid: u32) -> io::Result<FetchedMessage> {
        self.command(&format!("SELECT {}", quote(&source.mailbox)))?;
        let tag = self.next_tag();
        let command = format!("{tag} UID FETCH {uid} (UID RFC822.SIZE BODY.PEEK[])\r\n");
        self.transport.get_mut().write_all(command.as_bytes())?;
        self.transport.get_mut().flush()?;
        let raw = self.read_fetch_response(&tag)?;
        Ok(FetchedMessage { uid, source, rfc822: raw })
    }

    fn command(&mut self, command: &str) -> io::Result<Vec<String>> {
        let tag = self.next_tag();
        self.transport.get_mut().write_all(format!("{tag} {command}\r\n").as_bytes())?;
        self.transport.get_mut().flush()?;
        let mut lines = Vec::new();
        loop {
            let line = self.read_line()?;
            if line.starts_with(&tag) {
                if line.contains(" OK") { return Ok(lines); }
                return Err(io::Error::other(format!("IMAP command failed: {line}")));
            }
            lines.push(line);
        }
    }

    fn read_fetch_response(&mut self, tag: &str) -> io::Result<Vec<u8>> {
        loop {
            let line = self.read_line()?;
            if line.starts_with(tag) { return Err(io::Error::other("IMAP FETCH completed without an RFC 822 literal")); }
            if let Some(size) = literal_size(&line) {
                let mut raw = vec![0; size];
                self.transport.read_exact(&mut raw)?;
                // Consume the closing FETCH line and tagged completion.
                loop {
                    let trailing = self.read_line()?;
                    if trailing.starts_with(tag) {
                        if trailing.contains(" OK") { return Ok(raw); }
                        return Err(io::Error::other(format!("IMAP FETCH failed: {trailing}")));
                    }
                }
            }
        }
    }

    fn next_tag(&mut self) -> String { let tag = format!("A{:04}", self.next_tag); self.next_tag += 1; tag }
    fn read_line(&mut self) -> io::Result<String> { let mut line = String::new(); self.transport.read_line(&mut line)?; Ok(line.trim_end_matches(['\r', '\n']).to_owned()) }
}

fn quote(value: &str) -> String { format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")) }
fn literal_size(line: &str) -> Option<usize> { line.rsplit_once('{').and_then(|(_, rest)| rest.strip_suffix('}')).and_then(|size| size.parse().ok()) }
fn mailbox_name(line: &str) -> Option<String> { line.rsplit_once('"').and_then(|(before, _)| before.rsplit_once('"')).map(|(_, name)| name.to_owned()) }

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct MockTransport { reader: Cursor<Vec<u8>>, written: Vec<u8> }
    impl MockTransport {
        fn responding(bytes: Vec<u8>) -> Self { Self { reader: Cursor::new(bytes), written: Vec::new() } }
    }
    impl Read for MockTransport {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> { self.reader.read(buffer) }
    }
    impl Write for MockTransport {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> { self.written.extend_from_slice(buffer); Ok(buffer.len()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    #[test]
    fn fetches_raw_message_without_marking_it_seen() {
        let raw = b"Message-ID: <a@b>\r\n\r\nHello";
        let mut response = b"* 1 EXISTS\r\nA0001 OK selected\r\n* 1 FETCH (UID 7 RFC822.SIZE ".to_vec();
        response.extend_from_slice(raw.len().to_string().as_bytes());
        response.extend_from_slice(b" BODY[] {");
        response.extend_from_slice(raw.len().to_string().as_bytes());
        response.extend_from_slice(b"}\r\n");
        response.extend_from_slice(raw);
        response.extend_from_slice(b"\r\n)\r\nA0002 OK done\r\n");
        let transport = MockTransport::responding(response);
        let mut session = ImapSession::new(transport);
        let message = session.fetch_message(ImapSource { account: "a@example.org".into(), mailbox: "INBOX".into() }, 7).unwrap();

        assert_eq!(message.rfc822, raw);
        let transport = session.transport.into_inner();
        assert!(String::from_utf8(transport.written).unwrap().contains("UID FETCH 7 (UID RFC822.SIZE BODY.PEEK[])"));
    }
}
