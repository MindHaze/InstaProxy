use crate::{
    config::Config,
    model::{CheckResult, CheckStatus, ProxySpec, ProxyType},
};
use anyhow::{anyhow, Result};
use futures::{stream, Stream, StreamExt};
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{lookup_host, TcpStream},
    time::timeout,
};
use url::Url;

#[derive(Debug, Clone)]
struct ProbeTarget {
    host: String,
    port: u16,
    authority: String,
    url: String,
}

#[derive(Debug, Error)]
enum ProbeError {
    #[error("authentication required")]
    AuthRequired,
    #[error("{0}")]
    Failed(String),
}

impl ProbeTarget {
    fn from_url(value: &str) -> Result<Self> {
        let url = Url::parse(value)?;
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("probe_url must contain a host"))?
            .to_string();
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("probe_url must contain a known port"))?;
        let authority_host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.clone()
        };
        Ok(Self {
            host,
            port,
            authority: format!("{authority_host}:{port}"),
            url: value.to_string(),
        })
    }
}

pub fn check_stream(
    proxies: Vec<ProxySpec>,
    config: &Config,
) -> Result<impl Stream<Item = CheckResult> + Send + 'static> {
    let target = ProbeTarget::from_url(&config.probe_url)?;
    let max_timeout = Duration::from_millis(config.max_timeout_ms);
    let slow_threshold = Duration::from_millis(config.slow_threshold_ms);
    let concurrency = config.threads.max(1);

    Ok(stream::iter(proxies.into_iter().map(move |proxy| {
        let target = target.clone();
        async move { check_one(proxy, target, max_timeout, slow_threshold).await }
    }))
    .buffer_unordered(concurrency))
}

async fn check_one(
    proxy: ProxySpec,
    target: ProbeTarget,
    max_timeout: Duration,
    slow_threshold: Duration,
) -> CheckResult {
    let started = Instant::now();
    if proxy.auth_hint {
        return CheckResult {
            proxy,
            status: CheckStatus::AuthRequired,
            latency_ms: Some(0),
            detail: "credentials were present in the input; no credentials were attempted".into(),
        };
    }

    let transport = timeout(max_timeout, check_transport(&proxy, &target)).await;
    let elapsed = started.elapsed();
    let latency_ms = Some(elapsed.as_millis());
    match transport {
        Ok(Ok(detail)) => CheckResult {
            proxy,
            status: if elapsed > slow_threshold {
                CheckStatus::Slow
            } else {
                CheckStatus::Working
            },
            latency_ms,
            detail,
        },
        Ok(Err(ProbeError::AuthRequired)) => CheckResult {
            proxy,
            status: CheckStatus::AuthRequired,
            latency_ms,
            detail: "the proxy requested authentication".into(),
        },
        Ok(Err(ProbeError::Failed(detail))) => CheckResult {
            proxy,
            status: CheckStatus::Dead,
            latency_ms,
            detail,
        },
        Err(_) => CheckResult {
            proxy,
            status: CheckStatus::Timeout,
            latency_ms,
            detail: format!("timed out after {} ms", max_timeout.as_millis()),
        },
    }
}

async fn check_transport(proxy: &ProxySpec, target: &ProbeTarget) -> Result<String, ProbeError> {
    match proxy.proxy_type {
        ProxyType::Http | ProxyType::Https => check_http_proxy(proxy, target).await,
        ProxyType::Socks4 => check_socks4(proxy, target).await,
        ProxyType::Socks5 => check_socks5(proxy, target).await,
    }
}

async fn connect_proxy(proxy: &ProxySpec) -> Result<TcpStream, ProbeError> {
    TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .map_err(|error| ProbeError::Failed(format!("TCP connection failed: {error}")))
}

