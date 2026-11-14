use scraper::{Html, Node, Selector};
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct JsReport {
    pub detected: bool,
    pub total_scripts: usize,
    pub external_scripts: usize,
    pub inline_scripts: usize,
    pub inline_bytes: usize,
    pub event_handlers: usize,
    pub javascript_urls: usize,
    pub noscript: bool,
    pub frameworks: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct StripOptions {
    pub scripts: bool,
    pub styles: bool,
    pub handlers: bool,
    pub js_urls: bool,
}

impl StripOptions {
    pub fn for_text() -> Self {
        StripOptions {
            scripts: true,
            styles: true,
            handlers: true,
            js_urls: true,
        }
    }

    pub fn for_html() -> Self {
        StripOptions {
            scripts: true,
            styles: false,
            handlers: true,
            js_urls: true,
        }
    }
}

const FRAMEWORK_SIGNATURES: &[(&str, &str)] = &[
    ("react-dom", "React"),
    ("react.production", "React"),
    ("__next", "Next.js"),
    ("/_next/", "Next.js"),
    ("next/static", "Next.js"),
    ("vue.global", "Vue"),
    ("vue.min", "Vue"),
    ("vue.runtime", "Vue"),
    ("nuxt", "Nuxt"),
    ("@angular", "Angular"),
    ("angular.min", "Angular"),
    ("jquery", "jQuery"),
    ("svelte", "Svelte"),
    ("gatsby", "Gatsby"),
    ("webpack", "webpack"),
    ("googletagmanager", "Google Tag Manager"),
    ("gtag(", "Google Analytics"),
    ("cloudflare", "Cloudflare"),
    ("cdn-cgi", "Cloudflare"),
    ("htmx", "htmx"),
    ("alpinejs", "Alpine.js"),
    ("bootstrap.bundle", "Bootstrap"),
    ("wordpress", "WordPress"),
    ("wp-content", "WordPress"),
    ("wp-includes", "WordPress"),
    ("shopify", "Shopify"),
    ("wix.com", "Wix"),
    ("squarespace", "Squarespace"),
    ("drupal", "Drupal"),
    ("vercel", "Vercel"),
    ("modernizr", "Modernizr"),
    ("polyfill", "Polyfill"),
];

fn is_js_url(value: &str) -> bool {
    value
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("javascript:")
}

fn register_framework(haystack: &str, out: &mut Vec<String>) {
    let lower = haystack.to_ascii_lowercase();
    for (needle, label) in FRAMEWORK_SIGNATURES {
        if lower.contains(needle) && !out.iter().any(|existing| existing == label) {
            out.push((*label).to_string());
        }
    }
}

pub fn analyze(doc: &Html) -> JsReport {
    let mut report = JsReport::default();

    let script_selector = Selector::parse("script").unwrap();
    for script in doc.select(&script_selector) {
        report.total_scripts += 1;
        let element = script.value();
        let src = element.attr("src");
        let has_external = src.map(|s| !s.trim().is_empty()).unwrap_or(false);
        if has_external {
            report.external_scripts += 1;
            if let Some(src) = src {
                register_framework(src, &mut report.frameworks);
            }
        } else {
            report.inline_scripts += 1;
        }
        let text: String = script.text().collect();
        report.inline_bytes += text.len();
        if !text.is_empty() {
            register_framework(&text, &mut report.frameworks);
        }
    }

    let all_selector = Selector::parse("*").unwrap();
    for element in doc.select(&all_selector) {
        let value = element.value();
        if value.name() == "noscript" {
            report.noscript = true;
        }
        for (name, attr_value) in &value.attrs {
            let local: &str = &name.local;
            let local = local.to_ascii_lowercase();
            if local.len() > 2 && local.starts_with("on") {
                report.event_handlers += 1;
            }
            let attr_value: &str = attr_value;
            if is_js_url(attr_value) {
                report.javascript_urls += 1;
            }
        }
    }

    report.detected =
        report.total_scripts > 0 || report.event_handlers > 0 || report.javascript_urls > 0;
    report.frameworks.sort();
    report.frameworks.dedup();
    report
}

pub fn strip(mut doc: Html, opts: StripOptions) -> String {
    let ids: Vec<_> = doc
        .tree
        .root()
        .descendants()
        .map(|node| node.id())
        .collect();

    for id in ids {
        let Some(mut node_mut) = doc.tree.get_mut(id) else {
            continue;
        };

        let remove_node = match node_mut.value() {
            Node::Element(element) => {
                let name = element.name();
                (opts.scripts && name == "script") || (opts.styles && name == "style")
            }
            _ => false,
        };
        if remove_node {
            node_mut.detach();
            continue;
        }

        if let Node::Element(element) = node_mut.value() {
            element.attrs.retain(|(name, value)| {
                let local: &str = &name.local;
                let local = local.to_ascii_lowercase();
                let value: &str = value;

                if opts.handlers && local.len() > 2 && local.starts_with("on") {
                    return false;
                }
                if opts.js_urls
                    && matches!(local.as_str(), "href" | "src" | "action" | "formaction")
                    && is_js_url(value)
                {
                    return false;
                }
                true
            });
        }
    }

    doc.html()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<html><head>
        <script src="/_next/app.js"></script>
        <script>console.log("hi")</script>
        </head><body>
        <a href="/ok">ok</a>
        <a href="javascript:void(0)" onclick="go()">bad</a>
        <div onmouseover="x()">div</div>
        <noscript>enable</noscript>
        </body></html>"#;

    #[test]
    fn detects_javascript_usage() {
        let doc = Html::parse_document(SAMPLE);
        let report = analyze(&doc);
        assert!(report.detected);
        assert_eq!(report.total_scripts, 2);
        assert_eq!(report.external_scripts, 1);
        assert_eq!(report.inline_scripts, 1);
        assert_eq!(report.event_handlers, 2);
        assert_eq!(report.javascript_urls, 1);
        assert!(report.noscript);
        assert!(report.frameworks.contains(&"Next.js".to_string()));
    }

    #[test]
    fn strips_scripts_handlers_and_js_urls() {
        let doc = Html::parse_document(SAMPLE);
        let stripped = strip(doc, StripOptions::for_html());
        let lower = stripped.to_ascii_lowercase();
        assert!(!lower.contains("<script"));
        assert!(!lower.contains("onclick"));
        assert!(!lower.contains("onmouseover"));
        assert!(!lower.contains("javascript:"));
        assert!(lower.contains("enable"));
        assert!(lower.contains("href=\"/ok\""));
    }
}
