//! A deliberate line-based subset of docker compose YAML (contract: `detection.md`
//! "Compose files"). No YAML crate: only `services.<name>.{image, build, ports}`.

use serde::Serialize;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortMapping {
    pub published: Option<u16>,
    pub target: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedService {
    pub name: String,
    pub image: Option<String>,
    /// The raw build context (`build: <ctx>` or `build.context`).
    pub build: Option<String>,
    pub ports: Vec<PortMapping>,
    /// A scalar `command:` (shown in notes only, never run).
    pub command: Option<String>,
    /// Service names from `depends_on` (list or mapping form), in file order.
    pub depends_on: Vec<String>,
    /// What the parser could not read, one message each.
    pub unreadable: Vec<String>,
}

/// Image base names (registry, path and tag stripped) that count as infrastructure.
pub const INFRA_IMAGES: [&str; 23] = [
    "postgres",
    "postgis",
    "mysql",
    "mariadb",
    "mongo",
    "mongodb",
    "redis",
    "valkey",
    "keydb",
    "memcached",
    "rabbitmq",
    "kafka",
    "zookeeper",
    "nats",
    "mailpit",
    "mailhog",
    "minio",
    "elasticsearch",
    "opensearch",
    "timescaledb",
    "clickhouse",
    "localstack",
    "azurite",
];

/// Words that mark a compose file as production-only, longest first.
const IGNORE_WORDS: [&str; 5] = ["production", "dokploy", "staging", "deploy", "prod"];

struct Line<'a> {
    indent: usize,
    text: &'a str,
}

/// The `services` of a compose file in file order, or `None` without a
/// top-level `services:` key.
pub fn parse_compose(text: &str) -> Option<Vec<ParsedService>> {
    let lines: Vec<Line<'_>> = text
        .lines()
        .filter_map(|raw| {
            let text = strip_comment(raw).trim_end();
            let trimmed = text.trim_start();
            if trimmed.is_empty() || trimmed == "---" {
                return None;
            }
            Some(Line {
                indent: text.len() - trimmed.len(),
                text: trimmed,
            })
        })
        .collect();
    let start = lines.iter().position(|l| {
        l.indent == 0
            && l.text
                .strip_prefix("services:")
                .is_some_and(|rest| rest.trim().is_empty())
    })?;

    let mut services: Vec<ParsedService> = Vec::new();
    let mut service_indent: Option<usize> = None;
    let mut key_indent: Option<usize> = None;
    let mut current_key = String::new();
    let mut long_port: Option<LongPort> = None;
    let mut dep_indent: Option<usize> = None;

    for line in lines[start + 1..].iter().take_while(|l| l.indent > 0) {
        let svc_indent = *service_indent.get_or_insert(line.indent);
        if line.indent <= svc_indent {
            if let Some(service) = services.last_mut() {
                flush_long_port(&mut long_port, service);
            }
            let name = line.text.split_once(':').map_or(line.text, |(k, _)| k);
            services.push(ParsedService {
                name: unquote(name).to_string(),
                image: None,
                build: None,
                ports: Vec::new(),
                command: None,
                depends_on: Vec::new(),
                unreadable: Vec::new(),
            });
            key_indent = None;
            dep_indent = None;
            current_key.clear();
            continue;
        }
        let Some(service) = services.last_mut() else {
            continue;
        };
        let k_indent = *key_indent.get_or_insert(line.indent);
        if line.indent <= k_indent {
            flush_long_port(&mut long_port, service);
            let (key, value) = split_key(line.text);
            current_key = key.to_string();
            match key {
                "image" if !value.is_empty() => service.image = Some(unquote(value).to_string()),
                "build" if !value.is_empty() => service.build = Some(unquote(value).to_string()),
                "command" if !value.is_empty() && !value.starts_with(['[', '|', '>']) => {
                    service.command = Some(unquote(value).to_string());
                }
                "ports" if value.starts_with('[') => {
                    for item in flow_items(value) {
                        push_short_port(item, service);
                    }
                }
                "depends_on" if value.starts_with('[') => {
                    service
                        .depends_on
                        .extend(flow_items(value).map(|i| unquote(i).to_string()));
                }
                _ => {}
            }
            continue;
        }
        match current_key.as_str() {
            "build" => {
                let (key, value) = split_key(line.text);
                if key == "context" && !value.is_empty() {
                    service.build = Some(unquote(value).to_string());
                }
            }
            "depends_on" => {
                let entry_indent = *dep_indent.get_or_insert(line.indent);
                if let Some(item) = line.text.strip_prefix('-') {
                    service.depends_on.push(unquote(item).to_string());
                } else if line.indent == entry_indent {
                    let (key, _) = split_key(line.text);
                    service.depends_on.push(key.to_string());
                }
            }
            "ports" => {
                let (item, is_item) = match line.text.strip_prefix('-') {
                    Some(rest) => (rest.trim(), true),
                    None => (line.text, false),
                };
                if is_item {
                    flush_long_port(&mut long_port, service);
                }
                if is_long_key(item) {
                    let entry = long_port.get_or_insert_with(LongPort::default);
                    let (key, value) = split_key(item);
                    match key {
                        "target" => entry.target = Some(unquote(value).to_string()),
                        "published" => entry.published = Some(unquote(value).to_string()),
                        _ => {}
                    }
                } else if is_item && !item.is_empty() {
                    push_short_port(item, service);
                }
            }
            _ => {}
        }
    }
    if let Some(service) = services.last_mut() {
        flush_long_port(&mut long_port, service);
    }
    Some(services)
}

