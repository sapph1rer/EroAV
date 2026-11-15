//! Client for the `eroav-browser` helper (hidden WebView2 window, src/bin/eroav-browser.rs).
//! Trimmed copy of waku-core's browser.rs: started on first use, one request at a time, one JSON line per answer.
//! The helper shows its window by itself when Cloudflare wants a click.
//! One helper per site, each with its own profile: a helper that stays on one site answers from inside the
//! open page (fast fetch) instead of navigating back and forth, and both sites load at the same time.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

type Result<T> = std::result::Result<T, String>;

struct Proc {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next: u64,
}

pub struct Browser {
    exe: PathBuf,
    /// WebView2 profile folder (cookies, Cloudflare clearance)
    data: PathBuf,
    p: Mutex<Option<Proc>>,
}

/// Helper number `slot` (one per site), if the helper exe is next to the running exe.
pub fn get(slot: usize) -> Result<&'static Browser> {
    static B: OnceLock<Vec<Browser>> = OnceLock::new();
    let all = B.get_or_init(|| {
        let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_path_buf())) else { return vec![] };
        let exe = dir.join(if cfg!(windows) { "eroav-browser.exe" } else { "eroav-browser" });
        if !exe.exists() {
            return vec![];
        }
        let base = crate::images::data_dir();
        (0..crate::source::SOURCES.len())
            .map(|i| Browser {
                exe: exe.clone(),
                // slot 0 keeps the folder older versions used (and its Cloudflare clearance)
                data: base.join(if i == 0 { "browser".to_string() } else { format!("browser-{i}") }),
                p: Mutex::new(None),
            })
            .collect()
    });
    all.get(slot).ok_or_else(|| "eroav-browser.exe is missing next to eroav.exe".to_string())
}

impl Browser {
    fn start(&self) -> Result<Proc> {
        let mut cmd = Command::new(&self.exe);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).env("EROAV_BROWSER_DATA", &self.data);
        if let Some(p) = crate::dns::proxy_url() {
            cmd.env("EROAV_PROXY", p); // the helper resolves names through EroAV's DNS-over-HTTPS proxy
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| format!("starting browser helper: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && tx.send(v).is_err()
                {
                    break;
                }
            }
        });
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(v) if v.get("ready").and_then(Value::as_bool) == Some(true) => {}
            Ok(v) => return Err(format!("browser helper failed: {}", v.get("error").and_then(Value::as_str).unwrap_or("?"))),
            Err(_) => {
                let _ = child.kill();
                return Err("browser helper did not start (is the WebView2 runtime installed?)".into());
            }
        }
        Ok(Proc { child, stdin, rx, next: 1 })
    }

    fn call(&self, mut req: Value, timeout: Duration) -> Result<Value> {
        let mut g = self.p.lock().map_err(|_| "browser lock poisoned")?;
        if !g.as_mut().is_some_and(|p| p.child.try_wait().ok().flatten().is_none()) {
            *g = Some(self.start()?);
        }
        let p = g.as_mut().unwrap();
        let id = p.next;
        p.next += 1;
        req["id"] = json!(id);
        writeln!(p.stdin, "{req}").map_err(|e| format!("writing to browser helper: {e}"))?;
        let _ = p.stdin.flush();
        loop {
            match p.rx.recv_timeout(timeout) {
                Ok(v) if v.get("id").and_then(Value::as_u64) == Some(id) => {
                    if v.get("ok").and_then(Value::as_bool) == Some(true) {
                        return Ok(v);
                    }
                    return Err(v.get("error").and_then(Value::as_str).unwrap_or("browser error").to_string());
                }
                Ok(_) => continue, // late answer to an earlier, timed-out request
                Err(_) => {
                    let _ = p.child.kill();
                    *g = None;
                    return Err("browser helper timed out".into());
                }
            }
        }
    }

    /// Page HTML after scripts and the Cloudflare check.
    pub fn fetch(&self, url: &str) -> Result<String> {
        // 3 minutes: the Cloudflare checkbox waits for a human in the helper's window
        let v = self.call(json!({"op": "fetch", "url": url, "timeout": 180}), Duration::from_secs(200)).map_err(|e| {
            if e.contains("browser check") {
                "Cloudflare check not done. Tick \"Verify you are human\" in the EroAV verification window, then press Try again.".to_string()
            } else {
                e
            }
        })?;
        let status = v.get("status").and_then(Value::as_u64).unwrap_or(200);
        if status >= 400 {
            return Err(format!("GET {url} -> HTTP {status}"));
        }
        Ok(v.get("html").and_then(Value::as_str).unwrap_or("").to_string())
    }

    /// Video URLs the page's player requests (fallback when the link cannot be read from the HTML).
    pub fn sniff(&self, url: &str) -> Result<Vec<String>> {
        let v = self.call(json!({"op": "sniff", "url": url, "referer": url, "timeout": 25}), Duration::from_secs(60))?;
        Ok(v.get("urls").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default())
    }

}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Ok(mut g) = self.p.lock()
            && let Some(p) = g.as_mut()
        {
            let _ = writeln!(p.stdin, "{}", json!({"op": "quit"}));
        }
    }
}
