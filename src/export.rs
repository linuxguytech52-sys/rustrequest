use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::OutputFormat;
use crate::config::Config;
use crate::crawler::CrawlResult;

pub fn write(cfg: &Config, result: &CrawlResult) -> Result<PathBuf> {
    let path = cfg
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("rustrequest-output.{}", cfg.format.extension())));

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    match cfg.format {
        OutputFormat::Json => write_json(&path, result)?,
        OutputFormat::Jsonl => write_jsonl(&path, result)?,
        OutputFormat::Csv => write_csv(&path, result)?,
        OutputFormat::Sitemap => write_sitemap(&path, result)?,
        OutputFormat::Text => write_text(&path, result)?,
    }

    Ok(path)
}

fn write_json(path: &Path, result: &CrawlResult) -> Result<()> {
    let json = serde_json::to_string_pretty(&result.pages)?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

fn write_jsonl(path: &Path, result: &CrawlResult) -> Result<()> {
    let mut out = String::new();
    for page in &result.pages {
        out.push_str(&serde_json::to_string(page)?);
        out.push('\n');
    }
    fs::write(path, out).with_context(|| format!("failed to write {}", path.display()))
}

#[derive(Serialize)]
struct CsvRow<'a> {
    url: &'a str,
    status: u16,
    depth: usize,
    title: &'a str,
    content_type: &'a str,
    size: u64,
    elapsed_ms: u64,
    links: usize,
    js_detected: bool,
    scripts: usize,
    external_scripts: usize,
    inline_scripts: usize,
    event_handlers: usize,
    javascript_urls: usize,
    frameworks: String,
}

fn write_csv(path: &Path, result: &CrawlResult) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    for page in &result.pages {
        writer.serialize(CsvRow {
            url: &page.final_url,
            status: page.status,
            depth: page.depth,
            title: page.title.as_deref().unwrap_or(""),
            content_type: page.content_type.as_deref().unwrap_or(""),
            size: page.size,
            elapsed_ms: page.elapsed_ms,
            links: page.links.len(),
            js_detected: page.js.detected,
            scripts: page.js.total_scripts,
            external_scripts: page.js.external_scripts,
            inline_scripts: page.js.inline_scripts,
            event_handlers: page.js.event_handlers,
            javascript_urls: page.js.javascript_urls,
            frameworks: page.js.frameworks.join("|"),
        })?;
    }
    writer.flush()?;
    Ok(())
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn write_sitemap(path: &Path, result: &CrawlResult) -> Result<()> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for page in result
        .pages
        .iter()
        .filter(|p| (200..300).contains(&p.status))
    {
        out.push_str("  <url><loc>");
        out.push_str(&xml_escape(&page.final_url));
        out.push_str("</loc></url>\n");
    }
    out.push_str("</urlset>\n");
    fs::write(path, out).with_context(|| format!("failed to write {}", path.display()))
}

fn write_text(path: &Path, result: &CrawlResult) -> Result<()> {
    let mut out = String::new();
    for page in &result.pages {
        out.push_str(&format!("=== {} ({}) ===\n", page.final_url, page.status));
        if let Some(title) = &page.title {
            out.push_str(&format!("# {title}\n"));
        }
        if let Some(text) = &page.text {
            out.push_str(text);
            out.push_str("\n\n");
        }
    }
    fs::write(path, out).with_context(|| format!("failed to write {}", path.display()))
}

pub struct Summary {
    pub pages: usize,
    pub ok: usize,
    pub failed: usize,
    pub bytes: u64,
    pub links: usize,
    pub js_pages: usize,
    pub scripts: usize,
    pub handlers: usize,
}

pub fn summarize(result: &CrawlResult) -> Summary {
    let mut summary = Summary {
        pages: result.pages.len(),
        ok: 0,
        failed: 0,
        bytes: 0,
        links: 0,
        js_pages: 0,
        scripts: 0,
        handlers: 0,
    };
    for page in &result.pages {
        if (200..400).contains(&page.status) {
            summary.ok += 1;
        } else {
            summary.failed += 1;
        }
        summary.bytes += page.size;
        summary.links += page.links.len();
        if page.js.detected {
            summary.js_pages += 1;
        }
        summary.scripts += page.js.total_scripts;
        summary.handlers += page.js.event_handlers;
    }
    summary.failed += result.errors.len();
    summary
}
