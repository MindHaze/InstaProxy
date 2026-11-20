use crate::{
    config::{Config, SourceConfig},
    model::{deduplicate, parse_proxy_line, ProxySpec},
};
use anyhow::{anyhow, Context, Result};
use futures::{stream, StreamExt};
use reqwest::Client;
use serde::Serialize;
use std::{collections::HashSet, fs, time::Duration};
use url::Url;

const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct SourceReport {
    pub name: String,
    pub url: String,
    pub available: bool,
    pub status: Option<u16>,
    pub found: usize,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct ScrapeOutcome {
    pub proxies: Vec<ProxySpec>,
    pub reports: Vec<SourceReport>,
}

pub async fn scrape_sources(config: &Config) -> Result<ScrapeOutcome> {
    let client = Client::builder()
        .timeout(Duration::from_millis(config.source_timeout_ms))
        .user_agent(config.user_agent.clone())
        .build()?;

    let mut sources: Vec<SourceConfig> = config
        .sources
        .iter()
        .filter(|source| source.enabled)
        .cloned()
        .collect();
    if let Some(path) = &config.sources_file {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("could not read source file {}", path.display()))?;
        for (index, line) in contents.trim_start_matches('\u{feff}').lines().enumerate() {
            if line.split('#').next().unwrap_or_default().trim().is_empty() {
                continue;
            }
            let mut source = source_from_line(index + 1, line).ok_or_else(|| {
                anyhow!(
                    "{}:{}: expected an HTTP(S) source URL without credentials",
                    path.display(),
                    index + 1
                )
            })?;
            let filename = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            source.name = format!("{filename}:{}", index + 1);
            sources.push(source);
        }
    }
    let mut seen_urls = HashSet::new();
    sources.retain(|source| seen_urls.insert(source.url.clone()));
    let limit = config.threads.max(1).min(sources.len().max(1));

    let mut reports = Vec::with_capacity(sources.len());
    let mut all_proxies = Vec::new();
    let mut pending = stream::iter(sources.into_iter().map(|source| {
        let client = client.clone();
        async move { fetch_source(client, source, config.max_proxies_per_source).await }
    }))
    .buffer_unordered(limit);

    while let Some((report, proxies)) = pending.next().await {
        all_proxies.extend(proxies);
        reports.push(report);
    }

    reports.sort_by(|left, right| left.name.cmp(&right.name));
    let mut proxies = deduplicate(all_proxies)
        .into_iter()
        .filter(|proxy| config.accepts_type(proxy.proxy_type))
        .collect::<Vec<_>>();
    if proxies.len() > config.max_total_proxies {
        proxies.truncate(config.max_total_proxies);
    }

    Ok(ScrapeOutcome { proxies, reports })
}

fn source_from_line(line_number: usize, line: &str) -> Option<SourceConfig> {
    let url = line
        .trim_start_matches('\u{feff}')
        .split('#')
        .next()?
        .trim();
    if url.is_empty() {
        return None;
    }
    let parsed = Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    let proxy_type = parsed
        .query_pairs()
        .find_map(|(key, value)| (key == "type").then(|| value.parse().ok()).flatten())
        .or_else(|| {
            parsed
                .path()
                .rsplit(['/', '.', '-', '_'])
                .find_map(|part| part.parse().ok())
        })
        .or(Some(crate::model::ProxyType::Http));
    Some(SourceConfig {
        name: format!("source.txt:{line_number}"),
        url: url.to_string(),
        proxy_type,
        enabled: true,
    })
}

async fn fetch_source(
    client: Client,
    source: SourceConfig,
    max_proxies: usize,
) -> (SourceReport, Vec<ProxySpec>) {
    let mut report = SourceReport {
        name: source.name.clone(),
        url: source.url.clone(),
        available: false,
        status: None,
        found: 0,
        error: None,
    };

    let response = match client.get(&source.url).send().await {
        Ok(response) => response,
        Err(error) => {
            report.error = Some(error.to_string());
            return (report, Vec::new());
        }
    };
    report.status = Some(response.status().as_u16());
    if !response.status().is_success() {
        report.error = Some(format!("source returned HTTP {}", response.status()));
        return (report, Vec::new());
    }

    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            report.error = Some(error.to_string());
            return (report, Vec::new());
        }
    };
    if bytes.len() > MAX_SOURCE_BYTES {
        report.error = Some(format!("source body exceeds {MAX_SOURCE_BYTES} bytes"));
        return (report, Vec::new());
    }

    let body = String::from_utf8_lossy(&bytes);
    let proxies: Vec<ProxySpec> = body
        .lines()
        .filter_map(|line| parse_proxy_line(line, source.proxy_type))
        .take(max_proxies)
        .collect();
    report.available = true;
    report.found = proxies.len();
    (report, proxies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProxyType;

    #[test]
    fn source_lines_accept_bom_and_infer_type_from_path_not_host() {
        for (line, expected) in [
            ("\u{feff}https://example.com/socks5.txt", ProxyType::Socks5),
            (
                " https://example.com/socks4.txt # comment",
                ProxyType::Socks4,
            ),
            ("https://example.com/api?type=https", ProxyType::Https),
            ("https://socks5.example.com/http.txt", ProxyType::Http),
        ] {
            let source = source_from_line(1, line).unwrap();
            assert_eq!(source.proxy_type, Some(expected), "{line}");
        }
        assert!(source_from_line(1, "  # comment").is_none());
        assert!(source_from_line(1, "").is_none());
    }

    #[test]
    fn source_lines_reject_credentials_and_non_http_urls() {
        for line in [
            "https://user:secret@example.com/http.txt",
            "file:///tmp/list",
            "invalid",
        ] {
            assert!(source_from_line(1, line).is_none(), "{line}");
        }
    }

    #[tokio::test]
    async fn missing_source_file_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            sources_file: Some(directory.path().join("missing.txt")),
            sources: Vec::new(),
            ..Config::default()
        };
        let error = scrape_sources(&config).await.unwrap_err();
        assert!(error.to_string().contains("missing.txt"));
    }

    #[tokio::test]
    async fn invalid_source_line_reports_path_and_line_number() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom-sources.txt");
        fs::write(&path, "# sources\nnot-a-url\n").unwrap();
        let config = Config {
            sources_file: Some(path),
            sources: Vec::new(),
            ..Config::default()
        };
        let error = scrape_sources(&config).await.unwrap_err();
        assert!(error.to_string().contains("custom-sources.txt:2"));
    }

    #[test]
    fn source_report_defaults_to_unavailable() {
        let report = SourceReport {
            name: "test".into(),
            url: "https://example.com".into(),
            available: false,
            status: None,
            found: 0,
            error: Some("offline".into()),
        };
        assert!(!report.available);
    }

    #[test]
    fn configured_type_is_applied_by_parser() {
        let proxy = parse_proxy_line("192.0.2.1:1080", Some(ProxyType::Socks5)).unwrap();
        assert_eq!(proxy.proxy_type, ProxyType::Socks5);
    }
}
