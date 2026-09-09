//! `bsk credentials` — extract cookies and web storage for one domain.
//!
//! Axiom-fork tool: credential extraction against an explicitly targeted
//! domain is the legitimate output of authorized penetration-testing work.
//! Restrict usage to test targets the operator is authorized to assess.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use bsk_protocol::Method;
use bsk_protocol::tools::{CredentialEntry, CredentialsParams, CredentialsResult};
use clap::Args;

use crate::cli::TOOL_IPC_TIMEOUT;
use crate::cli::ensure_daemon::ensure_daemon;
use crate::cli::error::{CliError, Format};

#[derive(Debug, Clone, Args)]
pub struct CredentialsArgs {
    /// Session id (must be active).
    #[arg(long)]
    pub session: String,

    /// Target tab. Defaults to the Agent Window's active tab.
    #[arg(long = "tab-id")]
    pub tab_id: Option<i64>,

    /// Required domain to scope extraction (e.g. "example.com"). Cookies whose
    /// domain matches or is a subdomain of this value are returned.
    #[arg(long)]
    pub domain: String,
}

pub fn dispatch(args: CredentialsArgs, format: Format) -> Result<(), CliError> {
    let info = ensure_daemon().context("ensure daemon is running")?;
    run(info.sock_path, args, format)
}

fn run(sock: PathBuf, args: CredentialsArgs, format: Format) -> Result<(), CliError> {
    let params = CredentialsParams {
        session_id: args.session,
        tab_id: args.tab_id,
        domain: args.domain,
    };
    let reply: CredentialsResult = call(sock, params)?;
    render(&reply, format)
}

fn call(sock: PathBuf, params: CredentialsParams) -> Result<CredentialsResult, CliError> {
    crate::cli::business_rpc::call::<CredentialsParams, CredentialsResult>(
        sock,
        "credentials",
        Method::ToolCredentials,
        Some(params),
        TOOL_IPC_TIMEOUT,
    )
}

fn render(reply: &CredentialsResult, format: Format) -> Result<(), CliError> {
    match format {
        Format::Json => {
            let json = serde_json::to_string_pretty(reply)
                .map_err(|e| CliError::Local(anyhow::anyhow!(e)))?;
            println!("{json}");
        }
        Format::Human => {
            if let Some(cookie_header) = reply.formatted_headers.get("Cookie") {
                println!("Cookie: {cookie_header}");
            }
            if reply.cookies.is_empty() {
                println!("(no cookies found for {})", reply.target);
            } else {
                for cookie in &reply.cookies {
                    println!("{}", render_cookie(cookie));
                }
            }
            for (origin, storage) in sorted_storage(reply) {
                let mut keys: Vec<&String> = storage.keys().collect();
                keys.sort();
                let joined = keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ");
                println!("storage {origin}: {joined}");
            }
        }
    }
    Ok(())
}

fn render_cookie(cookie: &CredentialEntry) -> String {
    let mut line = format!("{} = {} ({})", cookie.name, cookie.value, cookie.domain);
    if cookie.http_only {
        line.push_str(" [HttpOnly]");
    }
    line
}

/// Deterministic (origin-ascending) view of the web-storage map so human
/// output never depends on HashMap iteration order.
fn sorted_storage(reply: &CredentialsResult) -> Vec<(&String, &HashMap<String, String>)> {
    let mut entries: Vec<(&String, &HashMap<String, String>)> =
        reply.storage_by_origin.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_line_appends_http_only_marker() {
        let cookie = CredentialEntry {
            name: "sid".into(),
            value: "abc123".into(),
            domain: ".example.com".into(),
            path: "/".into(),
            http_only: true,
            secure: true,
            same_site: Some("Lax".into()),
            expires: Some(1800000000.0),
        };
        assert_eq!(render_cookie(&cookie), "sid = abc123 (.example.com) [HttpOnly]");
    }

    #[test]
    fn cookie_line_omits_marker_for_script_readable_cookies() {
        let cookie = CredentialEntry {
            name: "prefs".into(),
            value: "dark".into(),
            domain: "example.com".into(),
            path: "/".into(),
            http_only: false,
            secure: false,
            same_site: None,
            expires: None,
        };
        assert_eq!(render_cookie(&cookie), "prefs = dark (example.com)");
    }

    #[test]
    fn human_storage_lines_are_origin_sorted() {
        let reply = CredentialsResult {
            target: "example.com".into(),
            cookie_count: 0,
            cookies: vec![],
            storage_by_origin: HashMap::from([
                (
                    "https://b.example.com".to_string(),
                    HashMap::from([("theme".to_string(), "dark".to_string())]),
                ),
                (
                    "https://a.example.com".to_string(),
                    HashMap::from([
                        ("cart".to_string(), "{}".to_string()),
                        ("session: otp".to_string(), "42".to_string()),
                    ]),
                ),
            ]),
            formatted_headers: HashMap::new(),
        };
        let lines: Vec<String> = sorted_storage(&reply)
            .into_iter()
            .map(|(origin, storage)| {
                let mut keys: Vec<&String> = storage.keys().collect();
                keys.sort();
                format!(
                    "storage {origin}: {}",
                    keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")
                )
            })
            .collect();
        assert_eq!(
            lines,
            vec![
                "storage https://a.example.com: cart, session: otp",
                "storage https://b.example.com: theme",
            ]
        );
    }
}
