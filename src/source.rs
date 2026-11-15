//! MissAV mirrors: listing pages, search, video page, stream link.
//! Pages come through the browser helper (Cloudflare), parsing is plain regex over the HTML.

use regex::Regex;
use std::sync::LazyLock;

pub struct Source {
    pub name: &'static str,
    /// site + tracking prefix + language, without the trailing slash
    pub base: &'static str,
}

pub static SOURCES: &[Source] = &[
    Source { name: "MissAV (EN)", base: "https://missav.live/dm539/en" },
    Source { name: "MissAV123 (CN)", base: "https://missav123.com/dm247/cn" },
];

/// Lists the Discover feed walks through, one after another, as the user scrolls.
pub const DISCOVER: &[&str] = &["weekly-hot", "monthly-hot", "chinese-subtitle", "uncensored-leak", "today-hot", "release"];

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Video {
    pub code: String,
    pub title: String,
    pub url: String,
    pub cover: String,
    pub duration: String,
    /// index into SOURCES of the site it came from
    #[serde(default)]
    pub src: usize,
}

impl Video {
    pub fn source(&self) -> &'static Source {
        &SOURCES[self.src.min(SOURCES.len() - 1)]
    }

    pub fn referer(&self) -> String {
        format!("{}/", self.source().origin())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Detail {
    pub video: Video,
    pub description: String,
    /// "Release date: 2024-01-01", "Actress: …" lines
    pub info: Vec<Info>,
    pub stream: Option<String>,
}

/// One line of the video page: "Genre: Pretty Girl, Petite". `links` are (name, list path under the site's base),
/// e.g. ("Pretty Girl", "genres/Pretty%20Girl").
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub key: String,
    pub value: String,
    pub links: Vec<(String, String)>,
}

/// Search sort options: (query value, label). "" = the site's default (relevance).
pub const SORTS: &[(&str, &str)] = &[
    ("", "Relevance"),
    ("released_at", "Release date"),
    ("published_at", "Recently added"),
    ("saved", "Most saved"),
    ("today_views", "Today's views"),
    ("weekly_views", "Weekly views"),
    ("monthly_views", "Monthly views"),
    ("views", "All-time views"),
];

/// Search filters: (query value, label).
pub const FILTERS: &[(&str, &str)] = &[
    ("", "All"),
    ("individual", "Single actress"),
    ("jav", "Japan AV"),
    ("asiaav", "Asia AV"),
    ("uncensored-leak", "Uncensored leak"),
    ("uncensored", "Uncensored"),
    ("english-subtitle", "English subtitle"),
];

/// `path` with the site's sort / filter query added ("" leaves one out).
pub fn with_params(path: &str, sort: &str, filter: &str) -> String {
    let mut q: Vec<String> = Vec::new();
    if !filter.is_empty() {
        q.push(format!("filters={filter}"));
    }
    if !sort.is_empty() {
        q.push(format!("sort={sort}"));
    }
    if q.is_empty() { path.to_string() } else { format!("{path}?{}", q.join("&")) }
}

impl Source {
    /// position in SOURCES (= its browser helper slot)
    pub fn index(&self) -> usize {
        SOURCES.iter().position(|s| std::ptr::eq(s, self)).unwrap_or(0)
    }

    /// language segment of the base ("en", "cn")
    pub fn lang(&self) -> &str {
        self.base.rsplit('/').next().unwrap_or("")
    }

    pub fn origin(&self) -> &str {
        let after = self.base.find("//").map_or(0, |i| i + 2);
        self.base[after..].find('/').map_or(self.base, |i| &self.base[..after + i])
    }

    /// `path` is a list under the site's base: "new", "weekly-hot", "search/<term>".
    pub fn list_url(&self, path: &str, page: u32) -> String {
        let url = format!("{}/{path}", self.base);
        let sep = if url.contains('?') { '&' } else { '?' };
        if page > 1 { format!("{url}{sep}page={page}") } else { url }
    }

