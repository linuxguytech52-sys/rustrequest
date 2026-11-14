use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Gauge, Paragraph, Sparkline, Wrap};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::config::Config;
use crate::crawler::{self, CrawlEvent, LogLevel, PageLog};
use crate::export;
use crate::http;
use crate::util;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunStatus {
    Running,
    Done,
    Failed,
}

enum FeedItem {
    Page(PageLog),
    Note(LogLevel, String),
}

struct App {
    started: Instant,
    last_second: Instant,
    seeds: Vec<String>,
    max_pages: usize,
    max_depth: usize,
    proxy: Option<String>,
    respect_robots: bool,
    user_agent: String,
    done: usize,
    queued: usize,
    failed: usize,
    bytes: u64,
    links: usize,
    js_pages: usize,
    scripts: usize,
    external_scripts: usize,
    handlers: usize,
    js_urls: usize,
    frameworks: BTreeMap<String, usize>,
    status_counts: BTreeMap<u16, usize>,
    feed: VecDeque<FeedItem>,
    progress: f64,
    target_progress: f64,
    throughput: Vec<u64>,
    sec_pages: u64,
    status: RunStatus,
    output: Option<String>,
    error: Option<String>,
    should_quit: bool,
}

impl App {
    fn new(cfg: &Config) -> Self {
        let now = Instant::now();
        App {
            started: now,
            last_second: now,
            seeds: cfg.seeds.iter().map(|u| u.to_string()).collect(),
            max_pages: cfg.max_pages,
            max_depth: cfg.max_depth,
            proxy: cfg.proxy.clone(),
            respect_robots: cfg.respect_robots,
            user_agent: cfg.user_agent.clone(),
            done: 0,
            queued: 0,
            failed: 0,
            bytes: 0,
            links: 0,
            js_pages: 0,
            scripts: 0,
            external_scripts: 0,
            handlers: 0,
            js_urls: 0,
            frameworks: BTreeMap::new(),
            status_counts: BTreeMap::new(),
            feed: VecDeque::new(),
            progress: 0.0,
            target_progress: 0.0,
            throughput: Vec::new(),
            sec_pages: 0,
            status: RunStatus::Running,
            output: None,
            error: None,
            should_quit: false,
        }
    }

    fn elapsed(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn handle(&mut self, event: CrawlEvent) {
        match event {
            CrawlEvent::Started {
                seeds,
                max_pages,
                max_depth,
                proxy,
                respect_robots,
                user_agent,
            } => {
                self.seeds = seeds;
                self.max_pages = max_pages;
                self.max_depth = max_depth;
                self.proxy = proxy;
                self.respect_robots = respect_robots;
                self.user_agent = user_agent;
            }
            CrawlEvent::Progress {
                done,
                queued,
                total,
            } => {
                self.done = done;
                self.queued = queued;
                if total > 0 {
                    self.max_pages = total;
                    self.target_progress = (done as f64 / total as f64).clamp(0.0, 1.0);
                }
            }
            CrawlEvent::Page(log) => {
                self.bytes += log.size;
                self.links += log.links;
                self.sec_pages += 1;
                *self.status_counts.entry(log.status).or_insert(0) += 1;
                if log.js.detected {
                    self.js_pages += 1;
                }
                self.scripts += log.js.total_scripts;
                self.external_scripts += log.js.external_scripts;
                self.handlers += log.js.event_handlers;
                self.js_urls += log.js.javascript_urls;
                for framework in &log.js.frameworks {
                    *self.frameworks.entry(framework.clone()).or_insert(0) += 1;
                }
                self.push_feed(FeedItem::Page(log));
            }
            CrawlEvent::Log(level, message) => {
                if level == LogLevel::Error {
                    self.failed += 1;
                }
                self.push_feed(FeedItem::Note(level, message));
            }
            CrawlEvent::Finished {
                pages,
                failed,
                elapsed_secs,
            } => {
                self.failed = self.failed.max(failed);
                self.push_feed(FeedItem::Note(
                    LogLevel::Info,
                    format!("crawl finished · {pages} pages in {:.1}s", elapsed_secs),
                ));
            }
            CrawlEvent::Done { output } => match output {
                Ok(path) => {
                    self.output = Some(path);
                    self.status = RunStatus::Done;
                    if self.done > 0 {
                        self.max_pages = self.done;
                    }
                    self.target_progress = 1.0;
                }
                Err(err) => {
                    self.error = Some(err);
                    self.status = RunStatus::Failed;
                }
            },
        }
    }

    fn push_feed(&mut self, item: FeedItem) {
        self.feed.push_back(item);
        while self.feed.len() > 500 {
            self.feed.pop_front();
        }
    }

    fn tick(&mut self) {
        self.progress += (self.target_progress - self.progress) * 0.18;
        if (self.target_progress - self.progress).abs() < 0.0005 {
            self.progress = self.target_progress;
        }
        if self.last_second.elapsed() >= Duration::from_secs(1) {
            self.throughput.push(self.sec_pages);
            if self.throughput.len() > 60 {
                self.throughput.remove(0);
            }
            self.sec_pages = 0;
            self.last_second = Instant::now();
        }
    }
}

fn rainbow_spans(text: &str, t: f64) -> Vec<Span<'static>> {
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let hue = (t * 90.0 + i as f64 * 16.0) % 360.0;
            let color = util::hue_color(hue, 0.72, 1.0);
            Span::styled(
                ch.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        })
        .collect()
}

