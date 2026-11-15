//! Local proxy that looks host names up over DNS-over-HTTPS.
//!
//! Some ISPs' DNS never answers for these sites (lookups time out), which made every page, cover and stream hang.
//! Everything EroAV loads goes through this proxy instead: the browser helper (WebView2 `--proxy-server`), the
//! cover downloader (reqwest) and mpv (`http-proxy`). TLS stays end to end: HTTPS goes through `CONNECT`, the
//! proxy only picks the address. No Windows setting is touched.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// `http://127.0.0.1:<port>` of the running proxy (started on first call).
pub fn proxy_url() -> Option<String> {
    static URL: OnceLock<Option<String>> = OnceLock::new();
    URL.get_or_init(|| {
        let l = TcpListener::bind("127.0.0.1:0").ok()?;
        let port = l.local_addr().ok()?.port();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                std::thread::spawn(move || {
                    let _ = handle(c);
                });
            }
        });
        Some(format!("http://127.0.0.1:{port}"))
    })
    .clone()
}

fn handle(client: TcpStream) -> std::io::Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut rd = BufReader::new(client.try_clone()?);
    let mut head = Vec::new();
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let end = line == "\r\n" || line == "\n";
        head.push(line);
        if end || head.len() > 100 {
            break;
        }
    }
    let first: Vec<&str> = head[0].split_whitespace().collect();
    let (method, target) = (first.first().copied().unwrap_or(""), first.get(1).copied().unwrap_or(""));
    let mut client = client;
    let (host, port, connect) = if method.eq_ignore_ascii_case("CONNECT") {
        let (h, p) = target.rsplit_once(':').unwrap_or((target, "443"));
        (h.trim_matches(['[', ']']).to_string(), p.parse().unwrap_or(443), true)
    } else {
        // plain http: "GET http://host[:port]/path HTTP/1.1" (servers accept the absolute form as is)
        let rest = target.strip_prefix("http://").unwrap_or(target);
        let hp = rest.split('/').next().unwrap_or("");
        let (h, p) = hp.rsplit_once(':').map_or((hp, 80), |(h, p)| (h, p.parse().unwrap_or(80)));
        (h.to_string(), p, false)
    };
    let upstream = match connect_to(&host, port) {
        Ok(s) => s,
        Err(e) => {
            let _ = client.write_all(format!("HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nX-Error: {e}\r\n\r\n").as_bytes());
            return Ok(());
        }
    };
    client.set_read_timeout(None)?;
    let mut up_w = upstream.try_clone()?;
    if connect {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    } else {
        up_w.write_all(head.concat().as_bytes())?;
    }
    // bytes the reader already buffered belong to the upstream
    let buffered = rd.buffer().to_vec();
    if !buffered.is_empty() {
        up_w.write_all(&buffered)?;
    }
    let mut up_r = upstream;
    let mut client_w = client.try_clone()?;
    let t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut up_r, &mut client_w);
        let _ = client_w.shutdown(std::net::Shutdown::Write);
    });
    let mut client_r = rd.into_inner();
    let _ = std::io::copy(&mut client_r, &mut up_w);
    let _ = up_w.shutdown(std::net::Shutdown::Write);
    let _ = t.join();
    Ok(())
}

fn connect_to(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut last = std::io::Error::other(format!("{host}: no address"));
    for ip in resolve(host) {
        match TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_secs(10)) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Addresses of `host`: DNS-over-HTTPS first (Cloudflare, then Google), the system resolver last. Cached 10 min.
pub fn resolve(host: &str) -> Vec<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return vec![ip];
    }
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Vec<IpAddr>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((t, v)) = cache.lock().ok().and_then(|c| c.get(host).cloned())
        && t.elapsed() < Duration::from_secs(600)
    {
        return v;
    }
    let mut v = doh(host);
    if v.is_empty() {
        v = (host, 0).to_socket_addrs().map(|a| a.map(|s| s.ip()).collect()).unwrap_or_default();
    }
    if !v.is_empty()
        && let Ok(mut c) = cache.lock()
    {
        c.insert(host.to_string(), (Instant::now(), v.clone()));
    }
    v
}

fn doh(host: &str) -> Vec<IpAddr> {
    static HTTP: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    let Some(http) = HTTP.get_or_init(|| reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().ok()) else {
        return vec![];
    };
    // servers addressed by IP: asking them needs no DNS of its own
    for server in ["https://1.1.1.1/dns-query", "https://8.8.8.8/resolve"] {
        let r = http.get(server).query(&[("name", host), ("type", "A")]).header("accept", "application/dns-json").send();
        let Ok(j) = r.and_then(|r| r.bytes()).map(|b| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default()) else { continue };
        let ips = parse_doh(&j);
        if !ips.is_empty() {
            return ips;
        }
    }
    vec![]
}

/// IPv4 addresses in a DNS JSON answer (CNAME entries are skipped: their "data" is a name).
fn parse_doh(j: &serde_json::Value) -> Vec<IpAddr> {
    j.get("Answer")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|x| x.get("data")?.as_str()?.parse().ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn doh_answer() {
        let j = serde_json::json!({"Status":0,"Answer":[{"name":"a.com","type":5,"data":"b.com."},{"name":"b.com","type":1,"data":"104.26.6.107"}]});
        assert_eq!(super::parse_doh(&j), vec!["104.26.6.107".parse::<std::net::IpAddr>().unwrap()]);
    }
}