    pub fn list(&self, path: &str, page: u32) -> Result<Vec<Video>, String> {
        let html = crate::browser::get(self.index())?.fetch(&self.list_url(path, page))?;
        let mut v = parse_list(&html);
        v.iter_mut().for_each(|x| x.src = self.index());
        if v.is_empty() && html.contains("Just a moment") {
            return Err("Cloudflare check was not passed".into());
        }
        Ok(v)
    }

    pub fn detail(&self, v: &Video) -> Result<Detail, String> {
        let b = crate::browser::get(self.index())?;
        let html = b.fetch(&v.url)?;
        let mut d = parse_detail(&html, v, self.lang());
        if d.stream.is_none() {
            // the player script changed: let the real page play and catch the playlist it loads
            d.stream = b.sniff(&v.url)?.into_iter().find(|u| u.contains(".m3u8"));
        }
        Ok(d)
    }
}

/// One step of a source test (Settings › Sources).
#[derive(Clone, Debug)]
pub struct Step {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

impl Source {
    /// newest list -> video page -> stream link -> playlist download, stopping at the first failure.
    pub fn test(&self) -> Vec<Step> {
        let mut out = Vec::new();
        let t = std::time::Instant::now();
        let mut step = |name, r: Result<String, String>| {
            let ok = r.is_ok();
            out.push(Step { name, ok, detail: r.unwrap_or_else(|e| e) });
            ok
        };
        let list = self.list("new", 1);
        let first = list.as_ref().ok().and_then(|v| v.first().cloned()).map(|mut v| {
            v.src = self.index();
            v
        });
        let r = match &list {
            Ok(v) if !v.is_empty() => Ok(format!("{} videos in {} ms", v.len(), t.elapsed().as_millis())),
            Ok(_) => Err("the page had no videos (layout changed?)".into()),
            Err(e) => Err(e.clone()),
        };
        if !step("List", r) {
            return out;
        }
        let v = first.unwrap();
        let t = std::time::Instant::now();
        let d = self.detail(&v);
        let ok = step("Video page", d.as_ref().map(|d| format!("{} in {} ms", d.video.code, t.elapsed().as_millis())).map_err(|e| e.clone()));
        let Some(stream) = d.ok().filter(|_| ok).and_then(|d| d.stream) else {
            if ok {
                step("Stream link", Err("no playlist link on the video page".into()));
            }
            return out;
        };
        step("Stream link", Ok(stream.clone()));
        let t = std::time::Instant::now();
        step("Stream", fetch_playlist(&stream, &v.referer()).map(|n| format!("playlist OK ({n} bytes) in {} ms", t.elapsed().as_millis())));
        out
    }
}

/// Download a playlist the way the player gets it: Referer + a browser User-Agent, through the DNS-over-HTTPS
/// proxy. Windows' own curl.exe, not reqwest: the host answers 403 to rustls' TLS handshake, while curl
/// (Schannel) and mpv get through.
fn fetch_playlist(url: &str, referer: &str) -> Result<usize, String> {
    const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0 Safari/537.36";
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-s", "-m", "20", "-A", UA, "-e", referer, "-w", "\n%{http_code}"]);
    if let Some(p) = crate::dns::proxy_url() {
        cmd.args(["-x", &p]);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.arg(url).output().map_err(|e| format!("curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_ref()));
    match code.trim() {
        "200" if body.starts_with("#EXTM3U") => Ok(body.len()),
        "200" => Err("the answer is not a playlist".into()),
        "000" => Err("no answer (network)".into()),
        c => Err(format!("HTTP {c}")),
    }
}

/// List path of a search ("" = newest releases).
pub fn list_path(query: &str) -> String {
    let q = query.trim();
    if q.is_empty() { "new".into() } else { format!("search/{}", encode(q)) }
}

/// The same list from every site (fetched at the same time, one helper each), interleaved, each video once
/// (a code both sites have keeps the first site's copy). Fails only when every site fails. A good result is
/// cached on disk, so the next start can show it at once (`cached`).
pub fn merged(path: &str, page: u32) -> Result<Vec<Video>, String> {
    let results: Vec<_> = std::thread::scope(|sc| {
        let jobs: Vec<_> = SOURCES
            .iter()
            .map(|s| {
                // one retry when the helper itself died or hung (it is restarted on the next call)
                sc.spawn(move || s.list(path, page).or_else(|e| if e.contains("helper") { s.list(path, page) } else { Err(e) }))
            })
            .collect();
        jobs.into_iter().map(|j| j.join().unwrap_or_else(|_| Err("crashed".into()))).collect()
    });
    let mut lists = Vec::new();
    let mut errors = Vec::new();
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Ok(mut v) => {
                v.iter_mut().for_each(|x| x.src = i);
                lists.push(v);
            }
            Err(e) => errors.push(format!("{}: {e}", SOURCES[i].name)),
        }
    }
    if lists.is_empty() {
        return Err(errors.join("\n"));
    }
    let v = interleave(lists);
    if !v.is_empty()
        && let Ok(json) = serde_json::to_vec(&v)
    {
        let f = cache_file(path, page);
        let _ = std::fs::create_dir_all(f.parent().unwrap());
        let _ = std::fs::write(f, json);
    }
    Ok(v)
}

/// The list `merged` got last time for this path and page.
pub fn cached(path: &str, page: u32) -> Option<Vec<Video>> {
    serde_json::from_slice(&std::fs::read(cache_file(path, page)).ok()?).ok()
}

fn cache_file(path: &str, page: u32) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, page).hash(&mut h);
    crate::images::data_dir().join("lists").join(format!("{:016x}.json", h.finish()))
}

