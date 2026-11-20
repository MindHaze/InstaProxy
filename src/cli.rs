use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::{
    io::{self, BufRead},
    path::PathBuf,
};

#[derive(Debug, Parser)]
#[command(
    name = "instaproxy",
    version,
    about = "Scrape and check public proxies safely"
)]
pub struct Cli {
    #[arg(
        short,
        long,
        global = true,
        value_name = "PATH",
        help = "YAML file (default: search current directory, executable directory, then Cargo project root)"
    )]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn config_path(&self) -> Result<PathBuf> {
        if let Some(path) = &self.config {
            return Ok(path.clone());
        }

        let cwd = std::env::current_dir().context("could not determine current directory")?;
        let executable = std::env::current_exe().context("could not locate executable")?;
        let mut candidates = vec![cwd.join("config.yaml")];
        if let Some(directory) = executable.parent() {
            let adjacent = directory.join("config.yaml");
            if !candidates.contains(&adjacent) {
                candidates.push(adjacent);
            }
            // A normal Cargo build lives in <project>/target/<profile>/.
            if let Some(target) = directory.parent() {
                if target.file_name().is_some_and(|name| name == "target") {
                    if let Some(project) = target.parent() {
                        let root_config = project.join("config.yaml");
                        if !candidates.contains(&root_config) {
                            candidates.push(root_config);
                        }
                    }
                }
            }
        }

        for path in &candidates {
            if path
                .try_exists()
                .with_context(|| format!("could not access {}", path.display()))?
            {
                return Ok(path.clone());
            }
        }
        let searched = candidates
            .iter()
            .map(|path| format!("  {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        bail!("could not locate config.yaml; searched:\n{searched}\nUse --config <PATH> to select a YAML file.")
    }
}

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum Command {
    #[command(
        name = "scrape-check",
        visible_alias = "run",
        about = "Scrape configured sources and check the collected proxies"
    )]
    ScrapeCheck,
    #[command(
        name = "scrape",
        about = "Scrape configured sources and save a timestamped snapshot"
    )]
    Scrape,
    #[command(
        name = "check",
        about = "Check proxies from history.txt and proxies.txt"
    )]
    Check,
}

pub fn print_banner() {
    let banner = r#"
  ___            _        ____
 |_ _|_ __   ___| |_ __ _|  _ \ _ __ _____  ___   _
  | || '_ \ / __| __/ _` | |_) | '__/ _ \ \/ / | | |
  | || | | | (__| || (_| |  __/| | | (_) >  <| |_| |
 |___|_| |_|\___|\__\__,_|_|   |_|  \___/_/\_\\__, |
                                              |___/
"#;
    println!("{}", banner.bright_cyan().bold());
    println!("{}\n", "Public proxy scraping and health checking".dimmed());
}

pub fn choose_command() -> Result<Option<Command>> {
    println!("{}", "Choose an operation:".bright_white().bold());
    println!("  {} Scrape + Check", "[1]".bright_cyan());
    println!("  {} Scrape", "[2]".bright_cyan());
    println!("  {} Check", "[3]".bright_cyan());
    println!("  {} Exit", "[0]".bright_cyan());
    print!("Selection: ");
    use std::io::Write;
    io::stdout().flush()?;

    let mut input = String::new();
    let read = io::stdin().lock().read_line(&mut input)?;
    if read == 0 {
        return Ok(None);
    }
    match parse_menu_choice(input.trim()) {
        Ok(command) => Ok(command),
        Err(()) => {
            println!("{}", "Invalid selection.".red());
            Ok(None)
        }
    }
}

fn parse_menu_choice(value: &str) -> std::result::Result<Option<Command>, ()> {
    match value {
        "1" => Ok(Some(Command::ScrapeCheck)),
        "2" => Ok(Some(Command::Scrape)),
        "3" => Ok(Some(Command::Check)),
        "0" => Ok(None),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_choices_map_to_commands() {
        assert!(matches!(
            parse_menu_choice("1"),
            Ok(Some(Command::ScrapeCheck))
        ));
        assert!(matches!(parse_menu_choice("2"), Ok(Some(Command::Scrape))));
        assert!(matches!(parse_menu_choice("3"), Ok(Some(Command::Check))));
        assert!(matches!(parse_menu_choice("0"), Ok(None)));
        assert!(parse_menu_choice("9").is_err());
    }
}
