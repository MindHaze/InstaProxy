use crate::{
    model::{CheckResult, CheckStatus, ProxySpec, ProxyType},
    scraper::SourceReport,
};
use anyhow::Result;
use chrono::Local;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub fn create_run_dir(results_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(results_dir)?;
    let stamp = Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut candidate = results_dir.join(&stamp);
    let mut suffix = 1_u32;
    while candidate.exists() {
        candidate = results_dir.join(format!("{stamp}-{suffix}"));
        suffix += 1;
    }
    fs::create_dir_all(&candidate)?;
    Ok(candidate)
}

pub fn write_scrape_snapshot(
    run_dir: &Path,
    proxies: &[ProxySpec],
    reports: &[SourceReport],
) -> Result<()> {
    for proxy_type in ProxyType::all() {
        let entries = proxies
            .iter()
            .filter(|proxy| proxy.proxy_type == *proxy_type && !proxy.auth_hint)
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        write_lines(
            run_dir.join(format!("scraped_{}", proxy_type.file_name())),
            &entries,
        )?;
    }

    let mut source_lines = Vec::new();
    for report in reports {
        let status = report
            .status
            .map(|value| value.to_string())
            .unwrap_or_else(|| "n/a".into());
        let state = if report.available {
            "available"
        } else {
            "unavailable"
        };
        let error = report.error.as_deref().unwrap_or("-");
        source_lines.push(format!(
            "{} | {} | HTTP {} | found={} | error={}",
            state, report.name, status, report.found, error
        ));
    }
    write_lines(run_dir.join("sources.txt"), &source_lines)
}

pub fn write_check_results(run_dir: &Path, results: &[CheckResult]) -> Result<()> {
    for proxy_type in ProxyType::all() {
        let entries = results
            .iter()
            .filter(|result| result.proxy.proxy_type == *proxy_type && result.status.is_saveable())
            .map(|result| result.proxy.to_string())
            .collect::<Vec<_>>();
        write_lines(run_dir.join(proxy_type.file_name()), &entries)?;
    }

    let slow = results
        .iter()
        .filter(|result| result.status == CheckStatus::Slow)
        .map(|result| result.proxy.to_string())
        .collect::<Vec<_>>();
    write_lines(run_dir.join("slow.txt"), &slow)?;

    let mut summary = String::from("InstaProxy check summary\n========================\n");
    for proxy_type in ProxyType::all() {
        let typed = results
            .iter()
            .filter(|result| result.proxy.proxy_type == *proxy_type);
        let mut working = 0;
        let mut slow = 0;
        let mut dead = 0;
        let mut timeouts = 0;
        let mut auth = 0;
        for result in typed {
            match result.status {
                CheckStatus::Working => working += 1,
                CheckStatus::Slow => slow += 1,
                CheckStatus::Dead => dead += 1,
                CheckStatus::Timeout => timeouts += 1,
                CheckStatus::AuthRequired => auth += 1,
            }
        }
        summary.push_str(&format!(
            "{proxy_type}: working={working}, slow={slow}, dead={dead}, timeout={timeouts}, auth_required={auth}\n"
        ));
    }
    summary.push_str(&format!("total_checked={}\n", results.len()));
    write_lines(run_dir.join("summary.txt"), &[summary])
}

pub fn update_history(path: &Path, results: &[CheckResult]) -> Result<usize> {
    let mut entries = BTreeSet::new();
    if path.exists() {
        let contents = fs::read_to_string(path)?;
        for line in contents.lines() {
            if let Some(proxy) = crate::model::parse_proxy_line(line, None) {
                entries.insert(proxy.to_string());
            }
        }
    }

    let before = entries.len();
    for result in results.iter().filter(|result| result.status.is_saveable()) {
        entries.insert(result.proxy.to_string());
    }

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let contents = entries.into_iter().collect::<Vec<_>>().join("\n");
    fs::write(
        path,
        if contents.is_empty() {
            String::new()
        } else {
            format!("{contents}\n")
        },
    )?;
    Ok(contents.lines().count().saturating_sub(before))
}

fn write_lines(path: PathBuf, lines: &[String]) -> Result<()> {
    let contents = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    fs::write(path, contents)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn result(proxy: &str, status: CheckStatus) -> CheckResult {
        CheckResult {
            proxy: crate::model::parse_proxy_line(proxy, None).unwrap(),
            status,
            latency_ms: Some(10),
            detail: String::new(),
        }
    }

    #[test]
    fn history_excludes_dead_and_auth_entries() {
        let directory = tempdir().unwrap();
        let history = directory.path().join("history.txt");
        let results = vec![
            result("http://192.0.2.1:80", CheckStatus::Working),
            result("http://192.0.2.2:80", CheckStatus::Dead),
            result("http://192.0.2.3:80", CheckStatus::AuthRequired),
        ];
        assert_eq!(update_history(&history, &results).unwrap(), 1);
        let contents = fs::read_to_string(history).unwrap();
        assert!(contents.contains("http://192.0.2.1:80"));
        assert!(!contents.contains("192.0.2.2"));
        assert!(!contents.contains("192.0.2.3"));
    }
}
