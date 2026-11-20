use crate::model::ProxyType;
use serde::Deserialize;
use std::{
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
};
use thiserror::Error;
use url::Url;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(default = "default_max_timeout_ms")]
    pub max_timeout_ms: u64,
    #[serde(default = "default_source_timeout_ms")]
    pub source_timeout_ms: u64,
    #[serde(default = "default_threads")]
    pub threads: usize,
    #[serde(default = "default_proxy_types")]
    pub proxy_types: Vec<ProxyType>,
    #[serde(default = "default_slow_threshold_ms")]
    pub slow_threshold_ms: u64,
    #[serde(default = "default_probe_url")]
    pub probe_url: String,
    #[serde(default)]
    pub check_history: bool,
    #[serde(default = "default_history_file")]
    pub history_file: PathBuf,
    #[serde(default = "default_input_file")]
    pub input_file: PathBuf,
    #[serde(default = "default_results_dir")]
    pub results_dir: PathBuf,
    #[serde(default = "default_max_proxies_per_source")]
    pub max_proxies_per_source: usize,
    #[serde(default = "default_max_total_proxies")]
    pub max_total_proxies: usize,
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    #[serde(default = "default_sources_file")]
    pub sources_file: Option<PathBuf>,
    #[serde(default = "default_sources")]
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub proxy_type: Option<ProxyType>,
    #[serde(default = "default_source_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not parse YAML config: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config: Self = serde_yaml::from_str(&contents)?;
        config.validate()?;
        let base = path.parent().unwrap_or(Path::new("."));
        for data_path in [
            &mut config.input_file,
            &mut config.history_file,
            &mut config.results_dir,
        ]
        .into_iter()
        .chain(config.sources_file.iter_mut())
        {
            if data_path.is_relative() {
                *data_path = base.join(&*data_path);
            }
        }
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.max_timeout_ms == 0 {
            return Err(ConfigError::Invalid(
                "max_timeout_ms must be greater than zero".into(),
            ));
        }
        if self.source_timeout_ms == 0 {
            return Err(ConfigError::Invalid(
                "source_timeout_ms must be greater than zero".into(),
            ));
        }
        if self.threads == 0 || self.threads > 2_048 {
            return Err(ConfigError::Invalid(
                "threads must be between 1 and 2048".into(),
            ));
        }
        if self.proxy_types.is_empty() {
            return Err(ConfigError::Invalid(
                "proxy_types must contain at least one type".into(),
            ));
        }
        if self.slow_threshold_ms >= self.max_timeout_ms {
            return Err(ConfigError::Invalid(
                "slow_threshold_ms must be lower than max_timeout_ms".into(),
            ));
        }
        if self.max_proxies_per_source == 0 || self.max_total_proxies == 0 {
            return Err(ConfigError::Invalid(
                "proxy limits must be greater than zero".into(),
            ));
        }

        let probe = Url::parse(&self.probe_url)
            .map_err(|error| ConfigError::Invalid(format!("probe_url is invalid: {error}")))?;
        validate_http_url(&probe, "probe_url")?;
        validate_public_probe_host(&probe)?;

        for source in &self.sources {
            let parsed = Url::parse(&source.url).map_err(|error| {
                ConfigError::Invalid(format!(
                    "source {} has an invalid URL: {error}",
                    source.name
                ))
            })?;
            validate_http_url(&parsed, &format!("source {}", source.name))?;
        }

        Ok(())
    }

    pub fn accepts_type(&self, proxy_type: ProxyType) -> bool {
        self.proxy_types.contains(&proxy_type)
    }
}

fn validate_http_url(url: &Url, label: &str) -> Result<(), ConfigError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ConfigError::Invalid(format!(
            "{label} must use http or https"
        )));
    }
    if url.host_str().is_none() {
        return Err(ConfigError::Invalid(format!("{label} must contain a host")));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::Invalid(format!(
            "{label} must not contain credentials"
        )));
    }
    Ok(())
}

fn validate_public_probe_host(url: &Url) -> Result<(), ConfigError> {
    let host = url.host_str().unwrap_or_default();
    let normalized = host.to_ascii_lowercase();
    if matches!(normalized.as_str(), "localhost" | "localhost.localdomain")
        || normalized.ends_with(".local")
        || normalized.ends_with(".internal")
    {
        return Err(ConfigError::Invalid(
            "probe_url must target a public host, not a local name".into(),
        ));
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        if is_private_or_local(address) {
            return Err(ConfigError::Invalid(
                "probe_url must target a public IP address".into(),
            ));
        }
    }
    Ok(())
}

