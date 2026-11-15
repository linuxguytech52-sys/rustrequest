use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, FontId, Layout, Margin, Rect, RichText, Sense, Stroke, Ui, pos2,
    vec2,
};
use tokio::sync::mpsc::error::TryRecvError;

use crate::cli::OutputFormat;
use crate::config::{self, Config};
use crate::crawler::{self, CrawlEvent, LogLevel, PageLog};
use crate::export;
use crate::http;
use crate::util;

const BG: Color32 = Color32::from_rgb(11, 13, 20);
const PANEL: Color32 = Color32::from_rgb(17, 20, 30);
const CARD: Color32 = Color32::from_rgb(24, 28, 41);
const CARD_SOFT: Color32 = Color32::from_rgb(20, 24, 36);
const BORDER: Color32 = Color32::from_rgb(40, 47, 66);
const TEXT: Color32 = Color32::from_rgb(226, 232, 240);
const MUTED: Color32 = Color32::from_rgb(133, 145, 168);
const ACCENT: Color32 = Color32::from_rgb(86, 200, 255);
const GREEN: Color32 = Color32::from_rgb(80, 220, 140);
const YELLOW: Color32 = Color32::from_rgb(242, 201, 76);
const RED: Color32 = Color32::from_rgb(244, 108, 108);
const PURPLE: Color32 = Color32::from_rgb(188, 160, 255);
const ORANGE: Color32 = Color32::from_rgb(255, 170, 90);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Scrape,
    Crawl,
}

impl Tab {
    fn verb(self) -> &'static str {
        match self {
            Tab::Scrape => "scrape",
            Tab::Crawl => "crawl",
        }
    }
}

fn hue_color(hue: f32, sat: f32, val: f32) -> Color32 {
    let (r, g, b) = util::hsv_to_rgb(hue as f64, sat as f64, val as f64);
    Color32::from_rgb(r, g, b)
}

#[derive(Default)]
struct RunStats {
    done: usize,
    queued: usize,
    max_pages: usize,
    failed: usize,
    bytes: u64,
    links: usize,
    js_pages: usize,
    scripts: usize,
    external_scripts: usize,
    handlers: usize,
    js_urls: usize,
    status_counts: BTreeMap<u16, usize>,
    frameworks: BTreeMap<String, usize>,
}

struct FeedEntry {
    log: PageLog,
    at: Instant,
}

struct NoteEntry {
    level: LogLevel,
    message: String,
    at: Instant,
}

enum FeedItem {
    Page(FeedEntry),
    Note(NoteEntry),
}

struct Form {
    urls: String,
    depth: usize,
    max_pages: usize,
    concurrency: usize,
    proxy: String,
    timeout: u64,
    delay: u64,
    retries: u32,
    user_agent: String,
    headers: String,
    exclude: String,
    respect_robots: bool,
    remove_js: bool,
    extract_text: bool,
    insecure: bool,
    same_domain: bool,
    include_subdomains: bool,
    max_links: usize,
    format: OutputFormat,
    output_dir: String,
}

impl Form {
    fn from_config(cfg: &Config) -> Self {
        Form {
            urls: cfg
                .seeds
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            depth: cfg.max_depth,
            max_pages: cfg.max_pages,
            concurrency: cfg.concurrency,
            proxy: cfg.proxy.clone().unwrap_or_default(),
            timeout: cfg.timeout.as_secs(),
            delay: cfg.delay.as_millis() as u64,
            retries: cfg.retries,
            user_agent: cfg.user_agent.clone(),
            headers: cfg
                .headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join("\n"),
            exclude: cfg.exclude.join(", "),
            respect_robots: cfg.respect_robots,
            remove_js: cfg.remove_js,
            extract_text: cfg.extract_text,
            insecure: cfg.insecure,
            same_domain: cfg.same_domain,
            include_subdomains: cfg.include_subdomains,
            max_links: cfg.max_links.unwrap_or(0),
            format: cfg.format,
            output_dir: cfg
                .output
                .as_ref()
                .and_then(|p| p.parent())
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        }
    }
}

pub struct RustRequestApp {
    form: Form,
    tab: Tab,
    running: bool,
    cancel: Arc<AtomicBool>,
    rx: Option<tokio::sync::mpsc::UnboundedReceiver<CrawlEvent>>,
    stats: RunStats,
    feed: VecDeque<FeedItem>,
    started: Option<Instant>,
    finished_secs: Option<f64>,
    progress: f32,
    target_progress: f32,
    throughput: Vec<f32>,
    sec_pages: u64,
    last_second: Instant,
    output_msg: Option<String>,
    error_msg: Option<String>,
    config_error: Option<String>,
    toast: Option<(String, Instant)>,
}

impl RustRequestApp {
    fn new(cfg: Config) -> Self {
        let stats = RunStats {
            max_pages: cfg.max_pages,
            ..Default::default()
        };
        RustRequestApp {
            tab: if cfg.fetch_only {
                Tab::Scrape
            } else {
                Tab::Crawl
            },
            form: Form::from_config(&cfg),
            running: false,
            cancel: Arc::new(AtomicBool::new(false)),
            rx: None,
            stats,
            feed: VecDeque::new(),
            started: None,
            finished_secs: None,
            progress: 0.0,
            target_progress: 0.0,
            throughput: Vec::new(),
            sec_pages: 0,
            last_second: Instant::now(),
            output_msg: None,
            error_msg: None,
            config_error: None,
            toast: None,
        }
    }

    fn build_config(&self) -> anyhow::Result<Config> {
        let mut seeds = Vec::new();
        for line in self.form.urls.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            seeds.push(config::normalize_url(line)?);
        }
        if seeds.is_empty() {
            anyhow::bail!("enter at least one URL");
        }

