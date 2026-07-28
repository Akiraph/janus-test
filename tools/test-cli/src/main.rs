use std::{io::Read, path::PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use futures_util::StreamExt;
use rand::RngCore;
use reqwest::{Client, Method};

#[derive(Debug, Parser)]
#[command(name = "janus-test", about = "Janus public-interface test CLI")]
struct Cli {
    #[arg(long, env = "JANUS_BASE_URL", default_value = "http://127.0.0.1:4317")]
    base_url: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Health,
    Request(RequestArgs),
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    Projects {
        #[command(subcommand)]
        command: ProjectsCommand,
    },
    /// Terminal lifecycle smoke test over the public HTTP + WebSocket surface.
    ///
    /// Probes the bash pipe backend end-to-end: create a runtime + terminal,
    /// issue a ticket, upgrade to a WebSocket, write input, and read scrollback.
    Terminal {
        #[command(subcommand)]
        command: TerminalCommand,
    },
    /// Session lifecycle and message surface over the public HTTP API.
    Sessions {
        #[command(subcommand)]
        command: SessionsCommand,
    },
}

#[derive(Debug, Args)]
struct RequestArgs {
    method: String,
    path: String,
    #[arg(long)]
    json: Option<PathBuf>,
    /// Extra header, as `Name: value`. Repeatable.
    #[arg(long = "header", short = 'H', value_name = "NAME: VALUE")]
    headers: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum EventsCommand {
    /// Follow the live event stream, optionally resuming from a cursor and/or
    /// stopping after N frames.
    Follow {
        #[arg(long)]
        after: Option<u64>,
        #[arg(long)]
        count: Option<usize>,
    },
    /// Replay the retained event history within an opaque cursor range
    /// `[after, until)` and exit; a single-sided `after` replays to the head.
    Range {
        #[arg(long)]
        after: Option<u64>,
        #[arg(long)]
        until: Option<u64>,
        #[arg(long, default_value = "256")]
        limit: usize,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectsCommand {
    /// List projects (dev-auth only; no login cookie).
    List,
    /// Create a project from a public HTTPS repo.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long)]
        branch: Option<String>,
        /// Random idempotency key if omitted.
        #[arg(long)]
        idempotency_key: Option<String>,
    },
    /// Fetch a single project's projection.
    Get { id: String },
    /// Git status projection for a project.
    GitStatus { id: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let client = Client::builder().build()?;
    match cli.command {
        Command::Health => health(&client, &cli.base_url).await,
        Command::Request(args) => request(&client, &cli.base_url, args).await,
        Command::Events {
            command: EventsCommand::Follow { after, count },
        } => follow_events(&client, &cli.base_url, after, count).await,
        Command::Events {
            command: EventsCommand::Range { after, until, limit },
        } => events_range(&client, &cli.base_url, after, until, limit).await,
        Command::Projects { command } => match command {
            ProjectsCommand::List => projects_list(&client, &cli.base_url).await,
            ProjectsCommand::Create {
                name,
                url,
                branch,
                idempotency_key,
            } => projects_create(&client, &cli.base_url, name, url, branch, idempotency_key).await,
            ProjectsCommand::Get { id } => projects_get(&client, &cli.base_url, &id).await,
            ProjectsCommand::GitStatus { id } => {
                projects_git_status(&client, &cli.base_url, &id).await
            }
        },
        Command::Terminal { command } => match command {
            TerminalCommand::Create {
                runtime_id,
                owner_kind,
                owner_id,
            } => terminal_create(&client, &cli.base_url, runtime_id, owner_kind, owner_id).await,
            TerminalCommand::List {
                owner_kind,
                owner_id,
            } => terminal_list(&client, &cli.base_url, owner_kind, owner_id).await,
            TerminalCommand::Ticket { id } => {
                terminal_ticket(&client, &cli.base_url, &id).await
            }
            TerminalCommand::Scrollback { id, after, limit } => {
                terminal_scrollback(&client, &cli.base_url, &id, after, limit).await
            }
            TerminalCommand::Resize { id, cols, rows } => {
                terminal_resize(&client, &cli.base_url, &id, cols, rows).await
            }
            TerminalCommand::Signal { id, signal } => {
                terminal_signal(&client, &cli.base_url, &id, &signal).await
            }
            TerminalCommand::Close { id } => {
                terminal_close(&client, &cli.base_url, &id).await
            }
        },
        Command::Sessions { command } => match command {
            SessionsCommand::List { project_id } => {
                sessions_list(&client, &cli.base_url, &project_id).await
            }
            SessionsCommand::Create { project_id, title } => {
                sessions_create(&client, &cli.base_url, &project_id, title).await
            }
            SessionsCommand::Get { id } => sessions_get(&client, &cli.base_url, &id).await,
            SessionsCommand::Delete { id } => sessions_delete(&client, &cli.base_url, &id).await,
            SessionsCommand::PostMessage {
                id,
                content,
                expected_session_version,
            } => sessions_post_message(
                &client,
                &cli.base_url,
                &id,
                &content,
                &expected_session_version,
            )
            .await,
            SessionsCommand::Timeline {
                id,
                before,
                after,
                limit,
            } => sessions_timeline(&client, &cli.base_url, &id, before, after, limit).await,
            SessionsCommand::GetTurn { id, turn_id } => {
                sessions_get_turn(&client, &cli.base_url, &id, &turn_id).await
            }
            SessionsCommand::Diff { id } => sessions_diff(&client, &cli.base_url, &id).await,
        },
    }
}

async fn health(client: &Client, base_url: &str) -> anyhow::Result<()> {
    for path in ["/health/live", "/health/ready"] {
        let response = client.get(url(base_url, path)).send().await?;
        let status = response.status();
        let body: serde_json::Value = response.json().await?;
        println!("{}", serde_json::to_string(&body)?);
        if !status.is_success() {
            bail!("{path} returned {status}");
        }
    }
    Ok(())
}

async fn request(client: &Client, base_url: &str, args: RequestArgs) -> anyhow::Result<()> {
    let method = Method::from_bytes(args.method.as_bytes())
        .with_context(|| format!("invalid HTTP method {}", args.method))?;
    let mut request = client.request(method, url(base_url, &args.path));
    for header in &args.headers {
        let (name, value) = header
            .split_once(':')
            .map(|(n, v)| (n.trim(), v.trim()))
            .with_context(|| format!("header must be `Name: value`, got {header}"))?;
        request = request.header(name, value);
    }
    if let Some(path) = args.json {
        let body = read_body(&path)?;
        let json: serde_json::Value = serde_json::from_str(&body)
            .with_context(|| format!("parse JSON from {}", path.display()))?;
        request = request.json(&json);
    }
    let response = request.send().await?;
    let status = response.status();
    let body = response.text().await?;
    println!("{body}");
    if !status.is_success() {
        bail!("request returned {status}");
    }
    Ok(())
}

async fn projects_list(client: &Client, base_url: &str) -> anyhow::Result<()> {
    let response = client.get(url(base_url, "/api/v1/projects")).send().await?;
    print_response(response).await
}

async fn projects_create(
    client: &Client,
    base_url: &str,
    name: String,
    url_: String,
    branch: Option<String>,
    idempotency_key: Option<String>,
) -> anyhow::Result<()> {
    let key = idempotency_key.unwrap_or_else(|| {
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    });
    let body = serde_json::json!({
        "name": name,
        "repository": {
            "access": "public_https",
            "url": url_,
            "branch": branch,
        }
    });
    let response = client
        .post(url(base_url, "/api/v1/projects"))
        .header("Idempotency-Key", key)
        .json(&body)
        .send()
        .await?;
    print_response(response).await
}

async fn projects_get(client: &Client, base_url: &str, id: &str) -> anyhow::Result<()> {
    let response = client
        .get(url(base_url, &format!("/api/v1/projects/{id}")))
        .send()
        .await?;
    print_response(response).await
}

#[derive(Debug, Subcommand)]
enum TerminalCommand {
    /// Create a terminal under a ready Runtime.
    Create {
        runtime_id: String,
        owner_kind: String,
        owner_id: String,
    },
    /// List terminals owned by a project or session.
    List {
        owner_kind: String,
        owner_id: String,
    },
    /// Issue a one-use access ticket and print the raw token once.
    Ticket { id: String },
    /// Read scrollback bytes after an optional cursor.
    Scrollback {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value = "65536")]
        limit: usize,
    },
    /// Resize a terminal viewport.
    Resize { id: String, cols: u16, rows: u16 },
    /// Raise an abstract signal (ctrl_c | terminate) against the shell.
    Signal { id: String, signal: String },
    /// Close a terminal shell and print the finalized projection.
    Close { id: String },
}

#[derive(Debug, Subcommand)]
enum SessionsCommand {
    /// List sessions under a project.
    List { project_id: String },
    /// Create a new session with an optional title.
    Create {
        project_id: String,
        #[arg(long)]
        title: Option<String>,
    },
    /// Fetch a single session projection.
    Get { id: String },
    /// Delete a session.
    Delete { id: String },
    /// Post a message, advancing the session under `expected_session_version`.
    PostMessage {
        id: String,
        content: String,
        expected_session_version: String,
    },
    /// Read the session timeline (optionally bounded by opaque cursors).
    Timeline {
        id: String,
        #[arg(long)]
        before: Option<String>,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value = "50")]
        limit: i64,
    },
    /// Fetch the routed Turn for a session.
    GetTurn { id: String, turn_id: String },
    /// Read the session diff projection.
    Diff { id: String },
}

