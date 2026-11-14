use std::collections::{HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::{StreamExt, stream};
use reqwest::Client;
use tokio::sync::mpsc::UnboundedSender;
use url::Url;

use crate::config::Config;
use crate::extract::{self, PageData};
use crate::http;
use crate::js::JsReport;
use crate::robots::RobotsCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct PageLog {
    pub url: String,
    pub status: u16,
    pub depth: usize,
    pub title: Option<String>,
    pub links: usize,
    pub size: u64,
    pub elapsed_ms: u64,
    pub js: JsReport,
}

#[derive(Debug, Clone)]
pub enum CrawlEvent {
    Started {
        seeds: Vec<String>,
        max_pages: usize,
        max_depth: usize,
        proxy: Option<String>,
        respect_robots: bool,
        user_agent: String,
    },
    Progress {
        done: usize,
        queued: usize,
        total: usize,
    },
    Page(PageLog),
    Log(LogLevel, String),
    Finished {
        pages: usize,
        failed: usize,
        elapsed_secs: f64,
    },
    Done {
        output: Result<String, String>,
    },
}

#[derive(Default)]
pub struct CrawlResult {
    pub pages: Vec<PageData>,
    pub errors: Vec<(String, String)>,
    pub elapsed: Duration,
}

enum Outcome {
    Ok(http::FetchResult),
    Skipped,
    Err(String),
}

fn host_allowed(url: &Url, hosts: &[String], include_subdomains: bool) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches("www.").to_ascii_lowercase();
    hosts
        .iter()
        .any(|seed| host == *seed || (include_subdomains && host.ends_with(&format!(".{seed}"))))
}

fn html_filename(url: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}.html", hasher.finish())
}

fn save_html(dir: &Path, url: &str, html: &str) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let path = dir.join(html_filename(url));
    let _ = std::fs::write(path, html);
}

pub async fn crawl(cfg: Config, client: Client, tx: UnboundedSender<CrawlEvent>) -> CrawlResult {
    crawl_with_cancel(cfg, client, tx, Arc::new(AtomicBool::new(false))).await
}