        let mut headers = Vec::new();
        for line in self.form.headers.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                anyhow::bail!("invalid header '{line}', expected \"Name: value\"");
            };
            if name.trim().is_empty() {
                anyhow::bail!("invalid header '{line}', empty name");
            }
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }

        let exclude: Vec<String> = self
            .form
            .exclude
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let output_dir = non_empty(&self.form.output_dir).map(PathBuf::from);
        if let Some(dir) = &output_dir
            && dir.exists()
            && !dir.is_dir()
        {
            anyhow::bail!(
                "output directory '{}' is a file; choose a directory instead",
                dir.display()
            );
        }
        let output = output_dir.map(|dir| {
            dir.join(format!(
                "rustrequest-output.{}",
                self.form.format.extension()
            ))
        });

        let fetch_only = self.tab == Tab::Scrape;
        let max_depth = if fetch_only { 0 } else { self.form.depth };
        let max_pages = if fetch_only {
            seeds.len()
        } else {
            self.form.max_pages.max(1)
        };

        Ok(Config {
            seeds,
            max_depth,
            max_pages,
            concurrency: self.form.concurrency.max(1),
            same_domain: self.form.same_domain,
            include_subdomains: self.form.include_subdomains,
            proxy: non_empty(&self.form.proxy),
            timeout: Duration::from_secs(self.form.timeout.max(1)),
            delay: Duration::from_millis(self.form.delay),
            retries: self.form.retries,
            user_agent: self.form.user_agent.clone(),
            headers,
            respect_robots: self.form.respect_robots,
            remove_js: self.form.remove_js,
            extract_text: self.form.extract_text,
            insecure: self.form.insecure,
            fetch_only,
            format: self.form.format,
            output,
            verbose: false,
            max_links: if self.form.max_links == 0 {
                None
            } else {
                Some(self.form.max_links)
            },
            exclude,
            tui: false,
            gui: true,
        })
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.config_error = None;
        self.error_msg = None;
        self.output_msg = None;
        self.feed.clear();
        self.throughput.clear();
        self.started = None;
        self.finished_secs = None;
        self.progress = 0.0;
        self.target_progress = 0.0;
        self.stats = RunStats::default();
    }

    fn url_count(&self) -> usize {
        self.form
            .urls
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    }

    fn output_preview(&self) -> String {
        let dir = non_empty(&self.form.output_dir).unwrap_or_else(|| ".".to_string());
        PathBuf::from(dir)
            .join(format!(
                "rustrequest-output.{}",
                self.form.format.extension()
            ))
            .display()
            .to_string()
    }

    fn planned_pages(&self) -> usize {
        if self.started.is_some() {
            self.stats.max_pages.max(1)
        } else if self.tab == Tab::Scrape {
            self.url_count().max(1)
        } else {
            self.form.max_pages.max(1)
        }
    }

    fn start_crawl(&mut self) {
        if self.running {
            return;
        }
        let cfg = match self.build_config() {
            Ok(cfg) => cfg,
            Err(err) => {
                self.config_error = Some(format!("{err:#}"));
                self.toast = Some(("Fix the configuration first".to_string(), Instant::now()));
                return;
            }
        };

        self.running = true;
        self.config_error = None;
        self.output_msg = None;
        self.error_msg = None;
        self.stats = RunStats {
            max_pages: cfg.max_pages,
            ..Default::default()
        };
        self.feed.clear();
        self.throughput.clear();
        self.progress = 0.0;
        self.target_progress = 0.0;
        self.started = Some(Instant::now());
        self.finished_secs = None;
        self.sec_pages = 0;
        self.last_second = Instant::now();
        self.cancel = Arc::new(AtomicBool::new(false));

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.rx = Some(rx);

        let cancel = Arc::clone(&self.cancel);
        let export_cfg = cfg.clone();
        let worker_tx = tx.clone();

        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = tx.send(CrawlEvent::Done {
                        output: Err(format!("failed to start runtime: {err}")),
                    });
                    return;
                }
            };

            let output = runtime.block_on(async move {
                let client = match http::build_client(&cfg) {
                    Ok(client) => client,
                    Err(err) => return Err(format!("{err:#}")),
                };
                let result = crawler::crawl_with_cancel(cfg, client, worker_tx, cancel).await;
                match export::write(&export_cfg, &result) {
                    Ok(path) => Ok(path.display().to_string()),
                    Err(err) => Err(format!("{err:#}")),
                }
            });

            let _ = tx.send(CrawlEvent::Done { output });
        });
    }

    fn stop(&mut self) {
        if self.running {
            self.cancel.store(true, Ordering::Relaxed);
            self.running = false;
            self.toast = Some((
                "Stopping crawl and writing results…".to_string(),
                Instant::now(),
            ));
        }
    }

    fn poll_events(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = &mut self.rx {
            loop {
                match rx.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.rx = None;
        }
        for event in events {
            self.apply(event);
        }
    }

    fn apply(&mut self, event: CrawlEvent) {
        match event {
            CrawlEvent::Started {
                seeds,
                max_pages,
                max_depth: _,
                proxy,
                respect_robots,
                user_agent,
            } => {
                self.stats.max_pages = max_pages;
                self.form.urls = seeds.join("\n");
                self.form.proxy = proxy.unwrap_or_default();
                self.form.respect_robots = respect_robots;
                self.form.user_agent = user_agent;
            }
            CrawlEvent::Progress {
                done,
                queued,
                total,
            } => {
                self.stats.done = done;
                self.stats.queued = queued;
                if total > 0 {
                    self.stats.max_pages = total;
                    self.target_progress = (done as f32 / total as f32).clamp(0.0, 1.0);
                }
            }
            CrawlEvent::Page(log) => {
                self.stats.bytes += log.size;
                self.stats.links += log.links;
                self.sec_pages += 1;
                *self.stats.status_counts.entry(log.status).or_insert(0) += 1;
                if log.js.detected {
                    self.stats.js_pages += 1;
                }
                self.stats.scripts += log.js.total_scripts;
                self.stats.external_scripts += log.js.external_scripts;
                self.stats.handlers += log.js.event_handlers;
                self.stats.js_urls += log.js.javascript_urls;
                for framework in &log.js.frameworks {
                    *self.stats.frameworks.entry(framework.clone()).or_insert(0) += 1;
                }
                self.push_feed(FeedItem::Page(FeedEntry {
                    log,
                    at: Instant::now(),
                }));
            }
            CrawlEvent::Log(level, message) => {
                if level == LogLevel::Error {
                    self.stats.failed += 1;
                }
                self.push_feed(FeedItem::Note(NoteEntry {
                    level,
                    message,
                    at: Instant::now(),
                }));
            }
            CrawlEvent::Finished {
                pages,
                failed,
                elapsed_secs,
            } => {
                self.stats.failed = self.stats.failed.max(failed);
                self.finished_secs = Some(elapsed_secs);
                self.push_feed(FeedItem::Note(NoteEntry {
                    level: LogLevel::Info,
                    message: format!(
                        "{} finished · {pages} pages in {elapsed_secs:.1}s",
                        self.tab.verb()
                    ),
                    at: Instant::now(),
                }));
            }
            CrawlEvent::Done { output } => {
                self.running = false;
                self.rx = None;
                match output {
                    Ok(path) => {
                        self.output_msg = Some(path);
                        self.toast = Some(("Results saved".to_string(), Instant::now()));
                    }
                    Err(err) => self.error_msg = Some(err),
                }
                if self.started.is_some() && self.finished_secs.is_none() {
                    self.finished_secs = self.started.map(|s| s.elapsed().as_secs_f64());
                }
                self.target_progress = 1.0;
                if self.stats.done > 0 {
                    self.stats.max_pages = self.stats.done;
                }
            }
        }
    }

    fn push_feed(&mut self, item: FeedItem) {
        self.feed.push_back(item);
        while self.feed.len() > 800 {
            self.feed.pop_front();
        }
    }

    fn tick(&mut self, dt: f32) {
        let k = 1.0 - (-dt * 14.0).exp();
        self.progress += (self.target_progress - self.progress) * k;
        if (self.target_progress - self.progress).abs() < 0.0004 {
            self.progress = self.target_progress;
        }
        if self.last_second.elapsed() >= Duration::from_secs(1) {
            self.throughput.push(self.sec_pages as f32);
            if self.throughput.len() > 60 {
                self.throughput.remove(0);
            }
            self.sec_pages = 0;
            self.last_second = Instant::now();
        }
    }

    fn elapsed_secs(&self) -> f64 {
        self.started
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    fn animating(&self) -> bool {
        (self.target_progress - self.progress).abs() > 0.001
            || self
                .toast
                .as_ref()
                .map(|(_, at)| at.elapsed() < Duration::from_secs(3))
                .unwrap_or(false)
            || self
                .feed
                .back()
                .map(|item| match item {
                    FeedItem::Page(entry) => entry.at.elapsed() < Duration::from_secs(2),
                    FeedItem::Note(entry) => entry.at.elapsed() < Duration::from_secs(2),
                })
                .unwrap_or(false)
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let start = ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::Enter,
            ))
        });
        if start {
            if self.running {
                self.stop();
            } else {
                self.start_crawl();
            }
        }
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn format_label(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Json => "JSON",
        OutputFormat::Jsonl => "JSON Lines",
        OutputFormat::Csv => "CSV",
        OutputFormat::Sitemap => "Sitemap XML",
        OutputFormat::Text => "Plain text",
    }
}

