use std::{io::Read, path::PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use futures_util::StreamExt;
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
}

#[derive(Debug, Args)]
struct RequestArgs {
    method: String,
    path: String,
    #[arg(long)]
    json: Option<PathBuf>,
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
