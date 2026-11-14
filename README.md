# rustrequest

A fast web scraper and crawler written in Rust, with a graphical desktop UI, an animated terminal UI, and a headless CLI. It can crawl a site by following links or scrape a fixed list of URLs, while analyzing pages for JavaScript usage and extracting metadata, readable text, and links.

## Features

- Three frontends: native GUI, animated TUI, and headless CLI output
- Two GUI tabs: **Scrape** (fetch each URL once) and **Crawl** (follow links in BFS order)
- Concurrent fetching with configurable workers, depth, page limits, and URL exclusions
- JavaScript analysis: inline and external scripts, event handlers, `javascript:` URLs, `<noscript>`, and framework hints (React, Next.js, Vue)
- Optional JavaScript removal from saved HTML and extracted text
- `robots.txt` support, including wildcard rules and `Crawl-delay`
- HTTP/HTTPS/SOCKS5 proxy support, custom headers, cookies, retries with backoff, and timeouts
- Metadata extraction: title, description, language, canonical URL, headings, links, and readable text
- Output as JSON, JSON Lines, CSV, sitemap XML, or plain text
- Optional HTML snapshots, live progress metrics, and graceful cancellation

## Installation

Requires a recent stable Rust toolchain.

```bash
git clone https://github.com/linuxguytech52-sys/rustrequest.git
cd rustrequest
cargo build --release
cargo install --path .
```

Install directly from Git:

```bash
cargo install --git https://github.com/linuxguytech52-sys/rustrequest.git
```

The GUI uses `eframe`/`wgpu`, so a working display server (X11 or Wayland) and graphics drivers are required. If the GUI cannot start, the app automatically falls back to headless mode.

## Quick start

```bash
# Opens the GUI when a display is available
rustrequest https://example.com

# Crawl up to 100 pages, two levels deep, through a proxy
rustrequest https://example.com -d 2 -m 100 -p socks5://127.0.0.1:1080

# Scrape a fixed list of URLs without following links
rustrequest https://example.com https://example.com/about --fetch

# Force the terminal UI, respect robots.txt, and write a sitemap
rustrequest https://example.com --tui --respect-robots -f sitemap -o sitemap.xml

# Headless JSONL output with extra diagnostics
rustrequest https://example.com --no-tui -f jsonl -v
```

Missing schemes are assumed to be `https://`.

## Frontend selection

| Flag | Behavior |
| --- | --- |
| `--gui` | Launch the graphical UI |
| `--tui` | Force the animated terminal UI, even for non-interactive output |
| `--no-tui` | Never use the terminal UI; print headless summaries |
| *(none)* | GUI when a display is detected, otherwise TUI for terminals, otherwise headless |

In the GUI, `Ctrl+Enter` starts or stops the current scrape or crawl.

## Scrape vs. crawl

| Mode | CLI flag | GUI tab | Behavior |
| --- | --- | --- | --- |
| Crawl | *(default)* | Crawl | Starts from seed URLs, follows discovered links (BFS), limited by depth, page count, and domain rules |
| Scrape | `--fetch` | Scrape | Fetches each supplied URL exactly once; links are extracted but not followed |

## Common options

| Option | Description |
| --- | --- |
| `-d, --depth <N>` | Maximum crawl depth (`0` = seed pages only) |
| `-m, --max-pages <N>` | Maximum number of pages to fetch |
| `-c, --concurrency <N>` | Number of concurrent requests |
| `--any-domain` | Allow crawling across different domains |
| `--include-subdomains` | Include subdomains of the seed host |
| `-p, --proxy <URL>` | Proxy URL (`http`, `https`, or `socks5`) |
| `--timeout <SECONDS>` | Per-request timeout |
| `--delay <MS>` | Politeness delay between requests |
| `--retries <N>` | Retries for failed or `429`/`5xx` responses |
| `-A, --user-agent <UA>` | User-Agent string (defaults to this repository URL) |
| `-H, --header "Name: value"` | Extra request header, repeatable |
| `--respect-robots` | Honor `robots.txt` rules and crawl delays |
| `--keep-js` | Keep JavaScript when saving HTML or extracting text |
| `--no-text` | Do not extract readable page text |
| `--insecure` | Accept invalid TLS certificates |
| `-f, --format <FORMAT>` | `json`, `jsonl`, `csv`, `sitemap`, or `text` |
| `-o, --output <PATH>` | Output path (defaults to `rustrequest-output.<ext>`) |
| `--save-html <DIR>` | Save fetched HTML into a directory |
| `--fetch` | Fetch seed URLs only; do not follow links |
| `--max-links <N>` | Limit extracted links per page |
| `--exclude <SUBSTRING>` | Skip URLs containing a substring, repeatable |
| `-v, --verbose` | Print extra diagnostics |

Run `rustrequest --help` for the full list.

## Output

The default output file is `rustrequest-output.<ext>`, where the extension depends on the format:

| Format | Extension | Contents |
| --- | --- | --- |
| `json` | `.json` | Pretty-printed array of page objects |
| `jsonl` | `.jsonl` | One page object per line |
| `csv` | `.csv` | URL, status, depth, title, size, timing, link and JS counts, frameworks |
| `sitemap` | `.xml` | Sitemap containing URLs with `2xx` responses |
| `text` | `.txt` | Human-readable per-page summary with extracted text |

JSON page objects include:

```json
{
  "url": "https://example.com/",
  "final_url": "https://example.com/",
  "status": 200,
  "content_type": "text/html",
  "depth": 0,
  "elapsed_ms": 113,
  "size": 1256,
  "title": "Example Domain",
  "description": "Example description",
  "lang": "en",
  "canonical": "https://example.com/",
  "headings": ["Example Domain"],
  "links": ["https://example.com/about"],
  "text": "Example Domain ...",
  "js": {
    "detected": true,
    "total_scripts": 2,
    "external_scripts": 1,
    "inline_scripts": 1,
    "inline_bytes": 512,
    "event_handlers": 0,
    "javascript_urls": 0,
    "noscript": false,
    "frameworks": ["React"]
  }
}
```

The `text` field is omitted when text extraction is disabled or no text is available.

## JavaScript handling

Pages are always analyzed for JavaScript usage. By default, scripts, inline event handlers, `javascript:` URLs, and related markup are removed from saved HTML and extracted text. Use `--keep-js` to preserve them.

## Project layout

| Path | Purpose |
| --- | --- |
| `src/main.rs` | Entry point and frontend dispatch |
| `src/cli.rs` | Command-line arguments and output formats |
| `src/config.rs` | Runtime configuration and frontend auto-detection |
| `src/crawler.rs` | BFS crawl engine, scraping mode, events, cancellation |
| `src/http.rs` | HTTP client, proxy/TLS setup, retries |
| `src/robots.rs` | `robots.txt` parsing and caching |
| `src/extract.rs` | Metadata, text, and link extraction |
| `src/js.rs` | JavaScript detection and stripping |
| `src/export.rs` | JSON/JSONL/CSV/sitemap/text writers |
| `src/gui.rs` | egui/eframe desktop UI |
| `src/tui.rs` | Animated terminal UI |
| `src/headless.rs` | Headless console output |

## Development

```bash
cargo fmt
cargo clippy --all-targets
cargo test
cargo build --release
```

## License

MIT. See [LICENSE](LICENSE).
