use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;
use url::Url;

use crate::cli::{Cli, OutputFormat};

#[derive(Debug, Clone)]
pub struct Config {
    pub seeds: Vec<Url>,
    pub max_depth: usize,
    pub max_pages: usize,
    pub concurrency: usize,
    pub same_domain: bool,
    pub include_subdomains: bool,
    pub proxy: Option<String>,
    pub timeout: Duration,
    pub delay: Duration,
    pub retries: u32,
    pub user_agent: String,
    pub headers: Vec<(String, String)>,
    pub respect_robots: bool,
    pub remove_js: bool,
    pub extract_text: bool,
    pub insecure: bool,
    pub fetch_only: bool,
    pub format: OutputFormat,
    pub output: Option<PathBuf>,
    pub verbose: bool,
    pub max_links: Option<usize>,
    pub exclude: Vec<String>,
    pub tui: bool,
    pub gui: bool,
}

pub fn has_display() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
}

pub fn normalize_url(raw: &str) -> anyhow::Result<Url> {
    let trimmed = raw.trim();
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let mut url =
        Url::parse(&with_scheme).map_err(|e| anyhow::anyhow!("invalid URL '{raw}': {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("unsupported URL scheme '{}' in '{raw}'", url.scheme());
    }
    url.set_fragment(None);
    Ok(url)
}

impl Config {
    pub fn from_cli(cli: Cli) -> anyhow::Result<Self> {
        let seeds: Vec<Url> = cli
            .urls
            .iter()
            .map(|u| normalize_url(u))
            .collect::<anyhow::Result<_>>()?;
        if seeds.is_empty() {
            anyhow::bail!("at least one URL is required");
        }

        let mut headers = Vec::new();
        for raw in &cli.headers {
            let Some((name, value)) = raw.split_once(':') else {
                anyhow::bail!("invalid header '{raw}', expected \"Name: value\"");
            };
            let name = name.trim();
            if name.is_empty() {
                anyhow::bail!("invalid header '{raw}', empty name");
            }
            headers.push((name.to_string(), value.trim().to_string()));
        }

        let fetch_only = cli.fetch;
        let max_depth = if fetch_only { 0 } else { cli.depth };
        let max_pages = if fetch_only {
            seeds.len()
        } else {
            cli.max_pages.max(1)
        };

        let (gui, tui) = if cli.gui {
            (true, false)
        } else if cli.tui {
            (false, true)
        } else if cli.no_tui {
            (false, false)
        } else if has_display() {
            (true, false)
        } else if std::io::stdout().is_terminal() {
            (false, true)
        } else {
            (false, false)
        };

        Ok(Config {
            seeds,
            max_depth,
            max_pages,
            concurrency: cli.concurrency.max(1),
            same_domain: !cli.any_domain,
            include_subdomains: cli.include_subdomains,
            proxy: cli.proxy,
            timeout: Duration::from_secs(cli.timeout.max(1)),
            delay: Duration::from_millis(cli.delay),
            retries: cli.retries,
            user_agent: cli.user_agent,
            headers,
            respect_robots: !cli.ignore_robots,
            remove_js: !cli.keep_js,
            extract_text: !cli.no_text,
            insecure: cli.insecure,
            fetch_only,
            format: cli.format,
            output: cli.output,
            verbose: cli.verbose,
            max_links: cli.max_links,
            exclude: cli.exclude,
            tui,
            gui,
        })
    }

    pub fn seed_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .seeds
            .iter()
            .filter_map(|u| u.host_str())
            .map(|h| h.trim_start_matches("www.").to_ascii_lowercase())
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }
}
