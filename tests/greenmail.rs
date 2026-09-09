#![cfg(feature = "greenmail-tests")]

use imap_safe_mail::imap::MailReader;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

#[test]
fn fetches_a_real_message_from_greenmail_over_imaps_without_marking_it_seen() {
    // A unique recipient gives every run a fresh GreenMail mailbox, making
    // the fixture's UID deterministically 1 even when the container persists.
    let user = format!("reader-{}@localhost", std::process::id());
    send_fixture_message(&user);

    let mut reader =
        MailReader::connect_unverified_greenmail("127.0.0.1", 3993, &user, "test-password")
            .expect("connect to the local GreenMail IMAPS service");
    let mailboxes = reader.list_mailboxes().expect("list mailboxes");
    assert!(
        mailboxes
            .iter()
            .any(|mailbox| mailbox.eq_ignore_ascii_case("INBOX"))
    );

    // The Compose service is created afresh for this suite, so the injected
    // message has UID 1. Fetching uses BODY.PEEK[] in MailReader.
    let message = reader
        .fetch_message("INBOX", 1)
        .expect("fetch fixture by UID");
    let raw = String::from_utf8(message.rfc822).expect("fixture is UTF-8");
    assert!(!message.is_seen, "BODY.PEEK[] must not set the \\Seen flag");
    assert!(raw.contains("Subject: GreenMail integration fixture"));
    assert!(raw.contains("A message retrieved through real IMAPS."));
    reader.logout().expect("logout");
}

fn send_fixture_message(recipient: &str) {
    let stream = TcpStream::connect("127.0.0.1:3025").expect("connect to GreenMail SMTP");
    let mut smtp = BufReader::new(stream);
    expect_ok(&mut smtp);
    command(&mut smtp, "EHLO localhost");
    command(&mut smtp, "MAIL FROM:<sender@localhost>");
    command(&mut smtp, &format!("RCPT TO:<{recipient}>"));
    command(&mut smtp, "DATA");
    smtp.get_mut().write_all(format!("From: sender@localhost\r\nTo: {recipient}\r\nSubject: GreenMail integration fixture\r\nMessage-ID: <greenmail-fixture@localhost>\r\n\r\nA message retrieved through real IMAPS.\r\n.\r\n").as_bytes()).expect("send fixture");
    smtp.get_mut().flush().expect("flush fixture");
    expect_ok(&mut smtp);
    command(&mut smtp, "QUIT");
}

fn command(stream: &mut BufReader<TcpStream>, command: &str) {
    stream
        .get_mut()
        .write_all(format!("{command}\r\n").as_bytes())
        .expect("write SMTP command");
    stream.get_mut().flush().expect("flush SMTP command");
    expect_ok(stream);
}

fn expect_ok(stream: &mut BufReader<TcpStream>) {
    let mut line = String::new();
    stream.read_line(&mut line).expect("read SMTP response");
    assert!(
        line.starts_with('2') || line.starts_with('3'),
        "unexpected SMTP response: {line}"
    );
    // EHLO is a multi-line response. Drain it until the final `250 ` line.
    while line.starts_with("250-") {
        line.clear();
        stream.read_line(&mut line).expect("read SMTP capability");
    }
}
