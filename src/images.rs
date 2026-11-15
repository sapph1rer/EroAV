//! Cover loader (same idea as waku-app's images.rs): worker threads download with a Referer,
//! cache on disk, decode + downscale; the UI thread turns them into textures.

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

const WORKERS: usize = 6;
const MAX_W: u32 = 720;

enum Slot {
    Loading,
    Ready(TextureHandle),
    Failed,
}

pub struct Images {
    slots: HashMap<String, Slot>,
    req: Sender<(String, String)>,
    done: Receiver<(String, Result<ColorImage, String>)>,
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("EroAV")
}

fn disk_path(url: &str) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    data_dir().join("img").join(format!("{:016x}", h.finish()))
}

fn fetch(http: &reqwest::blocking::Client, url: &str, referer: &str) -> Result<ColorImage, String> {
    let path = disk_path(url);
    let bytes = match std::fs::read(&path) {
        Ok(b) if !b.is_empty() => b,
        _ => {
            let r = http.get(url).header("Referer", referer).send().map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(format!("HTTP {}", r.status()));
            }
            let b = r.bytes().map_err(|e| e.to_string())?.to_vec();
            if let Some(d) = path.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(&path, &b);
            b
        }
    };
    let img = image::load_from_memory(&bytes).map_err(|e| {
        let _ = std::fs::remove_file(&path);
        e.to_string()
    })?;
    let img = if img.width() > MAX_W { img.resize(MAX_W, MAX_W, image::imageops::FilterType::Triangle) } else { img };
    let rgba = img.to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

impl Images {
    pub fn new(ctx: &Context) -> Self {
        let (req_tx, req_rx) = channel::<(String, String)>();
        let (done_tx, done_rx) = channel();
        let req_rx = Arc::new(Mutex::new(req_rx));
        let mut http = reqwest::blocking::Client::builder();
        if let Some(p) = crate::dns::proxy_url().and_then(|u| reqwest::Proxy::all(u).ok()) {
            http = http.proxy(p); // names resolved over DNS-over-HTTPS (src/dns.rs)
        }
        let http = http
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36")
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("http client");
        for _ in 0..WORKERS {
            let (rx, tx, http, ctx) = (req_rx.clone(), done_tx.clone(), http.clone(), ctx.clone());
            std::thread::spawn(move || {
                loop {
                    let job = rx.lock().ok().and_then(|r| r.recv().ok());
                    let Some((url, referer)) = job else { break };
                    let res = fetch(&http, &url, &referer);
                    if tx.send((url, res)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            });
        }
        Self { slots: HashMap::new(), req: req_tx, done: done_rx }
    }

    /// Texture for `url`, starting the download the first time it is asked for.
    pub fn get(&mut self, url: &str, referer: &str) -> Option<&TextureHandle> {
        if !self.slots.contains_key(url) {
            self.slots.insert(url.to_string(), Slot::Loading);
            let _ = self.req.send((url.to_string(), referer.to_string()));
        }
        match self.slots.get(url) {
            Some(Slot::Ready(t)) => Some(t),
            _ => None,
        }
    }

    pub fn failed(&self, url: &str) -> bool {
        matches!(self.slots.get(url), Some(Slot::Failed))
    }

    /// Move finished downloads into textures (once per frame).
    pub fn pump(&mut self, ctx: &Context) {
        while let Ok((url, res)) = self.done.try_recv() {
            let slot = match res {
                Ok(img) => Slot::Ready(ctx.load_texture(url.clone(), img, TextureOptions::LINEAR)),
                Err(_) => Slot::Failed,
            };
            self.slots.insert(url, slot);
        }
    }
}
