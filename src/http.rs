use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use url::Url;

use crate::config::Config;

pub struct FetchResult {
    pub final_url: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
    pub size: usize,
    pub elapsed: Duration,
}

pub fn build_client(cfg: &Config) -> Result<Client> {
    let mut headers = HeaderMap::new();
    for (name, value) in &cfg.headers {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .with_context(|| format!("invalid header name '{name}'"))?;
        let header_value = HeaderValue::from_str(value)
            .with_context(|| format!("invalid value for header '{name}'"))?;
        headers.insert(header_name, header_value);
    }

    let mut builder = Client::builder()
        .user_agent(&cfg.user_agent)
        .timeout(cfg.timeout)
        .connect_timeout(Duration::from_secs(10))
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::limited(10))
        .default_headers(headers);

    if let Some(proxy) = &cfg.proxy {
        let proxy =
            reqwest::Proxy::all(proxy).with_context(|| format!("invalid proxy URL '{proxy}'"))?;
        builder = builder.proxy(proxy);
    }

    if cfg.insecure {
        builder = builder.danger_accept_invalid_certs(true);
    }

    builder.build().context("failed to build HTTP client")
}

fn backoff(attempt: u32) -> Duration {
    let millis = 250u64.saturating_mul(1u64 << attempt.min(5));
    Duration::from_millis(millis.min(8_000))
}

pub async fn fetch(client: &Client, url: &Url, cfg: &Config) -> Result<FetchResult> {
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..=cfg.retries {
        let started = std::time::Instant::now();
        match client.get(url.clone()).send().await {
            Ok(response) => {
                let status = response.status();
                let final_url = response.url().to_string();
                let content_type = response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_string());

                let body = response.text().await.unwrap_or_default();
                let elapsed = started.elapsed();
                let size = body.len();

                let retryable = status.as_u16() == 429 || status.is_server_error();
                if retryable && attempt < cfg.retries {
                    last_error = Some(anyhow::anyhow!("HTTP {status}"));
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }

                return Ok(FetchResult {
                    final_url,
                    status: status.as_u16(),
                    content_type,
                    body,
                    size,
                    elapsed,
                });
            }
            Err(err) => {
                if attempt < cfg.retries {
                    last_error = Some(err.into());
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                return Err(anyhow::anyhow!("{err}"));
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("request failed")))
}