async fn terminal_create(
    client: &Client,
    base_url: &str,
    runtime_id: String,
    owner_kind: String,
    owner_id: String,
) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "runtime_id": runtime_id,
        "owner": { "kind": owner_kind, "id": owner_id },
        "working_directory": ".",
        "size": { "cols": 80, "rows": 24 },
    });
    let response = client
        .post(url(base_url, "/api/v1/terminals"))
        .json(&body)
        .send()
        .await?;
    print_response(response).await
}

async fn terminal_list(
    client: &Client,
    base_url: &str,
    owner_kind: String,
    owner_id: String,
) -> anyhow::Result<()> {
    let response = client
        .get(url(base_url, "/api/v1/terminals"))
        .query(&[("owner_kind", owner_kind.as_str()), ("owner_id", owner_id.as_str())])
        .send()
        .await?;
    print_response(response).await
}

async fn terminal_ticket(client: &Client, base_url: &str, id: &str) -> anyhow::Result<()> {
    // Tickets are origin-bound; the CLI mirrors the browser by sending the
    // configured public origin.
    let response = client
        .post(url(base_url, &format!("/api/v1/terminals/{id}/tickets")))
        .header("Origin", base_url.trim_end_matches('/'))
        .send()
        .await?;
    print_response(response).await
}