fn status_color(status: u16) -> Color32 {
    if (200..300).contains(&status) {
        GREEN
    } else if (300..400).contains(&status) {
        ACCENT
    } else if (400..500).contains(&status) {
        YELLOW
    } else {
        RED
    }
}

impl eframe::App for RustRequestApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let time = ctx.input(|i| i.time) as f32;
        let dt = ctx.input(|i| i.stable_dt).min(0.1);

        self.poll_events();
        self.tick(dt);
        self.handle_shortcuts(&ctx);

        if self.running {
            ctx.request_repaint();
        } else if self.animating() {
            ctx.request_repaint_after(Duration::from_millis(40));
        }

        egui::Panel::top("header")
            .exact_size(86.0)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(18, 10)),
            )
            .show(ui, |ui| self.draw_header(ui, time));

        egui::Panel::bottom("status_bar")
            .exact_size(36.0)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(18, 8)),
            )
            .show(ui, |ui| draw_status_bar(ui, self, time));

        egui::Panel::left("config_panel")
            .resizable(true)
            .default_size(344.0)
            .min_size(300.0)
            .max_size(480.0)
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(14, 12)),
            )
            .show(ui, |ui| self.draw_config(ui, time));

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(Margin::symmetric(16, 12)),
            )
            .show(ui, |ui| self.draw_dashboard(ui, time));
    }
}

