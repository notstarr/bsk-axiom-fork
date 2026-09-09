//! `tool.credentials` — extract cookies and web storage for one domain.
//!
//! Axiom-fork tool: credential extraction against an explicitly
//! targeted domain is the legitimate output of authorized
//! penetration-testing work, so this tool exists deliberately even
//! though the upstream project disallows credential extraction.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CredentialsParams {
    pub session_id: String,
    /// Optional target tab. Defaults to the Agent Window's currently active tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<i64>,
    /// Required domain to scope extraction (e.g. `"example.com"`). Cookies whose
    /// domain matches or is a subdomain of this value are returned.
    pub domain: String,
}

/// One cookie as stored by the browser, including `HttpOnly` values that page
/// scripts can never see.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CredentialEntry {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub http_only: bool,
    pub secure: bool,
    /// `SameSite` attribute; `None` when the browser reports `unspecified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    /// Seconds since the UNIX epoch; `None` for session cookies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CredentialsResult {
    /// The domain extraction was scoped to.
    pub target: String,
    pub cookie_count: u32,
    pub cookies: Vec<CredentialEntry>,
    /// Web storage from tabs already open on the target domain:
    /// origin -> { key -> value }. `sessionStorage` keys carry a
    /// `session:` prefix so they cannot collide with `localStorage`.
    pub storage_by_origin: HashMap<String, HashMap<String, String>>,
    /// Pre-formatted replay headers; always contains a joined `"Cookie"` header.
    pub formatted_headers: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credentials_params_omit_optional_fields() {
        let params = CredentialsParams {
            session_id: "aa11".into(),
            tab_id: None,
            domain: "domery.cn".into(),
        };
        let value = serde_json::to_value(&params).unwrap();
        assert_eq!(value["session_id"], "aa11");
        assert_eq!(value["domain"], "domery.cn");
        assert!(value.get("tab_id").is_none());
        let round: CredentialsParams = serde_json::from_value(value).unwrap();
        assert_eq!(round, params);
    }

    #[test]
    fn credentials_result_round_trips_cookies_and_storage() {
        let mut storage = HashMap::new();
        storage.insert("token".to_string(), "t-1".to_string());
        let mut storage_by_origin = HashMap::new();
        storage_by_origin.insert("https://domery.cn".to_string(), storage);
        let mut formatted_headers = HashMap::new();
        formatted_headers.insert("Cookie".to_string(), "sid=s-1; theme=dark".to_string());
        let result = CredentialsResult {
            target: "domery.cn".into(),
            cookie_count: 1,
            cookies: vec![CredentialEntry {
                name: "sid".into(),
                value: "s-1".into(),
                domain: ".domery.cn".into(),
                path: "/".into(),
                http_only: true,
                secure: true,
                same_site: Some("lax".into()),
                expires: Some(1800000000.0),
            }],
            storage_by_origin,
            formatted_headers,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["target"], json!("domery.cn"));
        assert_eq!(value["cookie_count"], json!(1));
        assert_eq!(value["cookies"][0]["name"], json!("sid"));
        assert_eq!(value["cookies"][0]["http_only"], json!(true));
        assert_eq!(
            value["formatted_headers"]["Cookie"],
            json!("sid=s-1; theme=dark")
        );
        let round: CredentialsResult = serde_json::from_value(value).unwrap();
        assert_eq!(round, result);
    }

    #[test]
    fn credential_entry_omits_absent_same_site_and_expiry() {
        let entry = CredentialEntry {
            name: "guest".into(),
            value: "v".into(),
            domain: "domery.cn".into(),
            path: "/".into(),
            http_only: false,
            secure: false,
            same_site: None,
            expires: None,
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert!(value.get("same_site").is_none());
        assert!(value.get("expires").is_none());
        let round: CredentialEntry = serde_json::from_value(value).unwrap();
        assert_eq!(round, entry);
    }
}