fn is_private_or_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            octets[0] == 10
                || octets[0] == 127
                || (octets[0] == 169 && octets[1] == 254)
                || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 168)
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn default_max_timeout_ms() -> u64 {
    10_000
}
fn default_source_timeout_ms() -> u64 {
    10_000
}
fn default_threads() -> usize {
    64
}
fn default_proxy_types() -> Vec<ProxyType> {
    ProxyType::all().to_vec()
}
fn default_slow_threshold_ms() -> u64 {
    2_000
}
fn default_probe_url() -> String {
    "https://example.com/".to_string()
}
fn default_history_file() -> PathBuf {
    PathBuf::from("history.txt")
}
fn default_input_file() -> PathBuf {
    PathBuf::from("proxies.txt")
}
fn default_results_dir() -> PathBuf {
    PathBuf::from("results")
}
fn default_max_proxies_per_source() -> usize {
    10_000
}
fn default_max_total_proxies() -> usize {
    50_000
}
fn default_user_agent() -> String {
    "InstaProxy/0.1 (public proxy health checker)".to_string()
}
fn default_sources_file() -> Option<PathBuf> {
    Some(PathBuf::from("source.txt"))
}
fn default_source_enabled() -> bool {
    true
}

fn default_sources() -> Vec<SourceConfig> {
    Vec::new()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_timeout_ms: default_max_timeout_ms(),
            source_timeout_ms: default_source_timeout_ms(),
            threads: default_threads(),
            proxy_types: default_proxy_types(),
            slow_threshold_ms: default_slow_threshold_ms(),
            probe_url: default_probe_url(),
            check_history: false,
            history_file: default_history_file(),
            input_file: default_input_file(),
            results_dir: default_results_dir(),
            max_proxies_per_source: default_max_proxies_per_source(),
            max_total_proxies: default_max_total_proxies(),
            user_agent: default_user_agent(),
            sources_file: default_sources_file(),
            sources: default_sources(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_file_does_not_enable_hidden_sources() {
        let config: Config = serde_yaml::from_str("sources_file: source.txt\n").unwrap();
        assert!(config.sources.is_empty());
    }

    #[test]
    fn source_file_path_is_relative_to_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom.yaml");
        fs::write(&path, "sources_file: lists.txt\nsources: []\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(
            config.sources_file,
            Some(directory.path().join("lists.txt"))
        );
    }

    #[test]
    fn relative_data_paths_are_resolved_beside_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom.yaml");
        fs::write(
            &path,
            "sources_file: lists/sources.txt\ninput_file: lists/input.txt\nhistory_file: saved.txt\nresults_dir: output\n",
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(
            config.sources_file,
            Some(directory.path().join("lists/sources.txt"))
        );
        assert_eq!(config.input_file, directory.path().join("lists/input.txt"));
        assert_eq!(config.history_file, directory.path().join("saved.txt"));
        assert_eq!(config.results_dir, directory.path().join("output"));
    }

    #[test]
    fn default_data_paths_are_resolved_beside_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom.yaml");
        fs::write(&path, "sources_file: null\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.sources_file, None);
        assert_eq!(config.input_file, directory.path().join("proxies.txt"));
        assert_eq!(config.history_file, directory.path().join("history.txt"));
        assert_eq!(config.results_dir, directory.path().join("results"));
    }

    #[test]
    fn absolute_data_paths_are_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let absolute = external.path().join("data");
        let quoted = serde_yaml::to_string(&absolute).unwrap();
        let quoted = quoted.trim();
        let path = directory.path().join("custom.yaml");
        fs::write(
            &path,
            format!("sources_file: {quoted}\ninput_file: {quoted}\nhistory_file: {quoted}\nresults_dir: {quoted}\n"),
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.sources_file.as_ref(), Some(&absolute));
        assert_eq!(config.input_file, absolute);
        assert_eq!(config.history_file, absolute);
        assert_eq!(config.results_dir, absolute);
    }

    #[test]
    fn defaults_are_usable() {
        let config = Config::default();
        assert_eq!(config.max_timeout_ms, 10_000);
        assert_eq!(config.threads, 64);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_non_http_probe_urls() {
        let config = Config {
            probe_url: "file:///tmp/test".into(),
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_local_probe_addresses() {
        let config = Config {
            probe_url: "http://127.0.0.1:8080/".into(),
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }
}
