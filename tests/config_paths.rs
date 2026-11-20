use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};
use tempfile::TempDir;

struct Installation {
    root: TempDir,
    executable: PathBuf,
}

impl Installation {
    fn new(relative_bin_dir: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join(relative_bin_dir);
        fs::create_dir_all(&directory).unwrap();
        let executable = directory.join(format!("instaproxy{}", std::env::consts::EXE_SUFFIX));
        fs::copy(env!("CARGO_BIN_EXE_instaproxy"), &executable).unwrap();
        Self { root, executable }
    }

    fn write_config(directory: &Path) {
        fs::write(
            directory.join("config.yaml"),
            "sources_file: source.txt\nsources: []\n",
        )
        .unwrap();
        fs::write(
            directory.join("source.txt"),
            "# Empty source list for offline testing\n",
        )
        .unwrap();
    }

    fn run_menu(&self, cwd: &Path, arguments: &[&str]) -> Output {
        let mut child = Command::new(&self.executable)
            .current_dir(cwd)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"0\n").unwrap();
        child.wait_with_output().unwrap()
    }
}

#[test]
fn release_directory_launch_finds_project_config_and_shows_menu() {
    let installation = Installation::new("target/release");
    Installation::write_config(installation.root.path());
    let output = installation.run_menu(installation.executable.parent().unwrap(), &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Choose an operation:"));
}

#[test]
fn launch_from_unrelated_directory_keeps_sources_and_results_beside_config() {
    let installation = Installation::new("target/release");
    Installation::write_config(installation.root.path());
    let caller = tempfile::tempdir().unwrap();
    let output = Command::new(&installation.executable)
        .current_dir(caller.path())
        .arg("scrape")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(installation.root.path().join("results").is_dir());
    assert!(!caller.path().join("results").exists());
    assert!(!installation
        .executable
        .parent()
        .unwrap()
        .join("results")
        .exists());
}

#[test]
fn standalone_executable_finds_adjacent_config() {
    let installation = Installation::new("portable");
    Installation::write_config(installation.executable.parent().unwrap());
    let caller = tempfile::tempdir().unwrap();
    let output = installation.run_menu(caller.path(), &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Choose an operation:"));
}

#[test]
fn explicit_missing_config_does_not_silently_fall_back() {
    let installation = Installation::new("target/release");
    Installation::write_config(installation.root.path());
    let output = Command::new(&installation.executable)
        .current_dir(installation.root.path())
        .args(["--config", "missing.yaml", "scrape"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing.yaml"));
    assert!(!installation.root.path().join("results").exists());
}

#[test]
fn explicit_config_takes_precedence_over_current_directory_config() {
    let installation = Installation::new("target/release");
    Installation::write_config(installation.root.path());
    fs::write(
        installation.root.path().join("custom.yaml"),
        "max_timeout_ms: 0\n",
    )
    .unwrap();
    let output = Command::new(&installation.executable)
        .current_dir(installation.root.path())
        .args(["--config", "custom.yaml", "scrape"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("max_timeout_ms"));
    assert!(!installation.root.path().join("results").exists());
}

#[test]
fn missing_default_config_reports_searched_paths_and_override() {
    let installation = Installation::new("portable");
    let caller = tempfile::tempdir().unwrap();
    let output = Command::new(&installation.executable)
        .current_dir(caller.path())
        .arg("scrape")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Use --config <PATH>"), "{stderr}");
    assert!(
        stderr.contains(caller.path().join("config.yaml").to_str().unwrap()),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            installation
                .executable
                .parent()
                .unwrap()
                .join("config.yaml")
                .to_str()
                .unwrap()
        ),
        "{stderr}"
    );
}

#[test]
fn current_directory_config_takes_precedence_over_executable_config() {
    let installation = Installation::new("portable");
    Installation::write_config(installation.executable.parent().unwrap());
    let caller = tempfile::tempdir().unwrap();
    fs::write(caller.path().join("config.yaml"), "max_timeout_ms: 0\n").unwrap();
    let output = Command::new(&installation.executable)
        .current_dir(caller.path())
        .arg("scrape")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("max_timeout_ms"));
}