async fn check_http_proxy(proxy: &ProxySpec, target: &ProbeTarget) -> Result<String, ProbeError> {
    let connect_status = http_exchange(proxy, target, true).await?;
    if connect_status == 407 {
        return Err(ProbeError::AuthRequired);
    }
    if (200..300).contains(&connect_status) {
        return Ok(format!("CONNECT returned HTTP {connect_status}"));
    }

    let get_status = http_exchange(proxy, target, false).await?;
    if get_status == 407 {
        return Err(ProbeError::AuthRequired);
    }
    if (200..500).contains(&get_status) {
        Ok(format!("absolute GET returned HTTP {get_status}"))
    } else {
        Err(ProbeError::Failed(format!(
            "proxy returned HTTP {connect_status} to CONNECT and HTTP {get_status} to GET"
        )))
    }
}

async fn http_exchange(
    proxy: &ProxySpec,
    target: &ProbeTarget,
    connect_request: bool,
) -> Result<u16, ProbeError> {
    let mut stream = connect_proxy(proxy).await?;
    let request = if connect_request {
        format!(
            "CONNECT {} HTTP/1.1\r\nHost: {}\r\nProxy-Connection: close\r\nConnection: close\r\nUser-Agent: InstaProxy/0.1\r\n\r\n",
            target.authority, target.authority
        )
    } else {
        format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: InstaProxy/0.1\r\n\r\n",
            target.url, target.authority
        )
    };
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| ProbeError::Failed(format!("could not write HTTP request: {error}")))?;
    read_http_status(&mut stream).await
}

async fn read_http_status(stream: &mut TcpStream) -> Result<u16, ProbeError> {
    let mut buffer = Vec::with_capacity(512);
    let mut chunk = [0_u8; 512];
    while buffer.len() < 8 * 1024 {
        let count = stream.read(&mut chunk).await.map_err(|error| {
            ProbeError::Failed(format!("could not read HTTP response: {error}"))
        })?;
        if count == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..count]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let line = buffer
        .split(|byte| *byte == b'\n')
        .next()
        .ok_or_else(|| ProbeError::Failed("HTTP response was empty".into()))?;
    let line = std::str::from_utf8(line)
        .map_err(|_| ProbeError::Failed("HTTP response header is not valid UTF-8".into()))?;
    parse_http_status_line(line)
        .ok_or_else(|| ProbeError::Failed("HTTP response did not contain a valid status".into()))
}

fn parse_http_status_line(line: &str) -> Option<u16> {
    let mut parts = line.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let status = parts.next()?.parse::<u16>().ok()?;
    if (100..600).contains(&status) {
        Some(status)
    } else {
        None
    }
}

async fn check_socks5(proxy: &ProxySpec, target: &ProbeTarget) -> Result<String, ProbeError> {
    let mut stream = connect_proxy(proxy).await?;
    stream
        .write_all(&[5, 2, 0, 2])
        .await
        .map_err(|error| ProbeError::Failed(format!("could not write SOCKS5 greeting: {error}")))?;
    let mut greeting = [0_u8; 2];
    stream
        .read_exact(&mut greeting)
        .await
        .map_err(|error| ProbeError::Failed(format!("could not read SOCKS5 greeting: {error}")))?;
    if greeting[0] != 5 {
        return Err(ProbeError::Failed("proxy did not speak SOCKS5".into()));
    }
    match greeting[1] {
        0 => {}
        2 => return Err(ProbeError::AuthRequired),
        0xff => return Err(ProbeError::AuthRequired),
        method => {
            return Err(ProbeError::Failed(format!(
                "SOCKS5 selected unsupported method 0x{method:02x}"
            )))
        }
    }

    let host = target.host.as_bytes();
    if host.len() > u8::MAX as usize {
        return Err(ProbeError::Failed("probe host name is too long".into()));
    }
    let mut request = vec![5, 1, 0, 3, host.len() as u8];
    request.extend_from_slice(host);
    request.extend_from_slice(&target.port.to_be_bytes());
    stream
        .write_all(&request)
        .await
        .map_err(|error| ProbeError::Failed(format!("could not write SOCKS5 request: {error}")))?;

    let mut response = [0_u8; 4];
    stream
        .read_exact(&mut response)
        .await
        .map_err(|error| ProbeError::Failed(format!("could not read SOCKS5 response: {error}")))?;
    if response[0] != 5 {
        return Err(ProbeError::Failed(
            "SOCKS5 response has an invalid version".into(),
        ));
    }
    if response[1] != 0 {
        return Err(ProbeError::Failed(format!(
            "SOCKS5 CONNECT failed with reply 0x{:02x}",
            response[1]
        )));
    }
    read_socks5_bound_address(&mut stream, response[3]).await?;
    Ok("SOCKS5 CONNECT succeeded".into())
}

