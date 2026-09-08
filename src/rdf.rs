//! RDF projection for messages fetched from IMAP.
//!
//! Namespaces and property names are intentionally kept identical to the
//! companion `mbox-rdf` project. Output is N-Triples when `graph` is absent
//! and N-Quads when it is provided.

use crate::imap::FetchedMessage;

pub const MAIL: &str = "https://mail.described.at/";
pub const SCHEMA: &str = "http://schema.org/";
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
pub const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

#[derive(Clone, Debug)]
pub struct RdfOptions {
    /// Base for generated instance IRIs; matches `mbox-rdf`'s `data_iri`.
    pub data_iri: String,
    /// Optional per-account named graph, matching the `mbox-rdf` convention.
    pub graph: Option<String>,
    pub include_body: bool,
}

impl Default for RdfOptions {
    fn default() -> Self {
        Self { data_iri: "https://example.org/data/".into(), graph: None, include_body: false }
    }
}

impl RdfOptions {
    fn data_base(&self) -> String {
        format!("{}/", self.data_iri.trim_end_matches('/'))
    }
}

/// Convert one fetched RFC 822 message into `mbox-rdf`-compatible RDF.
pub fn message_to_nquads(message: &FetchedMessage, options: &RdfOptions) -> String {
    let headers = Headers::parse(&message.rfc822);
    let base = options.data_base();
    let message_id = headers.get("message-id");
    let subject = headers.get("subject");
    let message_iri = match message_id {
        Some(id) if !id.is_empty() => format!("{base}msg/{}", percent_encode(id)),
        // IMAP UID is stable inside its mailbox UIDVALIDITY domain; the source
        // includes that scope so consumers can reconcile it if desired.
        _ => format!("{base}msg/imap/{}/{}", percent_encode(&message.source.account), message.uid),
    };
    let folder_iri = format!("{base}folder/{}", percent_encode(&message.source.mailbox));
    let source = message.source_uri();
    let mut lines = Vec::new();

    iri(&mut lines, &message_iri, RDF_TYPE, &format!("{MAIL}Message"), options);
    iri(&mut lines, &message_iri, RDF_TYPE, &format!("{SCHEMA}CreativeWork"), options);
    iri(&mut lines, &message_iri, &format!("{MAIL}folder"), &folder_iri, options);
    iri(&mut lines, &folder_iri, RDF_TYPE, &format!("{MAIL}Mailbox"), options);
    literal(&mut lines, &message_iri, &format!("{MAIL}sourcePath"), &source, None, options);
    literal(&mut lines, &message_iri, &format!("{MAIL}size"), &message.rfc822.len().to_string(), Some(XSD_INTEGER), options);

    if let Some(id) = message_id {
        literal(&mut lines, &message_iri, &format!("{MAIL}messageId"), id, None, options);
        iri(&mut lines, &message_iri, &format!("{MAIL}mid"), &mid_iri(id), options);
    }
    if let Some(value) = subject {
        literal(&mut lines, &message_iri, &format!("{MAIL}subject"), value, None, options);
        literal(&mut lines, &message_iri, &format!("{MAIL}normalizedSubject"), &normalize_subject(value), None, options);
    }
    for (header, predicate) in [("from", "from"), ("to", "to"), ("cc", "cc"), ("bcc", "bcc"), ("reply-to", "replyTo")] {
        if let Some(value) = headers.get(header) {
            for address in addresses(value) {
                let account = format!("mailto:{}", percent_encode(&address.to_lowercase()));
                iri(&mut lines, &message_iri, &format!("{MAIL}{predicate}"), &account, options);
                iri(&mut lines, &account, RDF_TYPE, &format!("{MAIL}Account"), options);
                literal(&mut lines, &account, &format!("{SCHEMA}email"), &address, None, options);
                if let Some((_, domain)) = address.rsplit_once('@') {
                    literal(&mut lines, &account, &format!("{MAIL}domain"), &domain.to_lowercase(), None, options);
                }
            }
        }
    }
    if let Some(agent) = headers.get("user-agent").or_else(|| headers.get("x-mailer")) {
        literal(&mut lines, &message_iri, &format!("{MAIL}userAgent"), agent, None, options);
    }
    if options.include_body {
        if let Some(body) = headers.body_text() {
            literal(&mut lines, &message_iri, &format!("{MAIL}bodyText"), body, None, options);
        }
    }

    lines.join("\n") + "\n"
}

