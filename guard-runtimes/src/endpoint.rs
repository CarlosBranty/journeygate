//! Gateway pinning: the only model endpoint a runtime may be pointed at.
//!
//! Two checks, both before the runtime starts:
//! 1. [`GatewayEndpoint::pin`]: `task.gateway.base_url` is a plain `http(s)`
//!    root (no credentials, path, query or fragment) and `via_gateway` is
//!    `true`. Only `copilot-cli` may run without it (the explicit
//!    `via_gateway = false` exception, agents-toml.md).
//! 2. [`ensure_only_gateway`]: no URL in the invocation (argv, env values,
//!    config files) points anywhere but the pinned root. With the Copilot
//!    exception no URL may appear at all. The prompt (stdin, or the argv item
//!    carrying it) is not scanned: a ticket may quote a URL without making it
//!    an endpoint.
//!
//! The sandbox egress proxy (sandbox.md) is the enforcing layer; these checks
//! make a misconfigured adapter fail loudly instead of silently being dropped
//! by the proxy.

use crate::invocation::Invocation;
use guard_core::ports::agent::{GatewayBinding, RuntimeError};
use reqwest::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayEndpoint {
    root: String,
}

impl GatewayEndpoint {
    pub fn pin(binding: &GatewayBinding) -> Result<Self, RuntimeError> {
        if !binding.via_gateway {
            return Err(RuntimeError::InvalidTask(
                "gateway.via_gateway = false is allowed only for copilot-cli".into(),
            ));
        }
        Self::parse(&binding.base_url).map_err(RuntimeError::InvalidTask)
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let bad = |why: &str| format!("gateway.base_url {raw:?}: {why}");
        let url = Url::parse(raw.trim()).map_err(|e| bad(&e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(bad("scheme must be http or https"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(bad("credentials are not allowed in the URL"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(bad("query and fragment are not allowed"));
        }
        if url.path() != "/" {
            return Err(bad("must be the gateway root, without a path"));
        }
        let host = url.host_str().ok_or_else(|| bad("missing host"))?;
        let root = match url.port() {
            Some(p) => format!("{}://{host}:{p}", url.scheme()),
            None => format!("{}://{host}", url.scheme()),
        };
        Ok(Self { root })
    }

    /// `scheme://host[:port]`, no trailing slash. LiteLLM serves the Anthropic
    /// Messages API (`/v1/messages`) under it.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The OpenAI-compatible base (`/v1/chat/completions`, `/v1/responses`).
    pub fn v1(&self) -> String {
        format!("{}/v1", self.root)
    }
}

/// Fails with `InvalidTask` naming the first URL in `inv` that is not under
/// `gateway` (or any URL at all when `gateway` is `None`).
pub fn ensure_only_gateway(
    inv: &Invocation,
    gateway: Option<&GatewayEndpoint>,
) -> Result<(), RuntimeError> {
    ensure_only_gateway_except(inv, gateway, None)
}

/// [`ensure_only_gateway`], also skipping the argv item equal to `prompt`.
pub(crate) fn ensure_only_gateway_except(
    inv: &Invocation,
    gateway: Option<&GatewayEndpoint>,
    prompt: Option<&str>,
) -> Result<(), RuntimeError> {
    let mut haystacks: Vec<&str> = inv
        .argv
        .iter()
        .map(String::as_str)
        .filter(|a| Some(*a) != prompt)
        .collect();
    haystacks.extend(inv.env.values().map(String::as_str));
    haystacks.extend(inv.files.iter().map(|f| f.contents.as_str()));
    for text in haystacks {
        for url in urls_in(text) {
            let allowed = gateway.is_some_and(|g| {
                url == g.root
                    || url
                        .strip_prefix(g.root.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            });
            if !allowed {
                return Err(RuntimeError::InvalidTask(format!(
                    "invocation references {url}, which is not the gateway{}",
                    gateway.map(|g| format!(" {}", g.root)).unwrap_or_default()
                )));
            }
        }
    }
    Ok(())
}

/// Every `http://` / `https://` token, up to the first character that cannot
/// be part of a URL in our config formats (whitespace, quotes, brackets).
fn urls_in(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("http") {
        let cand = &rest[i..];
        if cand.starts_with("http://") || cand.starts_with("https://") {
            let end = cand
                .find(|c: char| c.is_whitespace() || "\"'`<>()[]{},;".contains(c))
                .unwrap_or(cand.len());
            out.push(&cand[..end]);
            rest = &cand[end..];
        } else {
            rest = &cand[4..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_urls_in_config_formats() {
        assert_eq!(
            urls_in("base_url = \"http://gw:4000/v1\"\n{\"u\":\"https://x.y/z\"} httpfoo"),
            vec!["http://gw:4000/v1", "https://x.y/z"]
        );
    }
}
