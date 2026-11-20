use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fmt, str::FromStr};
use url::Url;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyType {
    Http,
    Https,
    Socks4,
    Socks5,
}

impl ProxyType {
    pub const fn all() -> &'static [Self] {
        &[Self::Http, Self::Https, Self::Socks4, Self::Socks5]
    }

    pub const fn scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks4 => "socks4",
            Self::Socks5 => "socks5",
        }
    }

    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Http => "http.txt",
            Self::Https => "https.txt",
            Self::Socks4 => "socks4.txt",
            Self::Socks5 => "socks5.txt",
        }
    }
}

impl fmt::Display for ProxyType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.scheme())
    }
}

impl FromStr for ProxyType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            "socks4" | "socks4a" => Ok(Self::Socks4),
            "socks5" | "socks5h" => Ok(Self::Socks5),
            other => Err(format!("unsupported proxy type: {other}")),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ProxySpec {
    pub host: String,
    pub port: u16,
    pub proxy_type: ProxyType,
    pub auth_hint: bool,
}

impl ProxySpec {
    pub fn canonical_key(&self) -> (String, u16, ProxyType) {
        (self.host.to_ascii_lowercase(), self.port, self.proxy_type)
    }
}

impl fmt::Display for ProxySpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') && !self.host.starts_with('[') {
            write!(
                formatter,
                "{}://[{}]:{}",
                self.proxy_type, self.host, self.port
            )
        } else {
            write!(
                formatter,
                "{}://{}:{}",
                self.proxy_type, self.host, self.port
            )
        }
    }
}

pub fn parse_proxy_line(line: &str, hinted_type: Option<ProxyType>) -> Option<ProxySpec> {
    let clean = line.split('#').next()?.trim();
    if clean.is_empty() {
        return None;
    }

    clean
        .split(|character: char| character.is_whitespace() || matches!(character, ',' | ';' | '|'))
        .filter(|token| !token.is_empty())
        .find_map(|token| parse_token(token, hinted_type))
}

fn parse_token(token: &str, hinted_type: Option<ProxyType>) -> Option<ProxySpec> {
    let (url_text, explicit_type) = if token.contains("://") {
        (token.to_string(), None)
    } else {
        let proxy_type = hinted_type.unwrap_or(ProxyType::Http);
        (
            format!("{}://{}", proxy_type.scheme(), token),
            Some(proxy_type),
        )
    };

    let parsed = Url::parse(&url_text).ok()?;
    let proxy_type = explicit_type.or_else(|| ProxyType::from_str(parsed.scheme()).ok())?;
    let host = parsed.host_str()?.trim().to_string();
    if host.is_empty() {
        return None;
    }
    // Url normalizes explicit :80/:443 away, so read the required port from the input.
    let authority = url_text
        .split_once("://")?
        .1
        .split(['/', '?', '#'])
        .next()?;
    let port = authority.rsplit_once(':')?.1.parse::<u16>().ok()?;
    if port == 0 {
        return None;
    }

    Some(ProxySpec {
        host,
        port,
        proxy_type,
        auth_hint: !parsed.username().is_empty() || parsed.password().is_some(),
    })
}

pub fn deduplicate(proxies: impl IntoIterator<Item = ProxySpec>) -> Vec<ProxySpec> {
    let mut unique = HashMap::<(String, u16, ProxyType), ProxySpec>::new();
    for proxy in proxies {
        let key = proxy.canonical_key();
        unique
            .entry(key)
            .and_modify(|existing| existing.auth_hint |= proxy.auth_hint)
            .or_insert(proxy);
    }
    let mut result: Vec<_> = unique.into_values().collect();
    result.sort_by_key(|proxy| proxy.to_string());
    result
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CheckStatus {
    Working,
    Slow,
    Dead,
    Timeout,
    AuthRequired,
}

impl CheckStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Working => "ALIVE",
            Self::Slow => "ALIVE SLOW",
            Self::Dead => "DEAD",
            Self::Timeout => "TIMEOUT",
            Self::AuthRequired => "AUTH REQUIRED",
        }
    }

    pub const fn is_saveable(self) -> bool {
        matches!(self, Self::Working | Self::Slow)
    }
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub proxy: ProxySpec,
    pub status: CheckStatus,
    pub latency_ms: Option<u128>,
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_proxy_with_source_hint() {
        let proxy = parse_proxy_line("192.0.2.10:8080", Some(ProxyType::Socks5)).unwrap();
        assert_eq!(proxy.proxy_type, ProxyType::Socks5);
        assert_eq!(proxy.to_string(), "socks5://192.0.2.10:8080");
    }

    #[test]
    fn parses_urls_and_marks_credentials_without_retaining_them() {
        let proxy = parse_proxy_line("http://user:secret@proxy.example:3128", None).unwrap();
        assert!(proxy.auth_hint);
        assert_eq!(proxy.to_string(), "http://proxy.example:3128");
    }

    #[test]
    fn ignores_comments_and_invalid_lines() {
        assert!(parse_proxy_line("# no proxy here", Some(ProxyType::Http)).is_none());
        assert!(parse_proxy_line("not-a-proxy", Some(ProxyType::Http)).is_none());
    }

    #[test]
    fn preserves_explicit_default_ports() {
        for (input, port) in [
            ("http://192.0.2.1:80", 80),
            ("192.0.2.1:80", 80),
            ("https://192.0.2.1:443", 443),
        ] {
            assert_eq!(
                parse_proxy_line(input, None).map(|proxy| proxy.port),
                Some(port)
            );
        }
    }

    #[test]
    fn rejects_missing_and_zero_ports() {
        for input in [
            "http://proxy.example",
            "proxy.example",
            "http://192.0.2.1:0",
        ] {
            assert!(parse_proxy_line(input, None).is_none(), "{input}");
        }
    }

    #[test]
    fn deduplicates_case_insensitively() {
        let first = parse_proxy_line("HTTP://Proxy.Example:80", None).unwrap();
        let second = parse_proxy_line("http://proxy.example:80", None).unwrap();
        assert_eq!(deduplicate([first, second]).len(), 1);
    }
}
