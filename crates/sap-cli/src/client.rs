use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use http::{Method, Request, Uri};
use http_body_util::{BodyExt, Full, Limited};
use hyper_util::{
    client::legacy::{Client as HyperClient, connect::HttpConnector},
    rt::TokioExecutor,
};
use serde::{Serialize, de::DeserializeOwned};
use std::{path::Path, time::Duration};

pub struct Client {
    base: String,
    key: Option<String>,
    http: HyperClient<HttpConnector, Full<Bytes>>,
}
impl Client {
    pub fn new(base: &str, key: Option<String>) -> Result<Self> {
        // ponytail: private-network HTTP endpoints only; add verified TLS before HTTPS.
        let uri: Uri = base.parse().context("invalid router URL")?;
        ensure!(
            uri.scheme_str() == Some("http")
                && uri.host().is_some()
                && !uri.authority().unwrap().as_str().contains('@')
                && uri.path_and_query().is_none_or(|p| p.as_str() == "/"),
            "router URL must be an HTTP authority without credentials or a path"
        );
        Ok(Self {
            base: base.trim_end_matches('/').into(),
            key,
            http: HyperClient::builder(TokioExecutor::new()).build_http(),
        })
    }
    pub async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&impl Serialize>,
    ) -> Result<T> {
        let bytes = body.map(serde_json::to_vec).transpose()?.unwrap_or_default();
        let mut request = Request::builder().method(method).uri(format!("{}{path}", self.base));
        if let Some(key) = &self.key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let request = request
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(bytes)))?;
        tokio::time::timeout(Duration::from_secs(5), async {
            let response = self.http.request(request).await.context("connect to router/server")?;
            let status = response.status();
            let bytes = Limited::new(response.into_body(), 8 * 1024 * 1024)
                .collect()
                .await?
                .to_bytes();
            // Never echo arbitrary server error bodies, which may contain secrets.
            if !status.is_success() {
                bail!("HTTP {status}");
            }
            Ok(serde_json::from_slice(&bytes)?)
        })
        .await
        .context("HTTP check timed out after 5 seconds")?
    }
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.request(Method::GET, path, None::<&()>).await
    }
}

pub fn management_key(explicit: Option<String>, file: &Path) -> Result<String> {
    let key = match explicit.or_else(|| std::env::var("SAP_MANAGEMENT_KEY").ok()) {
        Some(key) => key,
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    std::fs::metadata(file)?.permissions().mode() & 0o777 == 0o600,
                    "management-key file must have mode 0600"
                );
            }
            std::fs::read_to_string(file)
                .context("provide --key, SAP_MANAGEMENT_KEY, or a mode-0600 management-key file")?
                .trim()
                .into()
        }
    };
    ensure!(!key.is_empty(), "management key is empty");
    Ok(key)
}

/// Percent-encodes one complete path segment, including slashes in thread keys.
pub fn segment(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