fn animated_border(t: f64) -> Style {
    Style::default().fg(util::hue_color((t * 45.0) % 360.0, 0.55, 0.85))
}

fn block(title: &str, t: f64) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(animated_border(t))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
}

fn status_glyph(status: u16) -> (char, Color) {
    if (200..300).contains(&status) {
        ('✓', Color::Rgb(80, 220, 140))
    } else if (300..400).contains(&status) {
        ('→', Color::Rgb(90, 200, 255))
    } else if (400..500).contains(&status) {
        ('!', Color::Rgb(240, 200, 80))
    } else {
        ('✗', Color::Rgb(240, 100, 100))
    }
}

fn level_glyph(level: LogLevel) -> (char, Color) {
    match level {
        LogLevel::Info => ('·', Color::Rgb(150, 160, 180)),
        LogLevel::Warn => ('!', Color::Rgb(240, 200, 80)),
        LogLevel::Error => ('✗', Color::Rgb(240, 100, 100)),
    }
}

fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let outer = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);

    draw_header(frame, outer[0], app);

    let body = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(outer[1]);
    draw_feed(frame, body[0], app);

    let right = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(9),
        Constraint::Length(4),
        Constraint::Min(0),
    ])
    .split(body[1]);

    draw_progress(frame, right[0], app);
    draw_stats(frame, right[1], app);
    draw_throughput(frame, right[2], app);

    let bottom = Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(right[3]);
    draw_js(frame, bottom[0], app);
    draw_statuses(frame, bottom[1], app);

    draw_footer(frame, outer[2], app);
}

fn draw_header(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let t = app.elapsed();
    let border = Style::default().fg(util::hue_color((t * 45.0) % 360.0, 0.55, 0.85));
    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(border);

    let mut title = vec![Span::styled(
        "◆ ",
        Style::default().fg(util::hue_color((t * 90.0) % 360.0, 0.8, 1.0)),
    )];
    title.extend(rainbow_spans("rustrequest", t));
    title.push(Span::styled(
        "  v0.1  ",
        Style::default().fg(Color::Rgb(120, 130, 150)),
    ));
    title.push(Span::styled(
        "Rust web scraper & crawler",
        Style::default().fg(Color::Rgb(170, 180, 200)),
    ));

    let proxy = app.proxy.as_deref().unwrap_or("none");
    let robots = if app.respect_robots { "on" } else { "off" };
    let seed = app.seeds.first().cloned().unwrap_or_default();
    let info = format!(
        "seed {}   ·   proxy {}   ·   robots {}   ·   depth {}   ·   ua {}",
        util::truncate_chars(&util::short_url(&seed, 60), 44),
        util::truncate_chars(proxy, 24),
        robots,
        app.max_depth,
        util::truncate_chars(&app.user_agent, 26),
    );

    let paragraph = Paragraph::new(Text::from(vec![
        Line::from(title),
        Line::from(Span::styled(
            info,
            Style::default().fg(Color::Rgb(120, 130, 150)),
        )),
    ]))
    .block(block);
    frame.render_widget(paragraph, area);
}

fn draw_feed(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let t = app.elapsed();
    let inner_height = area.height.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();

    for item in app
        .feed
        .iter()
        .rev()
        .take(inner_height)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        match item {
            FeedItem::Page(log) => {
                let (glyph, color) = status_glyph(log.status);
                let title = log.title.as_deref().unwrap_or("");
                let mut spans = vec![
                    Span::styled(format!(" {glyph} "), Style::default().fg(color)),
                    Span::styled(
                        format!("{:>3} ", log.status),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("d{} ", log.depth),
                        Style::default().fg(Color::Rgb(100, 110, 130)),
                    ),
                    Span::styled(
                        util::short_url(&log.url, 44),
                        Style::default().fg(Color::Rgb(215, 222, 235)),
                    ),
                ];
                if !title.is_empty() {
                    spans.push(Span::styled(
                        "  ",
                        Style::default().fg(Color::Rgb(80, 85, 100)),
                    ));
                    spans.push(Span::styled(
                        util::truncate_chars(title, 30),
                        Style::default()
                            .fg(Color::Rgb(130, 140, 160))
                            .add_modifier(Modifier::ITALIC),
                    ));
                }
                lines.push(Line::from(spans));
            }
            FeedItem::Note(level, message) => {
                let (glyph, color) = level_glyph(*level);
                lines.push(Line::from(vec![
                    Span::styled(format!(" {glyph} "), Style::default().fg(color)),
                    Span::styled(
                        util::truncate_chars(message, 90),
                        Style::default().fg(Color::Rgb(150, 158, 175)),
                    ),
                ]));
            }
        }
    }

    if lines.is_empty() {
        let spin = SPINNER[(app.elapsed() * 10.0) as usize % SPINNER.len()];
        lines.push(Line::from(Span::styled(
            format!(" {spin} waiting for first response…"),
            Style::default().fg(Color::Rgb(120, 130, 150)),
        )));
    }

    let paragraph = Paragraph::new(Text::from(lines)).block(block("Crawl Feed", t));
    frame.render_widget(paragraph, area);
}

