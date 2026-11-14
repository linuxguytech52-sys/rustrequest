use ratatui::style::Color;

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn normalize_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

pub fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    (
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    )
}

pub fn hue_color(hue: f64, sat: f64, val: f64) -> Color {
    let (r, g, b) = hsv_to_rgb(hue, sat, val);
    Color::Rgb(r, g, b)
}

pub fn elapsed_hms(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn short_url(url: &str, max: usize) -> String {
    let cleaned = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    truncate_chars(cleaned, max)
}

const PAGE_EXTENSIONS: [&str; 28] = [
    "png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "bmp", "tiff", "mp4", "webm", "mp3", "wav",
    "ogg", "flac", "pdf", "zip", "rar", "7z", "tar", "gz", "exe", "dmg", "iso", "woff", "woff2",
    "ttf", "css",
];

pub fn looks_like_page(url: &url::Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    if path.ends_with(".js") || path.ends_with(".mjs") || path.ends_with(".map") {
        return false;
    }
    if let Some(ext) = path.rsplit_once('.').map(|(_, e)| e)
        && ext.len() <= 5
        && !ext.contains('/')
        && PAGE_EXTENSIONS.contains(&ext)
    {
        return false;
    }
    true
}
