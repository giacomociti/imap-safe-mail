use clap::{Args, Parser, Subcommand, ValueEnum};
use imap_safe_mail::{
    imap::{Authentication, MailReader},
    rdf::{RdfOptions, message_to_nquads},
    sync::{read_cursor, sparql_update, write_cursor},
};
use std::{
    env,
    fs::File,
    io::{self, BufWriter, Write},
    path::PathBuf,
    process::Command as ProcessCommand,
};

#[derive(Parser, Debug)]
#[command(
    name = "imap-safe-mail",
    about = "Fetch recent IMAP messages and save mbox-rdf-compatible N-Quads"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Fetch the most recent messages from one IMAP mailbox.
    Fetch(FetchArgs),
    /// Synchronize a mailbox to a SPARQL 1.1 Update document using QRESYNC when available.
    Sync(SyncArgs),
}

#[derive(Args, Debug)]
struct SyncArgs {
    /// IMAP hostname
    #[arg(long)]
    host: String,
    /// Implicit-TLS IMAP port
    #[arg(long, default_value_t = 993)]
    port: u16,
    /// IMAP login name, usually an email address
    #[arg(long)]
    username: String,
    /// Authentication mechanism advertised by the IMAP server
    #[arg(long, value_enum, default_value_t = AuthArg::Login)]
    auth: AuthArg,
    /// Environment variable holding a password or OAuth access token
    #[arg(long, default_value = "IMAP_AUTH_SECRET")]
    secret_env: String,
    /// Mailbox to synchronize
    #[arg(long, default_value = "INBOX")]
    mailbox: String,
    /// Persistent per-mailbox QRESYNC cursor; a missing file starts a full sync
    #[arg(long, default_value = ".imap-safe-mail/INBOX.cursor")]
    state: PathBuf,
    /// Write a SPARQL 1.1 Update document here. Send it to your triplestore's update endpoint.
    #[arg(short, long, default_value = "mail.ru")]
    output: PathBuf,
    /// SPARQL 1.1 Update endpoint. When set, the CLI applies the update with curl and only then advances the QRESYNC cursor.
    #[arg(long)]
    sparql_endpoint: Option<String>,
    /// Base IRI for generated message, folder, and attachment identifiers
    #[arg(long, default_value = "https://example.org/data/")]
    data_iri: String,
    /// Named graph IRI. Defaults to urn:email:<username>.
    #[arg(long)]
    graph: Option<String>,
    /// Include the RFC 822 body as mail:bodyText (may include sensitive content)
    #[arg(long, default_value_t = false)]
    include_body: bool,
}

#[derive(Args, Debug)]
struct FetchArgs {
    /// IMAP hostname (for Gmail: imap.gmail.com)
    #[arg(long)]
    host: String,

    /// Implicit-TLS IMAP port
    #[arg(long, default_value_t = 993)]
    port: u16,

    /// IMAP login name, usually an email address
    #[arg(long)]
    username: String,

    /// Authentication mechanism advertised by the IMAP server
    #[arg(long, value_enum, default_value_t = AuthArg::Login)]
    auth: AuthArg,

    /// Environment variable holding a password or OAuth access token
    #[arg(long, default_value = "IMAP_AUTH_SECRET")]
    secret_env: String,

    /// Mailbox to fetch from
    #[arg(long, default_value = "INBOX")]
    mailbox: String,

    /// Maximum number of newest messages to retrieve
    #[arg(long, default_value_t = 50)]
    limit: usize,

    /// Output N-Quads path
    #[arg(short, long, default_value = "mail.nq")]
    output: PathBuf,

    /// Base IRI for generated message, folder, and attachment identifiers
    #[arg(long, default_value = "https://example.org/data/")]
    data_iri: String,

    /// Named graph IRI. Defaults to urn:email:<username>.
    #[arg(long)]
    graph: Option<String>,