impl RustRequestApp {
    fn draw_header(&mut self, ui: &mut Ui, time: f32) {
        let rect = ui.max_rect();
        let painter = ui.painter().clone();
        let steps = 48;
        for i in 0..steps {
            let t0 = i as f32 / steps as f32;
            let t1 = (i + 1) as f32 / steps as f32;
            let hue = (time * 16.0 + t0 * 70.0 + 205.0) % 360.0;
            let color = hue_color(hue, 0.38, 0.17);
            let x0 = rect.left() + rect.width() * t0;
            let x1 = rect.left() + rect.width() * t1;
            painter.rect_filled(
                Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom())),
                0,
                color,
            );
        }

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (i, ch) in "rustrequest".chars().enumerate() {
                        let hue = (time * 70.0 + i as f32 * 18.0) % 360.0;
                        ui.label(
                            RichText::new(ch.to_string())
                                .size(28.0)
                                .strong()
                                .color(hue_color(hue, 0.75, 1.0)),
                        );
                    }
                });
                ui.label(
                    RichText::new(
                        "Rust web scraper & crawler · JS analysis · proxy support · live metrics",
                    )
                    .size(12.0)
                    .color(Color32::from_rgb(158, 170, 194)),
                );
            });

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (state, color) = if self.running {
                    (
                        if self.tab == Tab::Scrape {
                            "scraping"
                        } else {
                            "crawling"
                        },
                        ACCENT,
                    )
                } else if self.error_msg.is_some() {
                    ("error", RED)
                } else if self.output_msg.is_some() {
                    ("complete", GREEN)
                } else {
                    ("idle", MUTED)
                };
                badge(ui, state, color);

                let action = if self.tab == Tab::Scrape {
                    "scrape"
                } else {
                    "crawl"
                };
                let button = if self.running {
                    egui::Button::new(
                        RichText::new(format!("Stop {}", self.tab.verb()))
                            .strong()
                            .size(14.0)
                            .color(Color32::WHITE),
                    )
                    .fill(RED.gamma_multiply(0.85))
                    .corner_radius(10)
                    .min_size(vec2(110.0, 38.0))
                } else {
                    egui::Button::new(
                        RichText::new(format!("Start {action}"))
                            .strong()
                            .size(14.0)
                            .color(Color32::from_rgb(8, 14, 22)),
                    )
                    .fill(ACCENT)
                    .corner_radius(10)
                    .min_size(vec2(150.0, 38.0))
                };
                if ui.add(button).clicked() {
                    if self.running {
                        self.stop();
                    } else {
                        self.start_crawl();
                    }
                }
            });
        });
    }

    fn draw_config(&mut self, ui: &mut Ui, time: f32) {
        let scrape = self.tab == Tab::Scrape;
        let spacing = ui.spacing().item_spacing.x;
        ui.horizontal(|ui| {
            let width = ((ui.available_width() - spacing) / 2.0).max(80.0);
            for (tab, label) in [(Tab::Scrape, "Scrape"), (Tab::Crawl, "Crawl")] {
                let active = self.tab == tab;
                let button =
                    egui::Button::new(RichText::new(label).strong().size(13.0).color(if active {
                        Color32::from_rgb(8, 14, 22)
                    } else {
                        TEXT
                    }))
                    .fill(if active { ACCENT } else { CARD })
                    .stroke(Stroke::new(1.0, if active { ACCENT } else { BORDER }))
                    .corner_radius(9)
                    .min_size(vec2(width, 32.0));
                if ui.add_enabled(!self.running, button).clicked() && !active {
                    self.switch_tab(tab);
                }
            }
        });
        ui.add_space(12.0);

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("CONFIGURATION")
                    .size(11.0)
                    .strong()
                    .color(MUTED),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Reset").clicked() {
                    self.form = Form::from_config(&Config {
                        seeds: vec![],
                        max_depth: 2,
                        max_pages: 50,
                        concurrency: 8,
                        same_domain: true,
                        include_subdomains: false,
                        proxy: None,
                        timeout: Duration::from_secs(15),
                        delay: Duration::from_millis(0),
                        retries: 2,
                        user_agent:
                            "rustrequest/0.1 (+https://github.com/linuxguytech52-sys/rustrequest)"
                                .to_string(),
                        headers: vec![],
                        respect_robots: true,
                        remove_js: true,
                        extract_text: true,
                        insecure: false,
                        fetch_only: false,
                        format: OutputFormat::Json,
                        output: None,
                        verbose: false,
                        max_links: None,
                        exclude: vec![],
                        tui: false,
                        gui: true,
                    });
                }
            });
        });
        ui.label(
            RichText::new(format!("Ctrl+Enter starts or stops a {}", self.tab.verb()))
                .size(10.5)
                .color(Color32::from_rgb(110, 121, 144)),
        );
        ui.add_space(6.0);

        egui::ScrollArea::vertical()
            .id_salt("config_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                card(ui, if scrape { "Target URLs" } else { "Target" }, |ui| {
                    ui.label(
                        RichText::new(if scrape {
                            "URLs to scrape (one per line)"
                        } else {
                            "Seed URLs (one per line)"
                        })
                        .size(12.0)
                        .color(MUTED),
                    );
                    ui.add(
                        egui::TextEdit::multiline(&mut self.form.urls)
                            .desired_rows(4)
                            .desired_width(f32::INFINITY)
                            .hint_text("https://example.com\nhttps://another.site")
                            .code_editor(),
                    );
                    ui.add_space(4.0);
                    if scrape {
                        ui.label(
                            RichText::new(
                                "Fetches each URL once and extracts data; links are not followed.",
                            )
                            .size(10.5)
                            .color(Color32::from_rgb(112, 123, 146)),
                        );
                        ui.add_space(2.0);
                        drag_usize(ui, "Concurrency", &mut self.form.concurrency, 1..=64, "");
                    } else {
                        drag_usize(ui, "Depth", &mut self.form.depth, 0..=10, "");
                        drag_usize(ui, "Max pages", &mut self.form.max_pages, 1..=100_000, "");
                        drag_usize(ui, "Concurrency", &mut self.form.concurrency, 1..=64, "");
                        ui.add_space(2.0);
                        ui.checkbox(
                            &mut self.form.same_domain,
                            RichText::new("Stay on seed domain").size(13.0),
                        );
                        ui.add_enabled_ui(self.form.same_domain, |ui| {
                            ui.checkbox(
                                &mut self.form.include_subdomains,
                                RichText::new("Include subdomains").size(13.0),
                            );
                        });
                    }
                });

                ui.add_space(10.0);

                card(ui, "Network & proxy", |ui| {
                    ui.label(RichText::new("Proxy (HTTP / HTTPS / SOCKS5)").size(12.0).color(MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.proxy)
                            .desired_width(f32::INFINITY)
                            .hint_text("http://user:pass@127.0.0.1:8080  ·  socks5://user:pass@127.0.0.1:1080"),
                    );
                    ui.add_space(4.0);
                    drag_u64(ui, "Timeout (s)", &mut self.form.timeout, 1..=300, "");
                    drag_u64(ui, "Delay (ms)", &mut self.form.delay, 0..=60_000, "");
                    drag_u32(ui, "Retries", &mut self.form.retries, 0..=10, "");
                    ui.add_space(4.0);
                    ui.label(RichText::new("User-Agent").size(12.0).color(MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.user_agent)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(4.0);
                    ui.label(RichText::new("Extra headers (Name: value)").size(12.0).color(MUTED));
                    ui.add(
                        egui::TextEdit::multiline(&mut self.form.headers)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY)
                            .hint_text("Accept-Language: en-US")
                            .code_editor(),
                    );
                });

                ui.add_space(10.0);

                card(ui, "Processing", |ui| {
                    ui.checkbox(
                        &mut self.form.respect_robots,
                        RichText::new("Respect robots.txt").size(13.0),
                    );
                    ui.label(
                        RichText::new(
                            "Follows robots.txt rules and crawl delays. On by default; uncheck to ignore.",
                        )
                        .size(10.5)
                        .color(Color32::from_rgb(112, 123, 146)),
                    );
                    ui.checkbox(
                        &mut self.form.remove_js,
                        RichText::new("Detect & remove JavaScript").size(13.0),
                    );
                    ui.label(
                        RichText::new(
                            "Strips script and style content, inline handlers and javascript: URLs from extracted text.",
                        )
                        .size(10.5)
                        .color(Color32::from_rgb(112, 123, 146)),
                    );
                    ui.checkbox(
                        &mut self.form.extract_text,
                        RichText::new("Extract readable text").size(13.0),
                    );
                    ui.checkbox(
                        &mut self.form.insecure,
                        RichText::new("Accept invalid TLS certificates").size(13.0),
                    );
                    ui.add_space(4.0);
                    drag_usize(ui, "Max links / page (0 = all)", &mut self.form.max_links, 0..=10_000, "");
                    ui.label(RichText::new("Exclude substrings").size(12.0).color(MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.exclude)
                            .desired_width(f32::INFINITY)
                            .hint_text("/logout, /cart, ?preview="),
                    );
                });

                ui.add_space(10.0);

                card(ui, "Output", |ui| {
                    egui::ComboBox::from_id_salt("output_format")
                        .selected_text(format_label(self.form.format))
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for format in [
                                OutputFormat::Json,
                                OutputFormat::Jsonl,
                                OutputFormat::Csv,
                                OutputFormat::Sitemap,
                                OutputFormat::Text,
                            ] {
                                ui.selectable_value(
                                    &mut self.form.format,
                                    format,
                                    format_label(format),
                                );
                            }
                        });
                    ui.add_space(4.0);
                    ui.label(RichText::new("Output directory").size(12.0).color(MUTED));
                    ui.horizontal(|ui| {
                        let button_w = 86.0;
                        let field_w = (ui.available_width()
                            - button_w
                            - ui.spacing().item_spacing.x)
                            .max(120.0);
                        ui.add_sized(
                            [field_w, 22.0],
                            egui::TextEdit::singleline(&mut self.form.output_dir)
                                .hint_text("current directory"),
                        );
                        if ui
                            .add_sized([button_w, 22.0], egui::Button::new("Browse…"))
                            .clicked()
                            && let Some(dir) = rfd::FileDialog::new()
                                .set_title("Choose output directory")
                                .pick_folder()
                        {
                            self.form.output_dir = dir.display().to_string();
                        }
                    });
                    ui.label(
                        RichText::new(format!("Output file: {}", self.output_preview()))
                            .size(10.5)
                            .color(Color32::from_rgb(112, 123, 146)),
                    );
                    ui.label(
                        RichText::new(
                            "The file name is chosen automatically from the output format.",
                        )
                        .size(10.0)
                        .color(Color32::from_rgb(100, 110, 132)),
                    );
                });

                if let Some(err) = &self.config_error {
                    ui.add_space(8.0);
                    egui::Frame::new()
                        .fill(RED.gamma_multiply(0.15))
                        .corner_radius(8)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.label(RichText::new(err).size(12.0).color(RED));
                        });
                }

                ui.add_space(12.0);
                let width = ui.available_width();
                if self.running {
                    if ui
                        .add_sized(
                            [width, 40.0],
                            egui::Button::new(
                                RichText::new(format!("Stop {}", self.tab.verb()))
                                    .strong()
                                    .color(Color32::WHITE),
                            )
                            .fill(RED.gamma_multiply(0.85))
                            .corner_radius(10),
                        )
                        .clicked()
                    {
                        self.stop();
                    }
                } else if ui
                    .add_sized(
                        [width, 40.0],
                        egui::Button::new(
                            RichText::new(format!("Start {}", self.tab.verb()))
                                .strong()
                                .color(Color32::from_rgb(8, 14, 22)),
                        )
                        .fill(ACCENT)
                        .corner_radius(10),
                    )
                    .clicked()
                {
                    self.start_crawl();
                }

                animated_rule(ui, time);
            });
    }

    fn draw_dashboard(&self, ui: &mut Ui, time: f32) {
        let s = &self.stats;
        let cards: [(&str, String, String, Color32); 6] = [
            (
                "Pages",
                s.done.to_string(),
                format!("of {}", self.planned_pages()),
                ACCENT,
            ),
            (
                "Queued",
                s.queued.to_string(),
                "in frontier".to_string(),
                ORANGE,
            ),
            (
                "Failed",
                s.failed.to_string(),
                if s.failed == 0 {
                    "all good"
                } else {
                    "check feed"
                }
                .to_string(),
                RED,
            ),
            (
                "Downloaded",
                util::human_bytes(s.bytes).replace(' ', ""),
                "total bytes".to_string(),
                PURPLE,
            ),
            (
                "Links",
                s.links.to_string(),
                "discovered".to_string(),
                GREEN,
            ),
            (
                "JS pages",
                s.js_pages.to_string(),
                format!("{} scripts", s.scripts),
                YELLOW,
            ),
        ];

        ui.columns(6, |cols| {
            for (i, (label, value, sub, color)) in cards.iter().enumerate() {
                stat_card(&mut cols[i], label, value, sub, *color);
            }
        });

        ui.add_space(10.0);
        progress_card(ui, self, time);
        ui.add_space(10.0);

        let height = ui.available_height().max(220.0);
        let total = ui.available_width();
        let gap = 12.0;
        let right_w = (total * 0.38).clamp(300.0, 500.0);
        let feed_w = (total - right_w - gap).max(260.0);

        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(vec2(feed_w, height), Layout::top_down(Align::Min), |ui| {
                feed_card(ui, &self.feed, time, self.running, self.tab, height);
            });
            ui.add_space(gap);
            ui.allocate_ui_with_layout(vec2(right_w, height), Layout::top_down(Align::Min), |ui| {
                let elapsed = self.finished_secs.unwrap_or_else(|| self.elapsed_secs());
                insights_panel(
                    ui,
                    &self.stats,
                    &self.throughput,
                    elapsed,
                    time,
                    self.running,
                );
            });
        });
    }
}

