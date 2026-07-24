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
    Follow {
        #[arg(long)]
        after: Option<u64>,
        #[arg(long)]
        count: Option<usize>,
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
    Get {
        id: String,
    },
    /// Git status projection for a project.
    GitStatus {
        id: String,
    },
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
        Command::Projects { command } => match command {
            ProjectsCommand::List => projects_list(&client, &cli.base_url).await,
            ProjectsCommand::Create {
                name,
                url,
                branch,
                idempotency_key,
            } => {
                projects_create(&client, &cli.base_url, name, url, branch, idempotency_key).await
            }
            ProjectsCommand::Get { id } => projects_get(&client, &cli.base_url, &id).await,
            ProjectsCommand::GitStatus { id } => {
                projects_git_status(&client, &cli.base_url, &id).await
            }
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
