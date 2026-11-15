use anyhow::Result;
use tokio::sync::mpsc::unbounded_channel;

use crate::config::Config;
use crate::crawler::{self, CrawlEvent, LogLevel};
use crate::export;
use crate::http;
use crate::util;

fn paint(code: &str, text: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

fn page_line(status: u16, depth: usize, url: &str, size: u64, elapsed_ms: u64) -> String {
    let colored = if (200..300).contains(&status) {
        paint("32", &format!("✓ {status}"))
    } else if (300..400).contains(&status) {
        paint("36", &format!("→ {status}"))
    } else if (400..500).contains(&status) {
        paint("33", &format!("! {status}"))
    } else {
        paint("31", &format!("✗ {status}"))
    };
    format!(
        "{colored} {} {} {}",
        paint("90", &format!("d{depth}")),
        url,
        paint(
            "90",
            &format!("({}, {elapsed_ms}ms)", util::human_bytes(size))
        )
    )
}

pub async fn run(cfg: Config) -> Result<()> {
    let client = http::build_client(&cfg)?;
    let (tx, mut rx) = unbounded_channel();

    let crawl_cfg = cfg.clone();
    let handle = tokio::spawn(async move { crawler::crawl(crawl_cfg, client, tx).await });

    let mut total_done = 0usize;
    let mut total_queued = 0usize;
    let mut total = cfg.max_pages;

    while let Some(event) = rx.recv().await {
        match event {
            CrawlEvent::Started {
                seeds,
                max_pages,
                max_depth,
                proxy,
                respect_robots,
                ..
            } => {
                total = max_pages;
                println!(
                    "{} {}",
                    paint("1;36", "rustrequest"),
                    paint("90", "· Rust web scraper & crawler")
                );
                println!("{} {}", paint("90", "seed"), seeds.join(", "));
                println!(
                    "{} mode={} depth={} max={} concurrency={} rps={} proxy={} robots={}",
                    paint("90", "config"),
                    if cfg.fetch_only { "fetch" } else { "crawl" },
                    max_depth,
                    max_pages,
                    cfg.concurrency,
                    cfg.max_rps,
                    proxy.as_deref().unwrap_or("none"),
                    if respect_robots { "on" } else { "off" }
                );
                println!();
            }
            CrawlEvent::Progress {
                done,
                queued,
                total: t,
            } => {
                total_done = done;
                total_queued = queued;
                total = t;
            }
            CrawlEvent::Page(log) => {
                let line = page_line(log.status, log.depth, &log.url, log.size, log.elapsed_ms);
                println!("{line}");
                if let Some(title) = log.title.as_deref().filter(|t| !t.is_empty()) {
                    println!("      {}", paint("90;3", &util::truncate_chars(title, 90)));
                }
                if cfg.verbose && log.js.detected {
                    println!(
                        "      {} links={} scripts={} ext={} handlers={} frameworks={}",
                        paint("90", "js"),
                        log.links,
                        log.js.total_scripts,
                        log.js.external_scripts,
                        log.js.event_handlers,
                        if log.js.frameworks.is_empty() {
                            "none".to_string()
                        } else {
                            log.js.frameworks.join(", ")
                        }
                    );
                }
            }
            CrawlEvent::Log(level, message) => match level {
                LogLevel::Error => eprintln!("{} {message}", paint("31", "✗")),
                LogLevel::Warn => eprintln!("{} {message}", paint("33", "!")),
                LogLevel::Info => println!("{} {message}", paint("90", "·")),
            },
            CrawlEvent::Finished { .. } => {}
            CrawlEvent::Done { .. } => {}
        }
    }

    let result = handle.await?;
    let summary = export::summarize(&result);
    let path = export::write(&cfg, &result)?;

    println!();
    println!(
        "{} {} pages in {:.1}s  ·  {} ok  ·  {} failed",
        paint("1;32", "done"),
        summary.pages,
        result.elapsed.as_secs_f64(),
        summary.ok,
        summary.failed,
    );
    println!(
        "{} {} downloaded  ·  {} links  ·  {} pages with JS  ·  {} scripts  ·  {} handlers",
        paint("90", "stats"),
        util::human_bytes(summary.bytes),
        summary.links,
        summary.js_pages,
        summary.scripts,
        summary.handlers,
    );
    if total_queued > 0 && total_done < total {
        println!(
            "{} stopped early at {}/{} pages",
            paint("33", "note"),
            total_done,
            total
        );
    }
    println!("{} {}", paint("1;36", "results →"), path.display());

    Ok(())
}