fn card(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(12)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(12.5).strong().color(TEXT));
            ui.add_space(6.0);
            add(ui);
        });
}

fn badge(ui: &mut Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .corner_radius(6)
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(10.5)
                    .strong()
                    .monospace()
                    .color(color),
            );
        });
}

fn drag_usize(
    ui: &mut Ui,
    label: &str,
    value: &mut usize,
    range: std::ops::RangeInclusive<usize>,
    suffix: &str,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.5).color(MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(
                egui::DragValue::new(value)
                    .range(range)
                    .speed(0.25)
                    .suffix(suffix),
            );
        });
    });
}

fn drag_u64(
    ui: &mut Ui,
    label: &str,
    value: &mut u64,
    range: std::ops::RangeInclusive<u64>,
    suffix: &str,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.5).color(MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(
                egui::DragValue::new(value)
                    .range(range)
                    .speed(0.25)
                    .suffix(suffix),
            );
        });
    });
}

fn drag_u32(
    ui: &mut Ui,
    label: &str,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    suffix: &str,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(12.5).color(MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(
                egui::DragValue::new(value)
                    .range(range)
                    .speed(0.25)
                    .suffix(suffix),
            );
        });
    });
}

fn animated_rule(ui: &mut Ui, time: f32) {
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 2.0), Sense::hover());
    let steps = 40;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let hue = (time * 45.0 + t0 * 120.0) % 360.0;
        let x0 = rect.left() + rect.width() * t0;
        let x1 = rect.left() + rect.width() * t1;
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom())),
            0,
            hue_color(hue, 0.6, 0.85),
        );
    }
}