async fn terminal_resize(
    client: &Client,
    base_url: &str,
    id: &str,
    cols: u16,
    rows: u16,
) -> anyhow::Result<()> {
    let body = serde_json::json!({ "cols": cols, "rows": rows });
    let response = client
        .post(url(base_url, &format!("/api/v1/terminals/{id}/resize")))
        .json(&body)
        .send()
        .await?;
    print_response(response).await
}

async fn terminal_signal(
    client: &Client,
    base_url: &str,
    id: &str,
    signal: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({ "signal": signal });
    let response = client
        .post(url(base_url, &format!("/api/v1/terminals/{id}/signal")))
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    if !text.is_empty() {
        println!("{text}");
    }
    if !status.is_success() {
        bail!("signal returned {status}");
    }
    Ok(())
}

async fn terminal_close(client: &Client, base_url: &str, id: &str) -> anyhow::Result<()> {
    let response = client
        .post(url(base_url, &format!("/api/v1/terminals/{id}/close")))
        .send()
        .await?;
    print_response(response).await
}

async fn terminal_scrollback(
    client: &Client,
    base_url: &str,
    id: &str,
    after: Option<u64>,
    limit: usize,
) -> anyhow::Result<()> {
    let mut request = client.get(url(base_url, &format!("/api/v1/terminals/{id}/scrollback")));
    if let Some(cursor) = after {
        request = request.query(&[("after", cursor.to_string())]);
    }
    request = request.query(&[("limit", limit.to_string())]);
    let response = request.send().await?;
    print_response(response).await
}

async fn projects_git_status(client: &Client, base_url: &str, id: &str) -> anyhow::Result<()> {
    let response = client
        .get(url(base_url, &format!("/api/v1/projects/{id}/git/status")))
        .send()
        .await?;
    print_response(response).await
}

async fn print_response(response: reqwest::Response) -> anyhow::Result<()> {
    let status = response.status();
    let body = response.text().await?;
    println!("{body}");
    if !status.is_success() {
        bail!("response status {status}");
    }
    Ok(())
}