pub async fn crawl_with_cancel(
    cfg: Config,
    client: Client,
    tx: UnboundedSender<CrawlEvent>,
    cancel: Arc<AtomicBool>,
) -> CrawlResult {
    let started = Instant::now();
    let robots = RobotsCache::new(client.clone(), cfg.user_agent.clone(), cfg.respect_robots);

    let _ = tx.send(CrawlEvent::Started {
        seeds: cfg.seeds.iter().map(|u| u.to_string()).collect(),
        max_pages: cfg.max_pages,
        max_depth: cfg.max_depth,
        proxy: cfg.proxy.clone(),
        respect_robots: cfg.respect_robots,
        user_agent: cfg.user_agent.clone(),
    });

    let action = if cfg.fetch_only { "scrape" } else { "crawl" };
    let _ = tx.send(CrawlEvent::Log(
        LogLevel::Info,
        format!(
            "starting {action} · {} seed(s) · max {} pages · depth {}",
            cfg.seeds.len(),
            cfg.max_pages,
            cfg.max_depth
        ),
    ));

    let seed_hosts = cfg.seed_hosts();
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier: VecDeque<(Url, usize)> = VecDeque::new();

    for seed in &cfg.seeds {
        if visited.insert(seed.as_str().to_string()) {
            frontier.push_back((seed.clone(), 0));
        }
    }

    let mut pages: Vec<PageData> = Vec::new();
    let mut errors: Vec<(String, String)> = Vec::new();
    let mut pages_done = 0usize;
    let mut failed = 0usize;

    while !frontier.is_empty() && pages_done < cfg.max_pages && !cancel.load(Ordering::Relaxed) {
        let remaining = cfg.max_pages - pages_done;
        let mut level: Vec<(Url, usize)> = frontier.drain(..).collect();
        if level.len() > remaining {
            level.truncate(remaining);
        }

        let mut queued = level.len();
        let _ = tx.send(CrawlEvent::Progress {
            done: pages_done,
            queued,
            total: cfg.max_pages,
        });

        let mut next_frontier: Vec<(Url, usize)> = Vec::new();
        let client_ref = client.clone();
        let cfg_ref = &cfg;
        let robots_ref = &robots;

        let fetches = stream::iter(level).map(|(url, depth)| {
            let client = client_ref.clone();
            async move {
                let mut delay = cfg_ref.delay;
                if cfg_ref.respect_robots {
                    if !robots_ref.allowed(&url).await {
                        return (url, depth, Outcome::Skipped);
                    }
                    if let Some(secs) = robots_ref.crawl_delay(&url).await {
                        delay = delay.max(Duration::from_secs_f64(secs));
                    }
                }
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                match http::fetch(&client, &url, cfg_ref).await {
                    Ok(result) => (url, depth, Outcome::Ok(result)),
                    Err(err) => (url, depth, Outcome::Err(err.to_string())),
                }
            }
        });

        let mut results = fetches.buffer_unordered(cfg.concurrency);

        while let Some((url, depth, outcome)) = results.next().await {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            queued = queued.saturating_sub(1);
            match outcome {
                Outcome::Ok(fetch) => {
                    let extracted = extract::analyze(
                        url.as_str(),
                        &fetch,
                        depth,
                        extract::ExtractOptions {
                            remove_js: cfg.remove_js,
                            want_text: cfg.extract_text,
                            store_html: cfg.save_html.is_some(),
                            max_links: cfg.max_links,
                            exclude: &cfg.exclude,
                        },
                    );

                    if let (Some(dir), Some(html)) = (&cfg.save_html, &extracted.stored_html) {
                        save_html(dir, &extracted.page.final_url, html);
                    }

                    pages_done += 1;
                    if fetch.status >= 400 {
                        failed += 1;
                    }

                    let page = extracted.page;
                    if depth < cfg.max_depth {
                        for link in &page.links {
                            let Ok(link_url) = Url::parse(link) else {
                                continue;
                            };
                            if !matches!(link_url.scheme(), "http" | "https") {
                                continue;
                            }
                            if cfg.same_domain
                                && !host_allowed(&link_url, &seed_hosts, cfg.include_subdomains)
                            {
                                continue;
                            }
                            let key = link_url.as_str().to_string();
                            if visited.insert(key) {
                                next_frontier.push((link_url, depth + 1));
                            }
                        }
                    }

                    let _ = tx.send(CrawlEvent::Page(PageLog {
                        url: page.final_url.clone(),
                        status: page.status,
                        depth: page.depth,
                        title: page.title.clone(),
                        links: page.links.len(),
                        size: page.size,
                        elapsed_ms: page.elapsed_ms,
                        js: page.js.clone(),
                    }));
                    pages.push(page);
                }
                Outcome::Skipped => {
                    let _ = tx.send(CrawlEvent::Log(
                        LogLevel::Warn,
                        format!("robots.txt disallows {url}"),
                    ));
                }
                Outcome::Err(err) => {
                    failed += 1;
                    let _ = tx.send(CrawlEvent::Log(
                        LogLevel::Error,
                        format!("failed {url}: {err}"),
                    ));
                    errors.push((url.to_string(), err));
                }
            }

            let _ = tx.send(CrawlEvent::Progress {
                done: pages_done,
                queued: queued + next_frontier.len(),
                total: cfg.max_pages,
            });
        }

        frontier = next_frontier.into();
    }

    let elapsed = started.elapsed();
    let _ = tx.send(CrawlEvent::Finished {
        pages: pages.len(),
        failed,
        elapsed_secs: elapsed.as_secs_f64(),
    });

    CrawlResult {
        pages,
        errors,
        elapsed,
    }
}
