use std::{fs, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::oneshot,
    time::timeout,
};

async fn read_headers(stream: &mut TcpStream) {
    let mut request = Vec::new();
    let mut byte = [0; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        request.push(byte[0]);
        assert!(request.len() < 8192);
    }
}

async fn assert_live_check(command: &str) {
    let directory = tempfile::tempdir().unwrap();
    let fast = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fast_address = fast.local_addr().unwrap();
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled_address = stalled.local_addr().unwrap();
    let (release, wait) = oneshot::channel();
    let fast_server = tokio::spawn(async move {
        let (mut stream, _) = fast.accept().await.unwrap();
        read_headers(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
    });
    let stalled_server = tokio::spawn(async move {
        let (mut stream, _) = stalled.accept().await.unwrap();
        read_headers(&mut stream).await;
        let _ = wait.await;
        // Closing without a response produces DEAD, not TIMEOUT.
    });
    let proxies = format!(
        "http://{fast_address}\nhttp://{stalled_address}\nhttp://placeholder@auth.invalid:8080\n"
    );
    fs::write(directory.path().join("proxies.txt"), &proxies).unwrap();
    let mut yaml = String::from("sources_file: null\ncheck_history: false\nmax_timeout_ms: 5000\nslow_threshold_ms: 2000\nthreads: 3\n");
    let source_server = if command == "scrape-check" {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        yaml.push_str(&format!(
            "sources:\n  - name: Fixture\n    url: http://{address}/http.txt\n"
        ));
        Some(tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{proxies}",
                proxies.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }))
    } else {
        None
    };
    fs::write(directory.path().join("config.yaml"), yaml).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_instaproxy"))
        .current_dir(directory.path())
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut transcript = String::new();
    timeout(Duration::from_secs(2), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            transcript.push_str(&line);
            transcript.push('\n');
            if line.starts_with("[ALIVE]") && line.contains(&fast_address.to_string()) {
                return;
            }
        }
        panic!("No ALIVE result was printed: {transcript}");
    })
    .await
    .expect("ALIVE must be logged while another proxy is still waiting");
    assert!(
        child.try_wait().unwrap().is_none(),
        "Check finished before the stalled proxy was released"
    );
    release.send(()).unwrap();
    timeout(Duration::from_secs(3), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            transcript.push_str(&line);
            transcript.push('\n');
        }
        let output = child.wait_with_output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    })
    .await
    .unwrap();
    assert_eq!(transcript.matches("[ALIVE]").count(), 1, "{transcript}");
    assert_eq!(transcript.matches("[DEAD]").count(), 1, "{transcript}");
    assert_eq!(
        transcript.matches("[AUTH REQUIRED]").count(),
        1,
        "{transcript}"
    );
    assert!(
        transcript.contains("Alive: 1 | Dead: 1 | Timeout: 0 | Remaining: 0"),
        "{transcript}"
    );
    assert!(
        !transcript.contains('\u{1b}'),
        "Redirected output must not contain terminal controls"
    );
    let run = fs::read_dir(directory.path().join("results"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let saved = fs::read_to_string(run.join("http.txt")).unwrap();
    assert_eq!(saved, format!("http://{fast_address}\n"));
    if command == "scrape-check" {
        assert_eq!(
            fs::read_to_string(run.join("scraped_http.txt")).unwrap(),
            saved
        );
    }
    fast_server.await.unwrap();
    stalled_server.await.unwrap();
    if let Some(server) = source_server {
        server.await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn check_logs_results_before_the_batch_finishes() {
    assert_live_check("check").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scrape_check_logs_results_before_the_batch_finishes() {
    assert_live_check("scrape-check").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_has_its_own_log_counter_and_summary_and_is_not_saved() {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_headers(&mut stream).await;
        let mut byte = [0];
        let _ = stream.read(&mut byte).await;
    });
    fs::write(
        directory.path().join("proxies.txt"),
        format!("http://{address}\n"),
    )
    .unwrap();
    fs::write(
        directory.path().join("config.yaml"),
        "sources_file: null\nmax_timeout_ms: 2600\nslow_threshold_ms: 100\n",
    )
    .unwrap();
    let output = timeout(
        Duration::from_secs(4),
        Command::new(env!("CARGO_BIN_EXE_instaproxy"))
            .current_dir(directory.path())
            .arg("check")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("[TIMEOUT]"), "{stdout}");
    assert!(!stdout.contains("[DEAD]"), "{stdout}");
    let pending_updates = stdout
        .lines()
        .filter(|line| line.starts_with("[PROGRESS]") && line.contains("Remaining: 1"))
        .count();
    assert!(
        pending_updates >= 2,
        "A heartbeat must be logged before the stalled check times out: {stdout}"
    );
    assert!(
        stdout.contains("Alive: 0 | Dead: 0 | Timeout: 1 | Remaining: 0"),
        "{stdout}"
    );
    let run = fs::read_dir(directory.path().join("results"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(fs::read_to_string(run.join("http.txt")).unwrap().is_empty());
    assert!(fs::read_to_string(directory.path().join("history.txt"))
        .unwrap()
        .is_empty());
    let summary = fs::read_to_string(run.join("summary.txt")).unwrap();
    assert!(
        summary.contains("http: working=0, slow=0, dead=0, timeout=1, auth_required=0"),
        "{summary}"
    );
    server.await.unwrap();
}
