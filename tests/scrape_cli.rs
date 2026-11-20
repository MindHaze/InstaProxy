use std::{fs, process::Command, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scrape_reads_source_file_deduplicates_urls_and_skips_unavailable_sources() {
    let directory = tempfile::tempdir().unwrap();
    let settings = directory.path().join("settings");
    fs::create_dir(&settings).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
                assert!(request.len() < 8192);
            }
            let request = String::from_utf8(request).unwrap();
            let (status, body) = if request.starts_with("GET /socks5.txt ") {
                ("200 OK", "192.0.2.10:1080\n192.0.2.10:1080\n")
            } else {
                ("404 Not Found", "192.0.2.11:8080\n")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });

    fs::write(settings.join("source.txt"), format!(
        "\u{feff}# Raw URLs\r\n\r\nhttp://{address}/socks5.txt\r\nhttp://{address}/socks5.txt\r\nhttp://{address}/http.txt\r\n"
    )).unwrap();
    fs::write(
        settings.join("config.yaml"),
        "sources_file: source.txt\nsources: []\nsource_timeout_ms: 1000\nresults_dir: results\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_instaproxy"))
        .current_dir(directory.path())
        .args(["--config", "settings/config.yaml", "scrape"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout.matches("[SOURCE OK]").count(), 1, "{stdout}");
    assert_eq!(stdout.matches("[SOURCE DOWN]").count(), 1, "{stdout}");
    assert!(
        stdout.contains(&format!("[SOURCE OK] http://{address}/socks5.txt")),
        "{stdout}"
    );
    assert!(stdout.contains("source.txt:3"), "{stdout}");
    assert!(
        !stdout.contains(settings.join("source.txt").to_str().unwrap()),
        "{stdout}"
    );
    assert!(!directory.path().join("results").exists());
    let runs: Vec<_> = fs::read_dir(settings.join("results"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        fs::read_to_string(runs[0].join("scraped_socks5.txt")).unwrap(),
        "socks5://192.0.2.10:1080\n"
    );
    assert!(fs::read_to_string(runs[0].join("scraped_http.txt"))
        .unwrap()
        .is_empty());
    assert!(!directory.path().join("history.txt").exists());
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}
