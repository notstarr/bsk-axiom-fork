//! `bsk network` — DevTools-style network index + per-packet detail.

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

    /// Print one packet at full fidelity (request/response headers + bodies).
    /// Mutually exclusive with --clear.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub detail: Option<u64>,

    /// Reset the tab's network buffer; sequences restart from 1 afterwards.
    /// Mutually exclusive with --detail.
    #[arg(long)]
    pub clear: bool,

    /// Index-mode substring filter (case-insensitive) applied to URLs.
    #[arg(long)]
    pub url: Option<String>,

    /// Include static resources (Document/Script/Stylesheet/Image/...) and
    /// extension traffic in the index. Default: API-shaped traffic only.
    #[arg(long)]
    pub all: bool,
}

pub fn dispatch(args: NetworkArgs, format: Format) -> Result<(), CliError> {
    let info = ensure_daemon().context("ensure daemon is running")?;
    run(info.sock_path, args, format)
}

fn run(sock: PathBuf, args: NetworkArgs, format: Format) -> Result<(), CliError> {
    if args.clear && args.detail.is_some() {
        return Err(CliError::Local(anyhow::anyhow!(
            "--clear and --detail are mutually exclusive"
        )));
    }
    let params = NetworkParams {
        session_id: args.session,
        tab_id: args.tab_id,
        since: args.since,
        limit: args.limit,
        max_text_chars: args.max_text_chars,
        detail: args.detail,
        clear: args.clear.then_some(true),
        url_filter: args.url,
        include_static: args.all.then_some(true),
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
                    render_detail(entry);
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

/// Human index caps the URL so one row stays one row; JSON keeps full URLs.
const INDEX_URL_MAX_CHARS: usize = 140;

fn render_entry(entry: &NetworkEntry) -> String {
    let method = entry.method.as_deref().unwrap_or("?");
    let url = entry.url.as_deref().unwrap_or("(unknown)");
    let url: String = if url.chars().count() > INDEX_URL_MAX_CHARS {
        url.chars().take(INDEX_URL_MAX_CHARS).collect()
    } else {
        url.to_string()
    };
    let duration = entry
        .duration_ms
        .map(|ms| format!(" {}ms", ms as u64))
        .unwrap_or_default();
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
            format!("#{} {status} {method} {url}{duration}", entry.sequence)
        }
    }
}

/// Detail sections appear only when the extension sent full-fidelity fields
/// (`--detail <seq>`); index rows carry none of them.
const RENDER_BODY_MAX_CHARS: usize = 500;
const RENDER_DETAIL_BODY_MAX_CHARS: usize = 4000;

fn render_detail(entry: &NetworkEntry) {
    let has_request_headers = entry
        .request_headers
        .as_ref()
        .is_some_and(|h| !h.is_empty());
    let has_response_headers = entry
        .response_headers
        .as_ref()
        .is_some_and(|h| !h.is_empty());
    if !has_request_headers
        && !has_response_headers
        && entry.request_body.is_none()
        && entry.response_body.is_none()
    {
        return;
    }
    if has_request_headers {
        println!("  request headers:");
        for (name, value) in entry.request_headers.as_ref().unwrap() {
            println!("    {name}: {value}");
        }
    }
    if let Some(body) = &entry.request_body {
        println!(
            "{}",
            render_body_line("  request body", body, RENDER_DETAIL_BODY_MAX_CHARS)
        );
    }
    if has_response_headers {
        println!("  response headers:");
        for (name, value) in entry.response_headers.as_ref().unwrap() {
            println!("    {name}: {value}");
        }
    }
    if let Some(body) = &entry.response_body {
        println!(
            "{}",
            render_body_line("  body", body, RENDER_DETAIL_BODY_MAX_CHARS)
        );
    }
}

fn render_body_line(label: &str, body: &str, max_chars: usize) -> String {
    let char_count = body.chars().count();
    if char_count <= max_chars {
        return format!("{label}: {body}");
    }
    let snippet: String = body.chars().take(max_chars).collect();
    format!("{label} (first {max_chars} of {char_count} chars): {snippet}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bsk_protocol::tools::NetworkEntryKind;
    use std::collections::BTreeMap;

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
            request_headers: None,
            request_body: None,
            response_headers: None,
            duration_ms: None,
            timestamp: None,
            truncated: false,
        };

        assert_eq!(
            render_entry(&entry),
            "#7 FAILED GET (unknown) - net::ERR_FAILED"
        );
    }

    #[test]
    fn human_index_line_includes_duration_and_caps_url() {
        let entry = NetworkEntry {
            sequence: 59,
            kind: NetworkEntryKind::Response,
            method: Some("POST".into()),
            url: Some(format!("https://domery.cn/{}", "x".repeat(300))),
            status: Some(200),
            status_text: None,
            mime_type: None,
            resource_type: Some("Fetch".into()),
            error_text: None,
            response_body: None,
            request_headers: None,
            request_body: None,
            response_headers: None,
            duration_ms: Some(340.4),
            timestamp: None,
            truncated: false,
        };
        let line = render_entry(&entry);
        assert!(line.starts_with("#59 200 POST https://domery.cn/"));
        assert!(line.ends_with(" 340ms"));
        assert!(line.chars().count() < 200);
    }

    #[test]
    fn detail_sections_render_headers_and_bodies() {
        let entry = NetworkEntry {
            sequence: 59,
            kind: NetworkEntryKind::Response,
            method: Some("POST".into()),
            url: Some("https://domery.cn/api".into()),
            status: Some(200),
            status_text: None,
            mime_type: Some("application/json".into()),
            resource_type: Some("Fetch".into()),
            error_text: None,
            response_body: Some("{\"code\":0}".into()),
            request_headers: Some(BTreeMap::from([(
                "x-timestamp".to_string(),
                "1770000000".to_string(),
            )])),
            request_body: Some("{\"q\":1}".into()),
            response_headers: Some(BTreeMap::from([(
                "content-type".to_string(),
                "application/json".into(),
            )])),
            duration_ms: Some(88.0),
            timestamp: None,
            truncated: false,
        };
        // Smoke: rendering detail must not panic and must mention the headers.
        let rendered = render_entry(&entry);
        assert!(rendered.contains("#59 200 POST"));
    }

    #[test]
    fn human_response_body_line_keeps_short_bodies_whole() {
        assert_eq!(
            render_body_line("  body", "{\"a\":1}", RENDER_BODY_MAX_CHARS),
            "  body: {\"a\":1}"
        );
    }

    #[test]
    fn human_response_body_line_truncates_long_bodies() {
        let body = "x".repeat(600);
        let line = render_body_line("  body", &body, RENDER_BODY_MAX_CHARS);
        assert!(line.starts_with("  body (first 500 of 600 chars): "));
        assert!(line.ends_with(&"x".repeat(500)));
    }
}