async fn read_socks5_bound_address(
    stream: &mut TcpStream,
    address_type: u8,
) -> Result<(), ProbeError> {
    let address_length = match address_type {
        1 => 4,
        4 => 16,
        3 => {
            let mut length = [0_u8; 1];
            stream.read_exact(&mut length).await.map_err(|error| {
                ProbeError::Failed(format!("could not read SOCKS5 domain length: {error}"))
            })?;
            length[0] as usize
        }
        other => {
            return Err(ProbeError::Failed(format!(
                "SOCKS5 returned unsupported address type 0x{other:02x}"
            )))
        }
    };
    let mut address_and_port = vec![0_u8; address_length + 2];
    stream
        .read_exact(&mut address_and_port)
        .await
        .map_err(|error| {
            ProbeError::Failed(format!("could not read SOCKS5 bound address: {error}"))
        })?;
    Ok(())
}

async fn check_socks4(proxy: &ProxySpec, target: &ProbeTarget) -> Result<String, ProbeError> {
    let target_ip = resolve_ipv4(&target.host, target.port).await?;
    let mut stream = connect_proxy(proxy).await?;
    let mut request = vec![4, 1];
    request.extend_from_slice(&target.port.to_be_bytes());
    request.extend_from_slice(&target_ip.octets());
    request.push(0);
    stream
        .write_all(&request)
        .await
        .map_err(|error| ProbeError::Failed(format!("could not write SOCKS4 request: {error}")))?;
    let mut response = [0_u8; 8];
    stream
        .read_exact(&mut response)
        .await
        .map_err(|error| ProbeError::Failed(format!("could not read SOCKS4 response: {error}")))?;
    if response[1] == 90 {
        Ok("SOCKS4 CONNECT succeeded".into())
    } else {
        Err(ProbeError::Failed(format!(
            "SOCKS4 CONNECT failed with reply {}",
            response[1]
        )))
    }
}

async fn resolve_ipv4(host: &str, port: u16) -> Result<std::net::Ipv4Addr, ProbeError> {
    if let Ok(address) = host.parse::<std::net::IpAddr>() {
        return match address {
            std::net::IpAddr::V4(ipv4) => Ok(ipv4),
            std::net::IpAddr::V6(_) => Err(ProbeError::Failed(
                "SOCKS4 requires an IPv4 probe target".into(),
            )),
        };
    }
    let addresses = lookup_host((host, port))
        .await
        .map_err(|error| ProbeError::Failed(format!("could not resolve probe host: {error}")))?;
    addresses
        .filter_map(|address: SocketAddr| match address {
            SocketAddr::V4(ipv4) => Some(*ipv4.ip()),
            SocketAddr::V6(_) => None,
        })
        .next()
        .ok_or_else(|| ProbeError::Failed("probe host has no IPv4 address".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_status_lines() {
        assert_eq!(
            parse_http_status_line("HTTP/1.1 200 Connection established\r"),
            Some(200)
        );
        assert_eq!(
            parse_http_status_line("HTTP/1.1 407 Proxy Authentication Required"),
            Some(407)
        );
        assert_eq!(parse_http_status_line("not HTTP"), None);
    }

    #[test]
    fn builds_ipv6_probe_authority() {
        let target = ProbeTarget::from_url("http://[2001:db8::1]:8080/test").unwrap();
        assert_eq!(target.authority, "[2001:db8::1]:8080");
    }
}