    /// Include the RFC 822 body as mail:bodyText (may include sensitive content)
    #[arg(long, default_value_t = false)]
    include_body: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AuthArg {
    Login,
    Plain,
    Xoauth2,
    Oauthbearer,
}

impl From<AuthArg> for Authentication {
    fn from(value: AuthArg) -> Self {
        match value {
            AuthArg::Login => Self::Login,
            AuthArg::Plain => Self::Plain,
            AuthArg::Xoauth2 => Self::XOAuth2,
            AuthArg::Oauthbearer => Self::OAuthBearer,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Fetch(args) => fetch(args),
        Command::Sync(args) => sync(args),
    }
}

fn sync(args: SyncArgs) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let secret = env::var(&args.secret_env).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "authentication-secret environment variable {:?} is not set",
                args.secret_env
            ),
        )
    })?;
    let graph = args
        .graph
        .unwrap_or_else(|| format!("urn:email:{}", args.username));
    let cursor = read_cursor(&args.state)?;
    let mut reader = MailReader::connect_with_auth(
        &args.host,
        args.port,
        &args.username,
        &secret,
        args.auth.into(),
    )?;
    let result = reader.sync_mailbox(&args.mailbox, cursor.map(|cursor| cursor.mailbox_state()))?;
    let rdf_options = RdfOptions {
        data_iri: args.data_iri,
        graph: None,
        include_body: args.include_body,
    };
    let update = sparql_update(&args.username, &args.mailbox, &graph, &result, &rdf_options);
    let mut output = BufWriter::new(File::create(&args.output)?);
    output.write_all(update.as_bytes())?;
    output.flush()?;
    // Never advance an IMAP cursor just because a local file was created. The
    // cursor represents what the triplestore has accepted, not what was fetched.
    if let Some(endpoint) = args.sparql_endpoint {
        apply_update(&endpoint, &args.output)?;
        if result.state.highest_mod_sequence.is_some() {
            write_cursor(&args.state, result.state)?;
        } else {
            eprintln!(
                "Server did not provide HIGHESTMODSEQ; no cursor was saved and the next run will be a full refresh."
            );
        }
    } else {
        eprintln!(
            "No --sparql-endpoint was given; the cursor was not advanced. Apply {} and rerun with --sparql-endpoint to make an acknowledged incremental sync.",
            args.output.display()
        );
    }
    reader.logout()?;
    let mode = if result.used_qresync {
        "QRESYNC delta"
    } else {
        "full refresh (QRESYNC unavailable, first run, or UIDVALIDITY changed)"
    };
    println!(
        "Wrote {} changed message(s) and {} vanished UID(s) as {mode} to {}",
        result.changed.len(),
        result.vanished.len(),
        args.output.display()
    );
    Ok(())
}

fn apply_update(
    endpoint: &str,
    update_path: &PathBuf,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let status = ProcessCommand::new("curl")
        .args([
            "--fail-with-body",
            "--silent",
            "--show-error",
            "--request",
            "POST",
            "--header",
            "Content-Type: application/sparql-update",
            "--data-binary",
        ])
        .arg(format!("@{}", update_path.display()))
        .arg(endpoint)
        .status()?;
    if !status.success() {
        return Err(
            format!("SPARQL endpoint rejected the update (curl exited with {status})").into(),
        );
    }
    Ok(())
}

fn fetch(args: FetchArgs) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let secret = env::var(&args.secret_env).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "authentication-secret environment variable {:?} is not set",
                args.secret_env
            ),
        )
    })?;
    let graph = args
        .graph
        .unwrap_or_else(|| format!("urn:email:{}", args.username));
    let mut reader = MailReader::connect_with_auth(
        &args.host,
        args.port,
        &args.username,
        &secret,
        args.auth.into(),
    )?;
    let uids = reader.recent_uids(&args.mailbox, args.limit)?;
    let mut output = BufWriter::new(File::create(&args.output)?);
    let rdf_options = RdfOptions {
        data_iri: args.data_iri,
        graph: Some(graph),
        include_body: args.include_body,
    };
    let mut fetched = 0usize;

    for uid in uids {
        let message = reader.fetch_message(&args.mailbox, uid)?;
        output.write_all(message_to_nquads(&message, &rdf_options).as_bytes())?;
        fetched += 1;
    }
    output.flush()?;
    reader.logout()?;
    println!("Wrote {fetched} messages to {}", args.output.display());
    Ok(())
}
