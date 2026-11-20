mod checker;
mod cli;
mod config;
mod model;
mod output;
mod progress;
mod scraper;

use anyhow::Result;
use clap::Parser;
use colored::Colorize;
use config::Config;
use futures::StreamExt;
use model::{deduplicate, parse_proxy_line, CheckResult, CheckStatus, ProxySpec, ProxyType};
use progress::{set_console_title, CheckProgress};
use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::{Duration, Instant},
};
use tokio::time::{interval, MissedTickBehavior};

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli::print_banner();
    set_console_title("InstaProxy");
    let config = Config::load(&cli.config_path()?)?;
    let command = match cli.command {
        Some(command) => Some(command),
        None => cli::choose_command()?,
    };

    match command {
        Some(Command::ScrapeCheck) => run_scrape_check(&config).await?,
        Some(Command::Scrape) => run_scrape(&config).await?,
        Some(Command::Check) => run_check(&config).await?,
        None => println!("{}", "No operation selected.".dimmed()),
    }
    Ok(())
}

async fn run_scrape(config: &Config) -> Result<()> {
    println!("{}", "Scraping configured sources...".bright_white().bold());
    let scraped = scraper::scrape_sources(config).await?;
    print_source_reports(&scraped.reports);
    let run_dir = output::create_run_dir(&config.results_dir)?;
    output::write_scrape_snapshot(&run_dir, &scraped.proxies, &scraped.reports)?;
    println!(
        "{} {} proxies saved to {}",
        "[SCRAPE]".green().bold(),
        scraped.proxies.len(),
        run_dir.display()
    );
    Ok(())
}

async fn run_scrape_check(config: &Config) -> Result<()> {
    println!("{}", "Scraping configured sources...".bright_white().bold());
    let scraped = scraper::scrape_sources(config).await?;
    print_source_reports(&scraped.reports);

    let run_dir = output::create_run_dir(&config.results_dir)?;
    let scraped_proxies = scraped.proxies.clone();
    let candidates = load_candidates(config, scraped.proxies)?;
    println!(
        "{} checking {} unique proxies with {} concurrent workers",
        "[CHECK]".bright_cyan().bold(),
        candidates.len(),
        config.threads
    );
    let results = run_checks(candidates, config).await?;
    let saveable_keys: HashSet<_> = results
        .iter()
        .filter(|result| result.status.is_saveable())
        .map(|result| result.proxy.canonical_key())
        .collect();
    let safe_scraped = scraped_proxies
        .into_iter()
        .filter(|proxy| saveable_keys.contains(&proxy.canonical_key()))
        .collect::<Vec<_>>();
    output::write_scrape_snapshot(&run_dir, &safe_scraped, &scraped.reports)?;
    output::write_check_results(&run_dir, &results)?;
    let added = output::update_history(&config.history_file, &results)?;
    print_completion(&run_dir, &results, added);
    Ok(())
}

async fn run_check(config: &Config) -> Result<()> {
    let run_dir = output::create_run_dir(&config.results_dir)?;
    let candidates = load_candidates(config, Vec::new())?;
    if candidates.is_empty() {
        println!(
            "{} no proxies found in the configured input files",
            "[CHECK]".yellow().bold()
        );
    } else {
        println!(
            "{} checking {} unique proxies with {} concurrent workers",
            "[CHECK]".bright_cyan().bold(),
            candidates.len(),
            config.threads
        );
    }
    let results = run_checks(candidates, config).await?;
    output::write_check_results(&run_dir, &results)?;
    let added = output::update_history(&config.history_file, &results)?;
    print_completion(&run_dir, &results, added);
    Ok(())
}