fn stat_card(ui: &mut Ui, label: &str, value: &str, sub: &str, color: Color32) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(12)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_min_height(62.0);
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).size(10.5).strong().color(MUTED));
            });
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(RichText::new(value).size(22.0).strong().color(color))
                    .wrap_mode(egui::TextWrapMode::Extend),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(sub)
                        .size(10.0)
                        .color(Color32::from_rgb(115, 126, 149)),
                )
                .wrap_mode(egui::TextWrapMode::Extend),
            );
        });
}

fn progress_card(ui: &mut Ui, app: &RustRequestApp, time: f32) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(12)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let title = if app.tab == Tab::Scrape {
                    "Scrape progress"
                } else {
                    "Crawl progress"
                };
                ui.label(RichText::new(title).size(13.0).strong().color(TEXT));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let pct = app.target_progress * 100.0;
                    ui.label(
                        RichText::new(format!("{pct:.0}%"))
                            .size(13.0)
                            .strong()
                            .monospace()
                            .color(ACCENT),
                    );
                    ui.label(
                        RichText::new(format!("{} / {}", app.stats.done, app.planned_pages()))
                            .size(12.0)
                            .monospace()
                            .color(MUTED),
                    );
                });
            });
            ui.add_space(8.0);

            let desired = vec2(ui.available_width(), 26.0);
            let (response, painter) = ui.allocate_painter(desired, Sense::hover());
            let rect = response.rect;
            painter.rect_filled(rect, 13, Color32::from_rgb(14, 17, 25));

            let ratio = app.progress.clamp(0.0, 1.0);
            if ratio > 0.001 {
                let fill = Rect::from_min_size(
                    rect.min,
                    vec2((rect.width() * ratio).max(6.0), rect.height()),
                );
                let hue = (time * 55.0) % 360.0;
                painter.rect_filled(fill, 13, hue_color(hue, 0.65, 0.95));
                painter.rect_filled(
                    Rect::from_min_size(
                        rect.min + vec2(0.0, 2.0),
                        vec2(fill.width().max(6.0), rect.height() * 0.36),
                    ),
                    10,
                    Color32::from_white_alpha(26),
                );
                if app.running {
                    let phase = (time * 0.55).fract();
                    let cx = rect.left() + phase * rect.width();
                    let shimmer = Rect::from_min_max(
                        pos2(cx - 26.0, rect.top()),
                        pos2(cx + 26.0, rect.bottom()),
                    );
                    painter.rect_filled(shimmer, 13, Color32::from_white_alpha(20));
                }
            }

            let label = format!(
                "{} pages · {} queued · {} failed",
                app.stats.done, app.stats.queued, app.stats.failed
            );
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(12.0),
                Color32::WHITE,
            );

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let elapsed = app.finished_secs.unwrap_or_else(|| app.elapsed_secs());
                ui.label(
                    RichText::new(format!("elapsed {}", util::elapsed_hms(elapsed as u64)))
                        .size(11.0)
                        .color(MUTED),
                );
                if app.running && app.target_progress > 0.03 {
                    let eta =
                        elapsed * (1.0 - app.target_progress as f64) / app.target_progress as f64;
                    ui.label(
                        RichText::new(format!("· eta {}", util::elapsed_hms(eta as u64)))
                            .size(11.0)
                            .color(MUTED),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} downloaded", util::human_bytes(app.stats.bytes)))
                            .size(11.0)
                            .color(MUTED),
                    );
                });
            });
        });
}

fn feed_card(
    ui: &mut Ui,
    feed: &VecDeque<FeedItem>,
    time: f32,
    running: bool,
    tab: Tab,
    height: f32,
) {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(12)
        .stroke(Stroke::new(1.0, BORDER))
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_min_height((height - 24.0).max(160.0));
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let title = if tab == Tab::Scrape {
                    "Scrape Feed"
                } else {
                    "Crawl Feed"
                };
                ui.label(RichText::new(title).size(13.0).strong().color(TEXT));
                ui.label(
                    RichText::new(format!("{} entries", feed.len()))
                        .size(10.5)
                        .color(MUTED),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if running {
                        pulse_dot(ui, time, ACCENT);
                        ui.label(RichText::new("live").size(10.5).color(ACCENT));
                    }
                });
            });
            ui.add_space(6.0);
            ui.separator();

            let list_height = (height - 84.0).max(120.0);
            egui::ScrollArea::vertical()
                .id_salt("feed_scroll")
                .max_height(list_height)
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if feed.is_empty() {
                        ui.add_space(24.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new("Waiting for the first response…")
                                    .size(13.0)
                                    .color(MUTED),
                            );
                        });
                    }
                    let now = Instant::now();
                    for item in feed {
                        match item {
                            FeedItem::Page(entry) => feed_page_row(ui, entry, now),
                            FeedItem::Note(entry) => feed_note_row(ui, entry, now),
                        }
                    }
                });
        });
}