#[derive(Default)]
struct LongPort {
    target: Option<String>,
    published: Option<String>,
}

fn flush_long_port(pending: &mut Option<LongPort>, service: &mut ParsedService) {
    let Some(entry) = pending.take() else {
        return;
    };
    let target = entry.target.as_deref().and_then(parse_port);
    let published = entry.published.as_deref().map(parse_port);
    match (target, published) {
        (Some(target), None) => service.ports.push(PortMapping {
            published: None,
            target,
        }),
        (Some(target), Some(Some(published))) => service.ports.push(PortMapping {
            published: Some(published),
            target,
        }),
        _ => service.unreadable.push(format!(
            "port target {:?} published {:?} is not readable",
            entry.target.unwrap_or_default(),
            entry.published.unwrap_or_default()
        )),
    }
}

fn push_short_port(raw: &str, service: &mut ParsedService) {
    let value = unquote(raw);
    match parse_short_port(value) {
        Some(port) => service.ports.push(port),
        None => service
            .unreadable
            .push(format!("port {value:?} is not readable")),
    }
}

/// `"H:C"`, `"IP:H:C"`, `"H:C/proto"` or `"C"`.
fn parse_short_port(value: &str) -> Option<PortMapping> {
    if value.contains('$') {
        return None;
    }
    let value = value.split('/').next().unwrap_or(value);
    let parts: Vec<&str> = value.split(':').collect();
    let (published, target) = match parts.as_slice() {
        [target] => (None, *target),
        [published, target] | [_, published, target] => (Some(*published), *target),
        _ => return None,
    };
    let target = parse_port(target)?;
    let published = match published {
        Some(p) => Some(parse_port(p)?),
        None => None,
    };
    Some(PortMapping { published, target })
}

/// Items of a flow sequence such as `["80:80", 443]`.
fn flow_items(value: &str) -> impl Iterator<Item = &str> {
    value
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(str::trim)
        .filter(|i| !i.is_empty())
}

fn parse_port(value: &str) -> Option<u16> {
    value.trim().parse::<u16>().ok().filter(|p| *p > 0)
}

/// `key: value` of a long-syntax port line (`target: 80`), not a short `80:80`.
fn is_long_key(item: &str) -> bool {
    let (key, _) = split_key(item);
    item.contains(':') && !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

fn split_key(text: &str) -> (&str, &str) {
    match text.split_once(':') {
        Some((key, value)) => (unquote(key.trim()), value.trim()),
        None => (text, ""),
    }
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    for quote in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// Drops a `#` comment that starts a line or follows whitespace outside quotes.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev_space = true;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && prev_space => return &line[..i],
            None => {}
        }
        prev_space = c.is_whitespace();
    }
    line
}

/// `timescale/timescaledb:latest-pg16` -> `timescaledb`.
pub fn image_base(image: &str) -> String {
    let image = image.split('@').next().unwrap_or(image);
    let last = image.rsplit('/').next().unwrap_or(image);
    last.split(':').next().unwrap_or(last).to_ascii_lowercase()
}

pub fn is_infra_image(image: &str) -> bool {
    INFRA_IMAGES.contains(&image_base(image).as_str())
}

/// `docker-compose*.yml|yaml` or `compose*.yml|yaml`.
pub fn is_compose_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    (lower.starts_with("docker-compose") || lower.starts_with("compose"))
        && (lower.ends_with(".yml") || lower.ends_with(".yaml"))
}

/// The word that marks a compose file name as production-only.
pub fn ignore_word(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    IGNORE_WORDS.into_iter().find(|w| lower.contains(w))
}