fn iri(lines: &mut Vec<String>, s: &str, p: &str, o: &str, options: &RdfOptions) {
    lines.push(match &options.graph {
        Some(g) => format!("<{s}> <{p}> <{o}> <{g}> ."),
        None => format!("<{s}> <{p}> <{o}> ."),
    });
}

fn literal(lines: &mut Vec<String>, s: &str, p: &str, value: &str, datatype: Option<&str>, options: &RdfOptions) {
    let object = match datatype {
        Some(kind) => format!("\"{}\"^^<{kind}>", escape(value)),
        None => format!("\"{}\"", escape(value)),
    };
    lines.push(match &options.graph {
        Some(g) => format!("<{s}> <{p}> {object} <{g}> ."),
        None => format!("<{s}> <{p}> {object} ."),
    });
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn percent_encode(value: &str) -> String {
    value.bytes().flat_map(|byte| match byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => vec![(byte as char).to_string()],
        _ => vec![format!("%{byte:02X}")],
    }).collect()
}

fn mid_iri(value: &str) -> String {
    let safe = value.bytes().map(|byte| {
        (byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':' | b'@'))
            .then(|| (byte as char).to_string())
            .unwrap_or_else(|| format!("%{byte:02X}"))
    }).collect::<String>();
    format!("mid:{safe}")
}

fn normalize_subject(subject: &str) -> String {
    let mut result = subject.trim();
    while let Some((prefix, rest)) = result.split_once(':') {
        if matches!(prefix.trim().to_ascii_lowercase().as_str(), "re" | "fwd" | "fw" | "aw") {
            result = rest.trim();
        } else { break; }
    }
    result.to_owned()
}

fn addresses(value: &str) -> Vec<String> {
    value.split(',').filter_map(|part| {
        let trimmed = part.trim();
        let candidate = trimmed.rsplit_once('<').map(|(_, v)| v.trim_end_matches('>').trim()).unwrap_or(trimmed);
        candidate.contains('@').then_some(candidate.to_owned())
    }).collect()
}

struct Headers<'a> { entries: Vec<(String, String)>, body: &'a [u8] }
impl<'a> Headers<'a> {
    fn parse(raw: &'a [u8]) -> Self {
        let split = raw.windows(4).position(|v| v == b"\r\n\r\n").map(|i| (i, 4))
            .or_else(|| raw.windows(2).position(|v| v == b"\n\n").map(|i| (i, 2)))
            .unwrap_or((raw.len(), 0));
        let text = String::from_utf8_lossy(&raw[..split.0]);
        let mut entries: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            if line.starts_with(' ') || line.starts_with('\t') {
                if let Some((_, value)) = entries.last_mut() { value.push(' '); value.push_str(line.trim()); }
            } else if let Some((name, value)) = line.split_once(':') {
                entries.push((name.to_ascii_lowercase(), value.trim().to_owned()));
            }
        }
        Self { entries, body: &raw[split.0 + split.1..] }
    }
    fn get(&self, name: &str) -> Option<&str> { self.entries.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str()) }
    fn body_text(&self) -> Option<&str> { std::str::from_utf8(self.body).ok().filter(|body| !body.is_empty()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap::{FetchedMessage, ImapSource};

    #[test]
    fn projection_uses_the_mbox_rdf_namespaces_and_named_graphs() {
        let message = FetchedMessage {
            uid: 9,
            source: ImapSource { account: "alice@example.org".into(), mailbox: "INBOX".into() },
            rfc822: b"Message-ID: <a@example.org>\r\nSubject: Re: Hello\r\nFrom: Alice <alice@example.org>\r\n\r\nBody".to_vec(),
        };
        let rdf = message_to_nquads(&message, &RdfOptions { graph: Some("urn:email:alice@example.org".into()), include_body: true, ..Default::default() });
        assert!(rdf.contains("<https://mail.described.at/Message>"));
        assert!(rdf.contains("<http://schema.org/CreativeWork>"));
        assert!(rdf.contains("<urn:email:alice@example.org> ."));
        assert!(rdf.contains("\"Hello\""));
        assert!(rdf.contains("\"Body\""));
    }
}