async fn run_checks(candidates: Vec<ProxySpec>, config: &Config) -> Result<Vec<CheckResult>> {
    let mut progress = CheckProgress::new(candidates.len());
    let mut results = Vec::with_capacity(candidates.len());
    let pending = checker::check_stream(candidates, config)?;
    futures::pin_mut!(pending);
    let mut ticks = interval(Duration::from_millis(150));
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_log = Instant::now();
    set_console_title(&progress.title());
    println!("{} {}", "[PROGRESS]".bright_cyan(), progress.summary());
    loop {
        tokio::select! {
            result = pending.next() => {
                let Some(result) = result else { break };
                progress.record(result.status);
                print_check_result(&result);
                results.push(result);
                last_log = Instant::now();
            }
            _ = ticks.tick() => {
                progress.advance();
                set_console_title(&progress.title());
                if last_log.elapsed() >= Duration::from_secs(2) {
                    println!("{} {}", "[PROGRESS]".bright_cyan(), progress.summary());
                    last_log = Instant::now();
                }
            }
        }
    }
    set_console_title(&progress.title());
    println!("{} {}", "[PROGRESS]".bright_cyan(), progress.summary());
    results.sort_by_key(|result| result.proxy.to_string());
    Ok(results)
}

fn load_candidates(config: &Config, initial: Vec<ProxySpec>) -> Result<Vec<ProxySpec>> {
    let mut candidates = initial;
    if config.input_file.exists() {
        candidates.extend(read_proxy_file(&config.input_file, config)?);
    }
    if config.check_history && config.history_file.exists() {
        candidates.extend(read_proxy_file(&config.history_file, config)?);
    }
    let mut candidates = deduplicate(candidates)
        .into_iter()
        .filter(|proxy| config.accepts_type(proxy.proxy_type))
        .collect::<Vec<_>>();
    if candidates.len() > config.max_total_proxies {
        candidates.truncate(config.max_total_proxies);
    }
    Ok(candidates)
}

fn read_proxy_file(path: &Path, config: &Config) -> Result<Vec<ProxySpec>> {
    let contents = fs::read_to_string(path)?;
    Ok(contents
        .lines()
        .filter_map(|line| parse_proxy_line(line, None))
        .filter(|proxy| config.accepts_type(proxy.proxy_type))
        .collect())
}

fn print_source_reports(reports: &[scraper::SourceReport]) {
    for report in reports {
        let status = report
            .status
            .map(|value| value.to_string())
            .unwrap_or_else(|| "n/a".into());
        if report.available {
            println!(
                "{} {} ({} | HTTP {}, {} proxies)",
                "[SOURCE OK]".green(),
                report.url,
                report.name,
                status,
                report.found
            );
        } else {
            println!(
                "{} {} ({} | HTTP {}, {})",
                "[SOURCE DOWN]".red(),
                report.url,
                report.name,
                status,
                report.error.as_deref().unwrap_or("unavailable")
            );
        }
    }
}

fn print_check_result(result: &CheckResult) {
    let label = match result.status {
        CheckStatus::Working => result.status.label().green().bold(),
        CheckStatus::Slow => result.status.label().yellow().bold(),
        CheckStatus::Dead => result.status.label().red().bold(),
        CheckStatus::Timeout => result.status.label().bright_yellow().bold(),
        CheckStatus::AuthRequired => result.status.label().magenta().bold(),
    };
    let latency = result
        .latency_ms
        .map(|value| format!("{value} ms"))
        .unwrap_or_else(|| "n/a".into());
    let proxy_type = match result.proxy.proxy_type {
        ProxyType::Http => "HTTP".blue(),
        ProxyType::Https => "HTTPS".bright_blue(),
        ProxyType::Socks4 => "SOCKS4".cyan(),
        ProxyType::Socks5 => "SOCKS5".bright_cyan(),
    };
    println!(
        "[{label}] {:<8} {:>8} {} | {}",
        proxy_type,
        latency,
        result.proxy,
        result.detail.dimmed()
    );
}

fn print_completion(run_dir: &Path, results: &[CheckResult], added_to_history: usize) {
    let working = results
        .iter()
        .filter(|result| result.status.is_saveable())
        .count();
    let auth = results
        .iter()
        .filter(|result| result.status == CheckStatus::AuthRequired)
        .count();
    println!(
        "{} {} working/slow, {} auth-required excluded, history +{}, output {}",
        "[DONE]".bright_green().bold(),
        working,
        auth,
        added_to_history,
        run_dir.display()
    );
}