fn interleave(lists: Vec<Vec<Video>>) -> Vec<Video> {
    let mut out: Vec<Video> = Vec::new();
    let mut its: Vec<_> = lists.into_iter().map(|l| l.into_iter()).collect();
    loop {
        let mut any = false;
        for it in its.iter_mut() {
            if let Some(v) = it.next() {
                any = true;
                match out.iter_mut().find(|o| o.code == v.code) {
                    // same video on both sites: keep the first site's copy (English titles), in the earlier slot
                    Some(o) if o.src > v.src => *o = v,
                    Some(_) => {}
                    None => out.push(v),
                }
            }
        }
        if !any {
            return out;
        }
    }
}

/// Percent-encode a search term for a path segment.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn rx(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

static TAG: LazyLock<Regex> = LazyLock::new(|| rx(r"(?s)<[^>]*>"));
static HREF: LazyLock<Regex> = LazyLock::new(|| rx(r#"href="([^"]+)""#));
static IMG: LazyLock<Regex> = LazyLock::new(|| rx(r#"<img[^>]*?(?:data-src|src)="(https?://[^"]+\.(?:jpe?g|webp|png)[^"]*)""#));
static ALT: LazyLock<Regex> = LazyLock::new(|| rx(r#"<img[^>]*?alt="([^"]*)""#));
static TITLE: LazyLock<Regex> = LazyLock::new(|| rx(r#"(?s)class="text-secondary[^"]*"[^>]*>(.*?)</a>"#));
static DUR: LazyLock<Regex> = LazyLock::new(|| rx(r">\s*(\d{1,2}:\d{2}(?::\d{2})?)\s*<"));
static H1: LazyLock<Regex> = LazyLock::new(|| rx(r"(?s)<h1[^>]*>(.*?)</h1>"));
static OG_IMG: LazyLock<Regex> = LazyLock::new(|| rx(r#"property="og:image"\s+content="([^"]+)""#));
static OG_DESC: LazyLock<Regex> = LazyLock::new(|| rx(r#"property="og:description"\s+content="([^"]*)""#));
static INFO: LazyLock<Regex> = LazyLock::new(|| rx(r#"(?s)<div class="text-secondary">(.*?)</div>"#));
static PACKED: LazyLock<Regex> = LazyLock::new(|| rx(r"(?s)eval\(function\(p,a,c,k,e,d\).*?\}\('(.*?)',(\d+),(\d+),'(.*?)'\.split\('\|'\)"));
static M3U8: LazyLock<Regex> = LazyLock::new(|| rx(r#"https?:(?:\\?/){2}[^'"\s]+?\.m3u8"#));

fn text(html: &str) -> String {
    let s = TAG.replace_all(html, " ");
    let s = s.replace("&amp;", "&").replace("&quot;", "\"").replace("&#039;", "'").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn parse_list(html: &str) -> Vec<Video> {
    let mut out: Vec<Video> = Vec::new();
    for chunk in html.split(r#"class="thumbnail group"#).skip(1) {
        let Some(url) = HREF.captures(chunk).map(|c| c[1].to_string()) else { continue };
        let code = url.trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_uppercase();
        let title = TITLE.captures(chunk).map(|c| text(&c[1])).filter(|t| !t.is_empty()).or_else(|| ALT.captures(chunk).map(|c| text(&c[1]))).unwrap_or_else(|| code.clone());
        let v = Video {
            cover: IMG.captures(chunk).map(|c| c[1].to_string()).unwrap_or_default(),
            duration: DUR.captures(chunk).map(|c| c[1].to_string()).unwrap_or_default(),
            code,
            title,
            url,
            src: 0,
        };
        if !out.iter().any(|o| o.url == v.url) {
            out.push(v);
        }
    }
    out
}

/// "https://missav.live/dm437/en/genres/Pretty%20Girl" -> "genres/Pretty%20Girl": the part after the tracking
/// prefix and the language, so it can be put after any base of the site.
fn rel_path(url: &str, lang: &str) -> Option<String> {
    let after = url.split_once("://")?.1;
    let mut segs: Vec<&str> = after.split('/').skip(1).filter(|s| !s.is_empty()).collect();
    if segs.first().is_some_and(|s| s.len() > 2 && s.starts_with("dm") && s[2..].bytes().all(|b| b.is_ascii_digit())) {
        segs.remove(0);
    }
    if segs.first() == Some(&lang) {
        segs.remove(0);
    }
    (segs.len() >= 2).then(|| segs.join("/"))
}

static LINK: LazyLock<Regex> = LazyLock::new(|| rx(r#"(?s)<a href="([^"]+)"[^>]*>(.*?)</a>"#));
static KEY: LazyLock<Regex> = LazyLock::new(|| rx(r"(?s)^\s*<span>(.*?)</span>"));

fn parse_info(inner: &str, lang: &str) -> Option<Info> {
    let key = KEY.captures(inner).map(|c| text(&c[1]).trim_end_matches([':', '：']).to_string())?;
    let rest = KEY.replace(inner, "");
    let links: Vec<(String, String)> = LINK.captures_iter(&rest).filter_map(|c| Some((text(&c[2]), rel_path(&c[1], lang)?))).filter(|(n, _)| !n.is_empty()).collect();
    let value = text(&rest);
    (!value.is_empty() && value.len() < 400).then_some(Info { key, value, links })
}

pub fn parse_detail(html: &str, base: &Video, lang: &str) -> Detail {
    let mut video = base.clone();
    if let Some(t) = H1.captures(html).map(|c| text(&c[1])).filter(|t| !t.is_empty()) {
        video.title = t;
    }
    if let Some(c) = OG_IMG.captures(html) {
        video.cover = c[1].to_string();
    }
    let info = INFO.captures_iter(html).filter_map(|c| parse_info(&c[1], lang)).collect();
    Detail {
        description: OG_DESC.captures(html).map(|c| text(&c[1])).unwrap_or_default(),
        info,
        stream: find_stream(html),
        video,
    }
}

/// The playlist link: written in the page, or inside the packed `eval(function(p,a,c,k,e,d)…)` player script.
pub fn find_stream(html: &str) -> Option<String> {
    let mut found: Vec<String> = M3U8.find_iter(html).map(|m| m.as_str().replace("\\/", "/")).collect();
    for c in PACKED.captures_iter(html) {
        let words: Vec<&str> = c[4].split('|').collect();
        let base: u32 = c[2].parse().unwrap_or(36);
        found.extend(M3U8.find_iter(&unpack(&c[1], base, &words)).map(|m| m.as_str().to_string()));
    }
    // the master playlist (all qualities) first
    found.iter().find(|u| u.contains("playlist.m3u8")).or(found.first()).cloned()
}

/// Dean Edwards' packer: every word of the payload is a number in `base` that indexes `words`.
fn unpack(payload: &str, base: u32, words: &[&str]) -> String {
    let payload = payload.replace("\\'", "'");
    let word = rx(r"\b\w+\b");
    word.replace_all(&payload, |c: &regex::Captures| {
        let w = &c[0];
        match from_base(w, base).and_then(|n| words.get(n)).filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => w.to_string(),
        }
    })
    .into_owned()
}

fn from_base(s: &str, base: u32) -> Option<usize> {
    const DIGITS: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    s.chars().try_fold(0usize, |acc, ch| {
        let d = DIGITS.find(ch).filter(|&d| (d as u32) < base)?;
        acc.checked_mul(base as usize)?.checked_add(d)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_card() {
        let html = r#"<div class="thumbnail group"><div class="relative">
          <a href="https://missav.live/dm539/en/sone-123" alt="sone-123">
          <img class="w-full" data-src="https://fourhoi.com/sone-123/cover-t.jpg" alt="SONE-123 Alt title"></a>
          <a href="https://missav.live/dm539/en/sone-123"><span class="absolute bottom-1 right-1">
            2:00:43
          </span></a></div>
          <div class="my-2"><a class="text-secondary group-hover:text-primary" href="https://missav.live/dm539/en/sone-123">
            SONE-123 A &amp; B title </a></div></div>"#;
        let v = parse_list(html);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].code, "SONE-123");
        assert_eq!(v[0].title, "SONE-123 A & B title");
        assert_eq!(v[0].cover, "https://fourhoi.com/sone-123/cover-t.jpg");
        assert_eq!(v[0].duration, "2:00:43");
    }

    #[test]
    fn packed_stream() {
        // payload "0://1.2/3/4.5" with words https|surrit|com|abc-def|playlist|m3u8
        let html = r#"<script>eval(function(p,a,c,k,e,d){e=function(c){return c};return p}('6=\'0://1.2/3/4.5\';',36,7,'https|surrit|com|abc-def|playlist|m3u8|source'.split('|'),0,{}))</script>"#;
        assert_eq!(find_stream(html).as_deref(), Some("https://surrit.com/abc-def/playlist.m3u8"));
    }

    #[test]
    fn interleaves_and_dedupes() {
        let v = |c: &str, src| Video { code: c.into(), src, ..Default::default() };
        let out = interleave(vec![vec![v("A", 0), v("B", 0)], vec![v("B", 1), v("C", 1), v("D", 1)]]);
        let got: Vec<_> = out.iter().map(|x| (x.code.as_str(), x.src)).collect();
        assert_eq!(got, [("A", 0), ("B", 0), ("C", 1), ("D", 1)]);
    }

    #[test]
    fn info_links() {
        let html = r#"<div class="text-secondary">
            <span>Genre:</span> <a href="https://missav.live/dm437/en/genres/Pretty%20Girl" class="x">Pretty Girl</a>, <a href="https://missav.live/en/genres/Petite" class="x">Petite</a>
        </div><div class="text-secondary"><span>Code:</span> <span class="font-medium">SUJI-325</span></div>"#;
        let d = parse_detail(html, &Video::default(), "en");
        assert_eq!(d.info.len(), 2);
        assert_eq!(d.info[0].key, "Genre");
        assert_eq!(d.info[0].links, [("Pretty Girl".to_string(), "genres/Pretty%20Girl".to_string()), ("Petite".to_string(), "genres/Petite".to_string())]);
        assert_eq!((d.info[1].key.as_str(), d.info[1].value.as_str(), d.info[1].links.len()), ("Code", "SUJI-325", 0));
        assert_eq!(rel_path("https://missav123.com/cn/makers/a/b", "cn").as_deref(), Some("makers/a/b"));
        assert_eq!(SOURCES[0].list_url(&with_params("search/rei", "views", "jav"), 2), "https://missav.live/dm539/en/search/rei?filters=jav&sort=views&page=2");
    }

    #[test]
    fn urls() {
        assert_eq!(SOURCES[0].origin(), "https://missav.live");
        assert_eq!(SOURCES[1].list_url(&list_path(""), 2), "https://missav123.com/dm247/cn/new?page=2");
        assert_eq!(SOURCES[0].list_url(&list_path("a b"), 1), "https://missav.live/dm539/en/search/a%20b");
    }
}