fn draw_progress(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let t = app.elapsed();
    let hue = (t * 70.0) % 360.0;
    let color = util::hue_color(hue, 0.7, 1.0);
    let target = app.target_progress;
    let label = format!(
        "{}/{}  ·  {:.0}%  ·  {} queued",
        app.done,
        app.max_pages,
        target * 100.0,
        app.queued
    );
    let gauge = Gauge::default()
        .block(block("Progress", t))
        .gauge_style(Style::default().fg(color).bg(Color::Rgb(30, 33, 45)))
        .ratio(app.progress.clamp(0.0, 1.0))
        .label(Span::styled(
            label,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(gauge, area);
}

fn draw_stats(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let t = app.elapsed();
    let spin = if app.status == RunStatus::Running {
        SPINNER[(t * 12.0) as usize % SPINNER.len()]
    } else {
        "●"
    };
    let (state, state_color) = match app.status {
        RunStatus::Running => ("crawling", Color::Rgb(90, 200, 255)),
        RunStatus::Done => ("complete", Color::Rgb(80, 220, 140)),
        RunStatus::Failed => ("error", Color::Rgb(240, 100, 100)),
    };

    let rows: Vec<(&str, String, Color)> = vec![
        ("State", format!("{spin} {state}"), state_color),
        (
            "Pages",
            format!("{} / {}", app.done, app.max_pages),
            Color::Rgb(215, 222, 235),
        ),
        ("Queued", app.queued.to_string(), Color::Rgb(90, 200, 255)),
        (
            "Failed",
            app.failed.to_string(),
            if app.failed > 0 {
                Color::Rgb(240, 120, 120)
            } else {
                Color::Rgb(120, 130, 150)
            },
        ),
        (
            "Downloaded",
            util::human_bytes(app.bytes),
            Color::Rgb(200, 180, 255),
        ),
        (
            "Links found",
            app.links.to_string(),
            Color::Rgb(90, 220, 210),
        ),
        (
            "Elapsed",
            util::elapsed_hms(app.started.elapsed().as_secs()),
            Color::Rgb(150, 160, 180),
        ),
    ];

    let mut lines = Vec::new();
    for (label, value, color) in rows {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {label:<13}"),
                Style::default().fg(Color::Rgb(110, 120, 140)),
            ),
            Span::styled(
                value,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    let paragraph = Paragraph::new(Text::from(lines)).block(block("Session", t));
    frame.render_widget(paragraph, area);
}

fn draw_throughput(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let t = app.elapsed();
    let max = app.throughput.iter().copied().max().unwrap_or(1).max(1);
    let color = util::hue_color((t * 55.0) % 360.0, 0.65, 1.0);
    let sparkline = Sparkline::default()
        .block(block("Throughput · pages/sec", t))
        .data(app.throughput.clone())
        .max(max)
        .style(Style::default().fg(color));
    frame.render_widget(sparkline, area);
}

fn draw_js(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let t = app.elapsed();
    let detected_color = if app.js_pages > 0 {
        Color::Rgb(240, 200, 80)
    } else {
        Color::Rgb(120, 130, 150)
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                " pages w/ JS  ",
                Style::default().fg(Color::Rgb(110, 120, 140)),
            ),
            Span::styled(
                format!("{}", app.js_pages),
                Style::default()
                    .fg(detected_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                " scripts      ",
                Style::default().fg(Color::Rgb(110, 120, 140)),
            ),
            Span::styled(
                format!("{} ({} ext)", app.scripts, app.external_scripts),
                Style::default().fg(Color::Rgb(200, 180, 255)),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                " handlers     ",
                Style::default().fg(Color::Rgb(110, 120, 140)),
            ),
            Span::styled(
                format!("{}  ·  js: {}", app.handlers, app.js_urls),
                Style::default().fg(Color::Rgb(90, 220, 210)),
            ),
        ]),
    ];

    if !app.frameworks.is_empty() {
        let mut frameworks: Vec<String> = app
            .frameworks
            .iter()
            .map(|(name, count)| format!("{name}×{count}"))
            .collect();
        frameworks.sort();
        lines.push(Line::from(vec![
            Span::styled(
                " frameworks   ",
                Style::default().fg(Color::Rgb(110, 120, 140)),
            ),
            Span::styled(
                frameworks.join(", "),
                Style::default().fg(Color::Rgb(150, 200, 250)),
            ),
        ]));
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block("JavaScript", t))
        .wrap(Wrap { trim: true });
    frame.render_widget(paragraph, area);
}

fn draw_statuses(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let t = app.elapsed();
    let mut entries: Vec<(&u16, &usize)> = app.status_counts.iter().collect();
    entries.sort_by(|a, b| b.1.cmp(a.1));

    let mut lines = Vec::new();
    for (status, count) in entries.into_iter().take(6) {
        let (glyph, color) = status_glyph(*status);
        let bar_len = (*count).min(10);
        lines.push(Line::from(vec![
            Span::styled(format!(" {glyph} "), Style::default().fg(color)),
            Span::styled(
                format!("{:>3} ", status),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled("█".repeat(bar_len), Style::default().fg(color)),
            Span::styled(
                format!(" {count}"),
                Style::default().fg(Color::Rgb(170, 180, 200)),
            ),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no responses yet",
            Style::default().fg(Color::Rgb(120, 130, 150)),
        )));
    }

    let paragraph = Paragraph::new(Text::from(lines)).block(block("HTTP Status", t));
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let t = app.elapsed();
    let border = Style::default().fg(util::hue_color((t * 45.0) % 360.0, 0.55, 0.85));
    let block = Block::default().borders(Borders::TOP).border_style(border);

    let spin = SPINNER[(t * 12.0) as usize % SPINNER.len()];
    let (state_text, state_color, hint) = match app.status {
        RunStatus::Running => (
            format!("{spin} crawling"),
            Color::Rgb(90, 200, 255),
            "q quit · ctrl-c abort",
        ),
        RunStatus::Done => ("✓ complete".to_string(), Color::Rgb(80, 220, 140), "q quit"),
        RunStatus::Failed => (
            "✗ export failed".to_string(),
            Color::Rgb(240, 100, 100),
            "q quit",
        ),
    };

    let detail = if let Some(path) = &app.output {
        format!("results → {path}")
    } else if let Some(err) = &app.error {
        format!("error: {err}")
    } else {
        format!(
            "{} pages · {} failed · {} downloaded",
            app.done,
            app.failed,
            util::human_bytes(app.bytes)
        )
    };

    let paragraph = Paragraph::new(Text::from(vec![
        Line::from(vec![
            Span::styled(format!(" {state_text}"), Style::default().fg(state_color)),
            Span::styled(
                format!("   {hint}"),
                Style::default().fg(Color::Rgb(110, 120, 140)),
            ),
        ]),
        Line::from(Span::styled(
            format!(" {detail}"),
            Style::default().fg(Color::Rgb(150, 160, 180)),
        )),
    ]))
    .block(block);
    frame.render_widget(paragraph, area);
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    rx: &mut UnboundedReceiver<CrawlEvent>,
) -> io::Result<()> {
    loop {
        while let Ok(event) = rx.try_recv() {
            app.handle(event);
        }
        app.tick();
        terminal.draw(|frame| draw(frame, app))?;

        if event::poll(Duration::from_millis(33))? {
            while event::poll(Duration::ZERO)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.should_quit = true;
                        }
                        _ => {}
                    },
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

pub fn run(rt: &tokio::runtime::Runtime, cfg: Config) -> anyhow::Result<()> {
    let client = http::build_client(&cfg)?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let export_cfg = cfg.clone();
    let handle = rt.spawn(async move {
        let result = crawler::crawl(export_cfg.clone(), client, tx.clone()).await;
        let output = match export::write(&export_cfg, &result) {
            Ok(path) => Ok(path.display().to_string()),
            Err(err) => Err(format!("{err:#}")),
        };
        let _ = tx.send(CrawlEvent::Done { output });
    });

    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(err) => {
            handle.abort();
            return Err(err.into());
        }
    };

    let mut app = App::new(&cfg);
    let result = event_loop(&mut terminal, &mut app, &mut rx);
    ratatui::restore();

    if app.status != RunStatus::Done {
        handle.abort();
    }

    result?;

    if let Some(path) = &app.output {
        println!("Results written to {path}");
    } else if let Some(err) = &app.error {
        eprintln!("Export failed: {err}");
    }

    Ok(())
}
