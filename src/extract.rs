use scraper::{Html, Selector};
use serde::Serialize;
use url::Url;

use crate::http::FetchResult;
use crate::js::{self, JsReport, StripOptions};
use crate::util::{looks_like_page, normalize_whitespace};

const MAX_TEXT_CHARS: usize = 200_000;

#[derive(Debug, Clone, Serialize)]
pub struct PageData {
    pub url: String,
    pub final_url: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub depth: usize,
    pub elapsed_ms: u64,
    pub size: u64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub lang: Option<String>,
    pub canonical: Option<String>,
    pub headings: Vec<String>,
    pub links: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub js: JsReport,
}

pub struct Extracted {
    pub page: PageData,
    pub stored_html: Option<String>,
}

pub struct ExtractOptions<'a> {
    pub remove_js: bool,
    pub want_text: bool,
    pub store_html: bool,
    pub max_links: Option<usize>,
    pub exclude: &'a [String],
}

fn selector(input: &str) -> Selector {
    Selector::parse(input).expect("static selector must be valid")
}

fn first_text(doc: &Html, sel: &str) -> Option<String> {
    let s = selector(sel);
    doc.select(&s)
        .next()
        .map(|e| normalize_whitespace(&e.text().collect::<String>()))
        .filter(|t| !t.is_empty())
}

fn meta_content(doc: &Html, key: &str) -> Option<String> {
    let sel = selector(&format!("meta[name=\"{key}\"]"));
    if let Some(value) = doc
        .select(&sel)
        .next()
        .and_then(|e| e.value().attr("content"))
    {
        let value = normalize_whitespace(value);
        if !value.is_empty() {
            return Some(value);
        }
    }
    let sel = selector(&format!("meta[property=\"{key}\"]"));
    doc.select(&sel)
        .next()
        .and_then(|e| e.value().attr("content"))
        .map(normalize_whitespace)
        .filter(|v| !v.is_empty())
}

fn extract_text(html: &str) -> String {
    let doc = Html::parse_document(html);
    let body = selector("body");
    let mut text = String::new();
    if let Some(body) = doc.select(&body).next() {
        for chunk in body.text() {
            text.push_str(chunk);
            text.push(' ');
        }
    } else {
        for chunk in doc.root_element().text() {
            text.push_str(chunk);
            text.push(' ');
        }
    }
    let normalized = normalize_whitespace(&text);
    if normalized.chars().count() > MAX_TEXT_CHARS {
        normalized.chars().take(MAX_TEXT_CHARS).collect()
    } else {
        normalized
    }
}

fn extract_links(
    doc: &Html,
    base: &Url,
    max_links: Option<usize>,
    exclude: &[String],
) -> Vec<String> {
    let mut links = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let base_href = doc
        .select(&selector("base[href]"))
        .next()
        .and_then(|e| e.value().attr("href"))
        .and_then(|href| base.join(href).ok())
        .unwrap_or_else(|| base.clone());

    let anchor = selector("a[href]");
    for element in doc.select(&anchor) {
        let Some(raw) = element.value().attr("href") else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        if lower.starts_with("javascript:")
            || lower.starts_with("mailto:")
            || lower.starts_with("tel:")
            || lower.starts_with("data:")
        {
            continue;
        }
        let Ok(mut resolved) = base_href.join(raw) else {
            continue;
        };
        if !matches!(resolved.scheme(), "http" | "https") {
            continue;
        }
        if !looks_like_page(&resolved) {
            continue;
        }
        resolved.set_fragment(None);
        let href = resolved.to_string();
        if exclude
            .iter()
            .any(|pattern| href.contains(pattern.as_str()))
        {
            continue;
        }
        if seen.insert(href.clone()) {
            links.push(href);
            if let Some(limit) = max_links
                && links.len() >= limit
            {
                break;
            }
        }
    }
    links
}

pub fn analyze(
    requested_url: &str,
    fetch: &FetchResult,
    depth: usize,
    opts: ExtractOptions<'_>,
) -> Extracted {
    let ExtractOptions {
        remove_js,
        want_text,
        store_html,
        max_links,
        exclude,
    } = opts;
    let content_type = fetch.content_type.clone();
    let is_html = content_type
        .as_deref()
        .map(|c| c.contains("html") || c.contains("xml"))
        .unwrap_or_else(|| fetch.body.trim_start().starts_with('<'));

    if !is_html {
        let text = if want_text {
            Some(normalize_whitespace(&fetch.body))
        } else {
            None
        };
        return Extracted {
            page: PageData {
                url: requested_url.to_string(),
                final_url: fetch.final_url.clone(),
                status: fetch.status,
                content_type,
                depth,
                elapsed_ms: fetch.elapsed.as_millis() as u64,
                size: fetch.size as u64,
                title: None,
                description: None,
                lang: None,
                canonical: None,
                headings: Vec::new(),
                links: Vec::new(),
                text,
                js: JsReport::default(),
            },
            stored_html: if store_html {
                Some(fetch.body.clone())
            } else {
                None
            },
        };
    }

    let doc = Html::parse_document(&fetch.body);
    let js_report = js::analyze(&doc);

    let base = Url::parse(&fetch.final_url).unwrap_or_else(|_| Url::parse(requested_url).unwrap());

    let title = first_text(&doc, "title").or_else(|| meta_content(&doc, "og:title"));
    let description =
        meta_content(&doc, "description").or_else(|| meta_content(&doc, "og:description"));
    let lang = doc
        .select(&selector("html[lang]"))
        .next()
        .and_then(|e| e.value().attr("lang"))
        .map(|s| s.to_string());
    let canonical = doc
        .select(&selector("link[rel=\"canonical\"]"))
        .next()
        .and_then(|e| e.value().attr("href"))
        .and_then(|href| base.join(href).ok())
        .map(|u| u.to_string());

    let mut headings = Vec::new();
    for element in doc.select(&selector("h1, h2, h3")) {
        let text = normalize_whitespace(&element.text().collect::<String>());
        if !text.is_empty() && headings.len() < 20 {
            headings.push(text);
        }
    }

    let links = extract_links(&doc, &base, max_links, exclude);

    let stored_html = if store_html {
        if remove_js {
            Some(js::strip(doc.clone(), StripOptions::for_html()))
        } else {
            Some(doc.html())
        }
    } else {
        None
    };

    let text = if want_text {
        let for_text = js::strip(doc.clone(), StripOptions::for_text());
        Some(extract_text(&for_text))
    } else {
        None
    };

    Extracted {
        page: PageData {
            url: requested_url.to_string(),
            final_url: fetch.final_url.clone(),
            status: fetch.status,
            content_type,
            depth,
            elapsed_ms: fetch.elapsed.as_millis() as u64,
            size: fetch.size as u64,
            title,
            description,
            lang,
            canonical,
            headings,
            links,
            text,
            js: js_report,
        },
        stored_html,
    }
}