fn feed_page_row(ui: &mut Ui, entry: &FeedEntry, now: Instant) {
    let age = now.saturating_duration_since(entry.at).as_secs_f32();
    let glow = (1.0 - age / 1.6).clamp(0.0, 1.0);
    let fill = if glow > 0.0 {
        Color32::from_rgba_unmultiplied(86, 200, 255, (glow * 34.0) as u8)
    } else {
        Color32::TRANSPARENT
    };

    egui::Frame::new()
        .fill(fill)
        .corner_radius(7)
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                badge(
                    ui,
                    &entry.log.status.to_string(),
                    status_color(entry.log.status),
                );
                ui.label(
                    RichText::new(format!("d{}", entry.log.depth))
                        .size(10.0)
                        .monospace()
                        .color(MUTED),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(util::short_url(&entry.log.url, 96))
                            .size(12.0)
                            .monospace()
                            .color(TEXT),
                    )
                    .truncate(),
                );
            });
            ui.horizontal(|ui| {
                if let Some(title) = entry.log.title.as_deref().filter(|t| !t.is_empty()) {
                    let width = (ui.available_width() - 150.0).max(60.0);
                    ui.add_sized(
                        [width, 15.0],
                        egui::Label::new(
                            RichText::new(title)
                                .size(11.0)
                                .italics()
                                .color(Color32::from_rgb(148, 159, 182)),
                        )
                        .truncate(),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} · {}ms",
                            util::human_bytes(entry.log.size),
                            entry.log.elapsed_ms
                        ))
                        .size(10.0)
                        .color(MUTED),
                    );
                    if entry.log.js.detected {
                        ui.label(
                            RichText::new(format!("JS {}", entry.log.js.total_scripts))
                                .size(10.0)
                                .color(YELLOW),
                        );
                    }
                });
            });
        });
    ui.add_space(2.0);
}

fn feed_note_row(ui: &mut Ui, entry: &NoteEntry, now: Instant) {
    let age = now.saturating_duration_since(entry.at).as_secs_f32();
    let fade = (1.0 - age / 2.0).clamp(0.0, 1.0);
    let (color, tag) = match entry.level {
        LogLevel::Info => (MUTED, "info"),
        LogLevel::Warn => (YELLOW, "warn"),
        LogLevel::Error => (RED, "error"),
    };
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            (fade * 26.0) as u8,
        ))
        .corner_radius(7)
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                badge(ui, tag, color);
                ui.add(
                    egui::Label::new(
                        RichText::new(&entry.message)
                            .size(11.0)
                            .color(color.gamma_multiply(0.95)),
                    )
                    .truncate(),
                );
            });
        });
    ui.add_space(2.0);
}

fn insights_panel(
    ui: &mut Ui,
    stats: &RunStats,
    throughput: &[f32],
    elapsed: f64,
    time: f32,
    running: bool,
) {
    egui::ScrollArea::vertical()
        .id_salt("insights_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            card(ui, "Throughput · pages/sec", |ui| {
                let current = if running {
                    throughput.last().copied().unwrap_or(0.0)
                } else if elapsed > 0.0 {
                    stats.done as f32 / elapsed as f32
                } else {
                    0.0
                };
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{current:.0}"))
                            .size(26.0)
                            .strong()
                            .color(ACCENT),
                    );
                    ui.label(RichText::new("pages/s").size(11.0).color(MUTED));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if running {
                            pulse_dot(ui, time, ACCENT);
                        }
                    });
                });
                sparkline(ui, throughput, time, 62.0);
            });

            ui.add_space(10.0);

            card(ui, "HTTP Status", |ui| {
                if stats.status_counts.is_empty() {
                    ui.label(RichText::new("no responses yet").size(11.5).color(MUTED));
                } else {
                    let mut entries: Vec<(&u16, &usize)> = stats.status_counts.iter().collect();
                    entries.sort_by(|a, b| b.1.cmp(a.1));
                    let max = entries.first().map(|(_, c)| **c).unwrap_or(1).max(1);
                    for (status, count) in entries.into_iter().take(6) {
                        let color = status_color(*status);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{status:>3}"))
                                    .size(11.0)
                                    .monospace()
                                    .strong()
                                    .color(color),
                            );
                            let width = (ui.available_width() - 46.0).max(30.0);
                            ui.add_sized(
                                [width, 10.0],
                                egui::ProgressBar::new(*count as f32 / max as f32)
                                    .fill(color)
                                    .corner_radius(4),
                            );
                            ui.label(RichText::new(count.to_string()).size(10.5).color(MUTED));
                        });
                    }
                }
            });

            ui.add_space(10.0);

            card(ui, "JavaScript", |ui| {
                metric_row(ui, "pages with JS", &stats.js_pages.to_string(), YELLOW);
                metric_row(
                    ui,
                    "scripts",
                    &format!("{} ({} ext)", stats.scripts, stats.external_scripts),
                    PURPLE,
                );
                metric_row(ui, "event handlers", &stats.handlers.to_string(), GREEN);
                metric_row(ui, "javascript: URLs", &stats.js_urls.to_string(), ACCENT);
            });

            ui.add_space(10.0);

            card(ui, "Frameworks", |ui| {
                if stats.frameworks.is_empty() {
                    ui.label(RichText::new("none detected").size(11.5).color(MUTED));
                } else {
                    let mut frameworks: Vec<String> = stats
                        .frameworks
                        .iter()
                        .map(|(name, count)| format!("{name} ×{count}"))
                        .collect();
                    frameworks.sort();
                    ui.horizontal_wrapped(|ui| {
                        for framework in frameworks {
                            egui::Frame::new()
                                .fill(CARD_SOFT)
                                .corner_radius(6)
                                .stroke(Stroke::new(1.0, BORDER))
                                .inner_margin(Margin::symmetric(6, 2))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(framework)
                                            .size(10.5)
                                            .color(Color32::from_rgb(170, 200, 250)),
                                    );
                                });
                        }
                    });
                }
            });
        });
}

