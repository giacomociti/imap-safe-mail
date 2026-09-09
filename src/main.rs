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
    process::{Command as ProcessCommand, Stdio},
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
    /// Authentication scheme for the SPARQL endpoint
    #[arg(long, value_enum, requires = "sparql_auth_env")]
    sparql_auth: Option<SparqlAuthArg>,
    /// Environment variable holding the SPARQL credential. Bearer: token; Basic: username:password; Header: header value.
    #[arg(long, requires = "sparql_auth")]
    sparql_auth_env: Option<String>,
    /// Header name for `--sparql-auth header` (for example `X-API-Key`)
    #[arg(long, requires_if("header", "sparql_auth"))]
    sparql_header_name: Option<String>,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SparqlAuthArg {
    /// `Authorization: Bearer <token>`; this is QLever's documented update authentication.
    Bearer,
    /// HTTP Basic authentication; the environment variable must contain `username:password`.
    Basic,
    /// A custom header; provide its name with `--sparql-header-name`.
    Header,
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
    let endpoint_auth = if args.sparql_endpoint.is_some() {
        sparql_authentication(&args)?
    } else {
        None
    };
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
        apply_update(&endpoint, &args.output, endpoint_auth.as_ref())?;
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

struct SparqlAuthentication {
    config_line: String,
}

fn sparql_authentication(
    args: &SyncArgs,
) -> Result<Option<SparqlAuthentication>, Box<dyn std::error::Error + Send + Sync>> {
    let Some(kind) = args.sparql_auth else {
        return Ok(None);
    };
    let variable = args
        .sparql_auth_env
        .as_deref()
        .expect("clap requires --sparql-auth-env");
    let secret = env::var(variable).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("SPARQL authentication environment variable {variable:?} is not set"),
        )
    })?;
    if secret.contains(['\r', '\n']) {
        return Err("SPARQL authentication values must not contain newlines".into());
    }
    let config_line = match kind {
        SparqlAuthArg::Bearer => format!(
            "header = \"Authorization: Bearer {}\"\n",
            curl_config_escape(&secret)
        ),
        SparqlAuthArg::Basic => format!("user = \"{}\"\n", curl_config_escape(&secret)),
        SparqlAuthArg::Header => {
            let name = args
                .sparql_header_name
                .as_deref()
                .ok_or("--sparql-header-name is required with --sparql-auth header")?;
            if name.contains(['\r', '\n', ':']) {
                return Err("SPARQL header names must not contain colons or newlines".into());
            }
            format!("header = \"{name}: {}\"\n", curl_config_escape(&secret))
        }
    };
    Ok(Some(SparqlAuthentication { config_line }))
}

fn curl_config_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn apply_update(
    endpoint: &str,
    update_path: &PathBuf,
    authentication: Option<&SparqlAuthentication>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut child = ProcessCommand::new("curl")
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
        // `curl --config -` reads a config from stdin. It keeps credentials out
        // of the process command line and avoids writing a temporary secret file.
        .args(
            authentication
                .map(|_| ["--config", "-"])
                .into_iter()
                .flatten(),
        )
        .stdin(
            authentication
                .is_some()
                .then_some(Stdio::piped())
                .unwrap_or_else(Stdio::null),
        )
        .spawn()?;
    if let Some(authentication) = authentication {
        child
            .stdin
            .as_mut()
            .expect("piped curl stdin")
            .write_all(authentication.config_line.as_bytes())?;
    }
    let status = child.wait()?;
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
