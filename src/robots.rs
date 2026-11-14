use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use tokio::sync::Mutex;
use url::Url;

#[derive(Debug, Clone)]
struct Rule {
    allow: bool,
    pattern: String,
}

#[derive(Debug, Clone, Default)]
struct Group {
    agents: Vec<String>,
    rules: Vec<Rule>,
    crawl_delay: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct RobotsRules {
    groups: Vec<Group>,
}

impl RobotsRules {
    pub fn parse(text: &str) -> Self {
        let mut groups: Vec<Group> = Vec::new();
        let mut current: Option<Group> = None;
        let mut last_was_agent = false;

        for raw_line in text.lines() {
            let line = raw_line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((field, value)) = line.split_once(':') else {
                continue;
            };
            let field = field.trim().to_ascii_lowercase();
            let value = value.trim();

            match field.as_str() {
                "user-agent" => {
                    if !last_was_agent {
                        if let Some(group) = current.take() {
                            groups.push(group);
                        }
                        current = Some(Group::default());
                    }
                    if let Some(group) = current.as_mut() {
                        group.agents.push(value.to_ascii_lowercase());
                    }
                    last_was_agent = true;
                }
                "disallow" | "allow" => {
                    last_was_agent = false;
                    let group = current.get_or_insert_with(Group::default);
                    group.rules.push(Rule {
                        allow: field == "allow",
                        pattern: value.to_string(),
                    });
                }
                "crawl-delay" => {
                    last_was_agent = false;
                    let group = current.get_or_insert_with(Group::default);
                    if let Ok(delay) = value.parse::<f64>() {
                        group.crawl_delay = Some(delay);
                    }
                }
                _ => {
                    last_was_agent = false;
                }
            }
        }
        if let Some(group) = current {
            groups.push(group);
        }

        RobotsRules { groups }
    }

    fn matching_groups(&self, user_agent: &str) -> Vec<&Group> {
        let ua = user_agent.to_ascii_lowercase();
        let specific: Vec<&Group> = self
            .groups
            .iter()
            .filter(|g| {
                g.agents
                    .iter()
                    .any(|a| a != "*" && !a.is_empty() && ua.contains(a))
            })
            .collect();
        if !specific.is_empty() {
            return specific;
        }
        self.groups
            .iter()
            .filter(|g| g.agents.iter().any(|a| a == "*"))
            .collect()
    }

    pub fn allowed(&self, user_agent: &str, path: &str) -> bool {
        let groups = self.matching_groups(user_agent);
        let mut best: Option<(usize, bool)> = None;

        for group in groups {
            for rule in &group.rules {
                if rule.pattern.is_empty() {
                    continue;
                }
                if pattern_matches(&rule.pattern, path) {
                    let len = rule.pattern.len();
                    best = match best {
                        Some((best_len, best_allow)) => {
                            if len > best_len || (len == best_len && rule.allow && !best_allow) {
                                Some((len, rule.allow))
                            } else {
                                Some((best_len, best_allow))
                            }
                        }
                        None => Some((len, rule.allow)),
                    };
                }
            }
        }

        best.map(|(_, allow)| allow).unwrap_or(true)
    }

    pub fn crawl_delay(&self, user_agent: &str) -> Option<f64> {
        self.matching_groups(user_agent)
            .iter()
            .filter_map(|g| g.crawl_delay)
            .fold(None, |acc: Option<f64>, d| {
                Some(acc.map_or(d, |a| a.max(d)))
            })
    }
}

fn pattern_matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    if pattern.is_empty() {
        return false;
    }
    if anchored {
        glob_match(pattern, path)
    } else {
        let mut with_wildcard = pattern.to_string();
        with_wildcard.push('*');
        glob_match(&with_wildcard, path)
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == t[ti] || p[pi] == '?') {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(star_pi) = star {
            pi = star_pi + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

pub struct RobotsCache {
    client: Client,
    user_agent: String,
    enabled: bool,
    cache: Mutex<HashMap<String, Arc<RobotsRules>>>,
}

impl RobotsCache {
    pub fn new(client: Client, user_agent: String, enabled: bool) -> Self {
        RobotsCache {
            client,
            user_agent,
            enabled,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn allowed(&self, url: &Url) -> bool {
        if !self.enabled {
            return true;
        }
        let rules = self.rules_for(url).await;
        let mut path = url.path().to_string();
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        rules.allowed(&self.user_agent, &path)
    }

    pub async fn crawl_delay(&self, url: &Url) -> Option<f64> {
        if !self.enabled {
            return None;
        }
        self.rules_for(url).await.crawl_delay(&self.user_agent)
    }

    async fn rules_for(&self, url: &Url) -> Arc<RobotsRules> {
        let origin = url.origin().ascii_serialization();
        if let Some(found) = self.cache.lock().await.get(&origin).cloned() {
            return found;
        }

        let robots_url = format!("{}/robots.txt", origin.trim_end_matches('/'));
        let text = match self
            .client
            .get(&robots_url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                response.text().await.unwrap_or_default()
            }
            _ => String::new(),
        };

        let rules = Arc::new(RobotsRules::parse(&text));
        self.cache.lock().await.insert(origin, Arc::clone(&rules));
        rules
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disallow_and_allow_specificity() {
        let rules = RobotsRules::parse(
            "User-agent: *\nDisallow: /private\nAllow: /private/public\nDisallow: /*.pdf$\n",
        );
        assert!(!rules.allowed("rustrequest", "/private/secret"));
        assert!(rules.allowed("rustrequest", "/private/public/page"));
        assert!(rules.allowed("rustrequest", "/public/page"));
        assert!(!rules.allowed("rustrequest", "/files/doc.pdf"));
        assert!(rules.allowed("rustrequest", "/files/doc.html"));
    }

    #[test]
    fn user_agent_specific_overrides_star() {
        let rules =
            RobotsRules::parse("User-agent: *\nDisallow: /\n\nUser-agent: rustrequest\nAllow: /\n");
        assert!(rules.allowed("rustrequest/0.1", "/anything"));
        assert!(!rules.allowed("otherbot", "/anything"));
    }

    #[test]
    fn crawl_delay_is_parsed() {
        let rules = RobotsRules::parse("User-agent: *\nCrawl-delay: 2.5\nDisallow:\n");
        assert_eq!(rules.crawl_delay("rustrequest"), Some(2.5));
    }
}
