//! `bsk network` — read buffered page network responses / failures.

use std::path::PathBuf;

use anyhow::Context;
use bsk_protocol::Method;
use bsk_protocol::tools::{NetworkEntry, NetworkParams, NetworkResult};
use clap::Args;

use crate::cli::TOOL_IPC_TIMEOUT;
use crate::cli::ensure_daemon::ensure_daemon;
use crate::cli::error::{CliError, Format};

#[derive(Debug, Clone, Args)]
pub struct NetworkArgs {
    /// Session id (must be active).
    #[arg(long)]
    pub session: String,

    /// Target tab. Defaults to the Agent Window's active tab.
    #[arg(long = "tab-id")]
    pub tab_id: Option<i64>,

    /// Return entries with sequence greater than this cursor.
    #[arg(long)]
    pub since: Option<u64>,

    /// Maximum number of entries to return. Defaults to 50; extension caps at 200.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub limit: Option<u32>,

    /// Maximum characters per URL / error text. Defaults to 1000; extension caps at 4096.
    #[arg(long = "max-text-chars", value_parser = clap::value_parser!(u32).range(1..))]
    pub max_text_chars: Option<u32>,
}

pub fn dispatch(args: NetworkArgs, format: Format) -> Result<(), CliError> {
    let info = ensure_daemon().context("ensure daemon is running")?;
    run(info.sock_path, args, format)
}

fn run(sock: PathBuf, args: NetworkArgs, format: Format) -> Result<(), CliError> {
    let params = NetworkParams {
        session_id: args.session,
        tab_id: args.tab_id,
        since: args.since,
        limit: args.limit,
        max_text_chars: args.max_text_chars,
    };
    let reply: NetworkResult = call(sock, params)?;
    render(&reply, format)
}

fn call(sock: PathBuf, params: NetworkParams) -> Result<NetworkResult, CliError> {
    crate::cli::business_rpc::call::<NetworkParams, NetworkResult>(
        sock,
        "network",
        Method::ToolNetwork,
        Some(params),
        TOOL_IPC_TIMEOUT,
    )
}

fn render(reply: &NetworkResult, format: Format) -> Result<(), CliError> {
    match format {
        Format::Json => {
            let json = serde_json::to_string_pretty(reply)
                .map_err(|e| CliError::Local(anyhow::anyhow!(e)))?;
            println!("{json}");
        }
        Format::Human => {
            if reply.entries.is_empty() {
                println!("(no network activity captured)");
            } else {
                for entry in &reply.entries {
                    println!("{}", render_entry(entry));
                    if let Some(body) = &entry.response_body {
                        println!("{}", render_response_body(body));
                    }
                }
            }
            if reply.truncated {
                eprintln!(
                    "warning: network output truncated (next_since={}). Use --since / --limit / --max-text-chars to request a different slice.",
                    reply.next_since
                );
            }
        }
    }
    Ok(())
}

fn render_entry(entry: &NetworkEntry) -> String {
    let method = entry.method.as_deref().unwrap_or("?");
    let url = entry.url.as_deref().unwrap_or("(unknown)");
    match entry.kind {
        bsk_protocol::tools::NetworkEntryKind::Failure => {
            let err = entry.error_text.as_deref().unwrap_or("failed");
            format!("#{} FAILED {method} {url} - {err}", entry.sequence)
        }
        bsk_protocol::tools::NetworkEntryKind::Response => {
            let status = entry
                .status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "?".to_string());
            format!("#{} {status} {method} {url}", entry.sequence)
        }
    }
}

/// Human output caps the captured body preview; JSON output keeps the full
/// extension-capped payload (up to 256 KB per entry).
const RENDER_BODY_MAX_CHARS: usize = 500;

fn render_response_body(body: &str) -> String {
    let char_count = body.chars().count();
    if char_count <= RENDER_BODY_MAX_CHARS {
        return format!("  body: {body}");
    }
    let snippet: String = body.chars().take(RENDER_BODY_MAX_CHARS).collect();
    format!("  body (first {RENDER_BODY_MAX_CHARS} of {char_count} chars): {snippet}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bsk_protocol::tools::NetworkEntryKind;

    #[test]
    fn human_failure_output_uses_ascii_and_displays_unknown_url() {
        let entry = NetworkEntry {
            sequence: 7,
            kind: NetworkEntryKind::Failure,
            method: Some("GET".into()),
            url: None,
            status: None,
            status_text: None,
            mime_type: None,
            resource_type: Some("Fetch".into()),
            error_text: Some("net::ERR_FAILED".into()),
            response_body: None,
            timestamp: None,
            truncated: false,
        };

        assert_eq!(
            render_entry(&entry),
            "#7 FAILED GET (unknown) - net::ERR_FAILED"
        );
    }

    #[test]
    fn human_response_body_line_keeps_short_bodies_whole() {
        assert_eq!(render_response_body("{\"a\":1}"), "  body: {\"a\":1}");
    }

    #[test]
    fn human_response_body_line_truncates_long_bodies() {
        let body = "x".repeat(600);
        let line = render_response_body(&body);
        assert!(line.starts_with("  body (first 500 of 600 chars): "));
        assert!(line.ends_with(&"x".repeat(500)));
    }
}