async fn follow_events(
    client: &Client,
    base_url: &str,
    after: Option<u64>,
    count: Option<usize>,
) -> anyhow::Result<()> {
    let mut request = client.get(url(base_url, "/api/v1/events"));
    if let Some(cursor) = after {
        request = request.query(&[("after", cursor.to_string())]);
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await?;
        bail!("events returned {status}: {body}");
    }

    let mut received = 0usize;
    let mut buffered = String::new();
    let mut bytes = response.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        buffered.push_str(&String::from_utf8_lossy(&chunk?));
        if buffered.contains("\r\n") {
            buffered = buffered.replace("\r\n", "\n");
        }
        while let Some(end) = buffered.find("\n\n") {
            let frame = buffered[..end].to_owned();
            buffered.drain(..end + 2);
            let data = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .collect::<Vec<_>>();
            if !data.is_empty() {
                println!("{}", data.join("\n"));
                received += 1;
            }
            if count.is_some_and(|limit| received >= limit) {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Replay retained events over an opaque cursor range. The `/events` stream is
/// the same SSE endpoint used by `follow`, but bounded replay still requires a
/// streaming consumer: this helper reads frames until the `until` cursor is
/// observed, `limit` frames arrive, or the server ends retained replay.
async fn events_range(
    client: &Client,
    base_url: &str,
    after: Option<u64>,
    until: Option<u64>,
    limit: usize,
) -> anyhow::Result<()> {
    let mut request = client.get(url(base_url, "/api/v1/events"));
    if let Some(cursor) = after {
        request = request.query(&[("after", cursor.to_string())]);
    }
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await?;
        bail!("events returned {status}: {body}");
    }

    let mut received = 0usize;
    let mut buffered = String::new();
    let mut bytes = response.bytes_stream();
    while let Some(chunk) = bytes.next().await {
        buffered.push_str(&String::from_utf8_lossy(&chunk?));
        if buffered.contains("\r\n") {
            buffered = buffered.replace("\r\n", "\n");
        }
        while let Some(end) = buffered.find("\n\n") {
            let frame = buffered[..end].to_owned();
            buffered.drain(..end + 2);
            let mut data: Vec<&str> = Vec::new();
            let mut frame_id: Option<&str> = None;
            for line in frame.lines() {
                if let Some(rest) = line.strip_prefix("id: ") {
                    frame_id = Some(rest);
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    data.push(rest);
                }
            }
            if !data.is_empty() {
                println!("{}", data.join("\n"));
                let stop = until
                    .zip(frame_id)
                    .is_some_and(|(until_id, fid)| fid.parse::<u64>().ok() == Some(until_id));
                received += 1;
                if stop || received >= limit {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

async fn sessions_list(client: &Client, base_url: &str, project_id: &str) -> anyhow::Result<()> {
    let response = client
        .get(url(base_url, &format!("/api/v1/projects/{project_id}/sessions")))
        .send()
        .await?;
    print_response(response).await
}

async fn sessions_create(
    client: &Client,
    base_url: &str,
    project_id: &str,
    title: Option<String>,
) -> anyhow::Result<()> {
    let body = serde_json::json!({ "title": title });
    let response = client
        .post(url(base_url, &format!("/api/v1/projects/{project_id}/sessions")))
        .json(&body)
        .send()
        .await?;
    print_response(response).await
}

async fn sessions_get(client: &Client, base_url: &str, id: &str) -> anyhow::Result<()> {
    let response = client
        .get(url(base_url, &format!("/api/v1/sessions/{id}")))
        .send()
        .await?;
    print_response(response).await
}

async fn sessions_delete(client: &Client, base_url: &str, id: &str) -> anyhow::Result<()> {
    let response = client
        .delete(url(base_url, &format!("/api/v1/sessions/{id}")))
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    if !text.is_empty() {
        println!("{text}");
    }
    if !status.is_success() {
        bail!("delete returned {status}");
    }
    Ok(())
}

async fn sessions_post_message(
    client: &Client,
    base_url: &str,
    id: &str,
    content: &str,
    expected_session_version: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "content": content,
        "expected_session_version": expected_session_version,
    });
    let response = client
        .post(url(base_url, &format!("/api/v1/sessions/{id}/messages")))
        .json(&body)
        .send()
        .await?;
    print_response(response).await
}

async fn sessions_timeline(
    client: &Client,
    base_url: &str,
    id: &str,
    before: Option<String>,
    after: Option<String>,
    limit: i64,
) -> anyhow::Result<()> {
    let mut request =
        client.get(url(base_url, &format!("/api/v1/sessions/{id}/timeline")));
    if let Some(cursor) = &before {
        request = request.query(&[("before", cursor.as_str())]);
    }
    if let Some(cursor) = &after {
        request = request.query(&[("after", cursor.as_str())]);
    }
    request = request.query(&[("limit", limit.to_string())]);
    let response = request.send().await?;
    print_response(response).await
}

async fn sessions_get_turn(
    client: &Client,
    base_url: &str,
    id: &str,
    turn_id: &str,
) -> anyhow::Result<()> {
    let response = client
        .get(url(
            base_url,
            &format!("/api/v1/sessions/{id}/turns/{turn_id}"),
        ))
        .send()
        .await?;
    print_response(response).await
}

async fn sessions_diff(client: &Client, base_url: &str, id: &str) -> anyhow::Result<()> {
    let response = client
        .get(url(base_url, &format!("/api/v1/sessions/{id}/diff")))
        .send()
        .await?;
    print_response(response).await
}

fn url(base_url: &str, path: &str) -> String {
    format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        if path.starts_with('/') {
            path.into()
        } else {
            format!("/{path}")
        }
    )
}

fn read_body(path: &PathBuf) -> anyhow::Result<String> {
    if path.as_os_str() == "-" {
        let mut body = String::new();
        std::io::stdin().read_to_string(&mut body)?;
        Ok(body)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
    }
}
