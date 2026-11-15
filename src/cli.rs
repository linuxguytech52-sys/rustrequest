use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Json,
    Jsonl,
    Csv,
    Sitemap,
    Text,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Json => "json",
            OutputFormat::Jsonl => "jsonl",
            OutputFormat::Csv => "csv",
            OutputFormat::Sitemap => "xml",
            OutputFormat::Text => "txt",
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "rustrequest",
    version,
    about = "A fast Rust web scraper & crawler with a graphical and terminal UI",
    long_about = None,
)]
pub struct Cli {
    /// Seed URL(s) to crawl. Missing schemes are assumed to be https://
    #[arg(value_name = "URL", required = true)]
    pub urls: Vec<String>,

    /// Maximum crawl depth (0 = seed pages only)
    #[arg(short = 'd', long, default_value_t = 2)]
    pub depth: usize,

    /// Maximum number of pages to fetch
    #[arg(short = 'm', long = "max-pages", default_value_t = 50)]
    pub max_pages: usize,

    /// Number of concurrent requests
    #[arg(short = 'c', long, default_value_t = 8)]
    pub concurrency: usize,

    /// Allow crawling across different domains
    #[arg(long)]
    pub any_domain: bool,

    /// Include subdomains of the seed host when staying on-domain
    #[arg(long)]
    pub include_subdomains: bool,

    /// Proxy URL (http, https or socks5), e.g. http://127.0.0.1:8080
    #[arg(short = 'p', long)]
    pub proxy: Option<String>,

    /// Per-request timeout in seconds
    #[arg(long, default_value_t = 15)]
    pub timeout: u64,

    /// Politeness delay between requests in milliseconds
    #[arg(long, default_value_t = 0)]
    pub delay: u64,

    /// Number of retries for failed requests
    #[arg(long, default_value_t = 2)]
    pub retries: u32,

    /// User-Agent string
    #[arg(
        short = 'A',
        long = "user-agent",
        default_value = "rustrequest/0.1 (+https://github.com/linuxguytech52-sys/rustrequest)"
    )]
    pub user_agent: String,

    /// Extra request header, repeatable. Format: "Name: value"
    #[arg(short = 'H', long = "header", value_name = "NAME: VALUE")]
    pub headers: Vec<String>,

    /// Respect robots.txt rules (enabled by default)
    #[arg(long, conflicts_with = "ignore_robots")]
    pub respect_robots: bool,

    /// Ignore robots.txt rules
    #[arg(long, conflicts_with = "respect_robots")]
    pub ignore_robots: bool,

    /// Keep script and style content when extracting readable text
    #[arg(long)]
    pub keep_js: bool,

    /// Do not extract page text
    #[arg(long)]
    pub no_text: bool,

    /// Accept invalid TLS certificates
    #[arg(long)]
    pub insecure: bool,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = OutputFormat::Json)]
    pub format: OutputFormat,

    /// Output file path (defaults to rustrequest-output.<ext>)
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,

    /// Fetch seed URLs only; do not follow links
    #[arg(long)]
    pub fetch: bool,

    /// Never use the animated terminal UI
    #[arg(long)]
    pub no_tui: bool,

    /// Force the animated terminal UI even for non-interactive output
    #[arg(long)]
    pub tui: bool,

    /// Launch the graphical desktop UI
    #[arg(long)]
    pub gui: bool,

    /// Limit number of extracted links per page
    #[arg(long)]
    pub max_links: Option<usize>,

    /// Skip URLs containing any of these substrings (repeatable)
    #[arg(long = "exclude", value_name = "SUBSTRING")]
    pub exclude: Vec<String>,

    /// Print extra diagnostics
    #[arg(short = 'v', long)]
    pub verbose: bool,
}