fn metric_row(ui: &mut Ui, label: &str, value: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(11.5).color(MUTED));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(value)
                    .size(11.5)
                    .strong()
                    .monospace()
                    .color(color),
            );
        });
    });
}

fn sparkline(ui: &mut Ui, data: &[f32], time: f32, height: f32) {
    let desired = vec2(ui.available_width(), height);
    let (response, painter) = ui.allocate_painter(desired, Sense::hover());
    let rect = response.rect;
    painter.rect_filled(rect, 6, Color32::from_rgb(14, 17, 25));

    let max = data.iter().copied().fold(1.0f32, f32::max);
    let slot = (rect.width() / 60.0).max(2.0);
    let bar_w = (slot - 1.5).max(1.5);

    if data.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "sampling…",
            FontId::proportional(11.0),
            MUTED,
        );
        return;
    }

    for (i, value) in data.iter().enumerate() {
        let h = (value / max) * (rect.height() - 10.0);
        let x0 = rect.right() - (data.len() - i) as f32 * slot;
        let bar = Rect::from_min_max(
            pos2(x0 + 1.0, rect.bottom() - h - 3.0),
            pos2(x0 + 1.0 + bar_w, rect.bottom() - 3.0),
        );
        let hue = (time * 40.0 + i as f32 * 6.0) % 360.0;
        painter.rect_filled(bar, 2, hue_color(hue, 0.7, 0.95));
    }

    painter.line_segment(
        [
            pos2(rect.left(), rect.bottom() - 3.0),
            pos2(rect.right(), rect.bottom() - 3.0),
        ],
        Stroke::new(1.0, BORDER),
    );
}

fn pulse_dot(ui: &mut Ui, time: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
    let radius = 3.5 + (time * 5.0).sin().abs() * 1.8;
    ui.painter()
        .circle_filled(rect.center(), radius, color.gamma_multiply(0.9));
}

fn draw_status_bar(ui: &mut Ui, app: &RustRequestApp, time: f32) {
    ui.horizontal(|ui| {
        let (state, color) = if app.running {
            (
                if app.tab == Tab::Scrape {
                    "scraping"
                } else {
                    "crawling"
                },
                ACCENT,
            )
        } else if app.error_msg.is_some() {
            ("error", RED)
        } else if app.output_msg.is_some() {
            ("complete", GREEN)
        } else {
            ("idle", MUTED)
        };
        pulse_dot(ui, time, color);
        ui.label(RichText::new(state).size(11.5).strong().color(color));
        ui.separator();
        ui.label(
            RichText::new(format!(
                "{} pages · {} queued · {} failed",
                app.stats.done, app.stats.queued, app.stats.failed
            ))
            .size(11.0)
            .color(MUTED),
        );
        ui.separator();
        ui.label(
            RichText::new(format!(
                "{} downloaded · {} links",
                util::human_bytes(app.stats.bytes),
                app.stats.links
            ))
            .size(11.0)
            .color(MUTED),
        );
        ui.separator();
        let elapsed = app.finished_secs.unwrap_or_else(|| app.elapsed_secs());
        ui.label(
            RichText::new(format!("elapsed {}", util::elapsed_hms(elapsed as u64)))
                .size(11.0)
                .color(MUTED),
        );

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some((message, at)) = &app.toast {
                if at.elapsed() < Duration::from_secs(4) {
                    ui.label(
                        RichText::new(message)
                            .size(11.0)
                            .color(Color32::from_rgb(170, 200, 250)),
                    );
                } else if let Some(path) = &app.output_msg {
                    ui.label(
                        RichText::new(format!("results saved → {path}"))
                            .size(11.0)
                            .color(GREEN),
                    );
                }
            } else if let Some(path) = &app.output_msg {
                ui.label(
                    RichText::new(format!("results saved → {path}"))
                        .size(11.0)
                        .color(GREEN),
                );
            } else if let Some(err) = &app.error_msg {
                ui.label(RichText::new(err).size(11.0).color(RED));
            } else {
                ui.label(
                    RichText::new("Ctrl+Enter start / stop")
                        .size(10.5)
                        .color(Color32::from_rgb(105, 116, 138)),
                );
            }
        });
    });
}

fn configure_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(14, 17, 25);
    visuals.faint_bg_color = CARD;
    visuals.text_edit_bg_color = Some(Color32::from_rgb(14, 17, 25));
    visuals.hyperlink_color = ACCENT;
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.menu_corner_radius = egui::CornerRadius::same(10);
    visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);

    visuals.widgets.noninteractive.bg_fill = PANEL;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(8);

    visuals.widgets.inactive.bg_fill = CARD;
    visuals.widgets.inactive.weak_bg_fill = CARD;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(34, 40, 58);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(34, 40, 58);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);

    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.weak_bg_fill = ACCENT;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5, Color32::BLACK);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);

    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 6.0);
        style.spacing.button_padding = vec2(10.0, 6.0);
        style.spacing.menu_margin = Margin::same(8);
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(11.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(13.5));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, FontId::proportional(20.0));
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, FontId::monospace(12.5));
    });
}

pub fn run(cfg: Config) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("rustrequest")
            .with_inner_size([1240.0, 820.0])
            .with_min_inner_size([940.0, 600.0]),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "rustrequest",
        options,
        Box::new(move |cc| {
            configure_style(&cc.egui_ctx);
            Ok(Box::new(RustRequestApp::new(cfg)))
        }),
    )
    .map_err(|err| anyhow::anyhow!("{err}"))
}
