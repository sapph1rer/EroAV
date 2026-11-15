//! The window: sidebar, home (continue watching + rows), video grid (new / discover / search), video page,
//! built-in player (rmp).
//! Every list mixes both sites (source::merged); a video remembers which site it came from.
//! Layout and animations follow waku-app: hover lift on cards, fading pages, shimmering placeholders.

use crate::images::Images;
use crate::settings::Settings;
use crate::source::{self, DISCOVER, Detail, FILTERS, SORTS, SOURCES, Step, Video};
use crate::theme::*;
use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Frame, Margin, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, pos2, vec2,
};
use egui_phosphor::regular as icon;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};

const CARD_W: f32 = 272.0;
const COVER_H: f32 = CARD_W * 0.67; // MissAV covers are ~3:2
const CARD_H: f32 = COVER_H + 52.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Page {
    Home,
    New,
    Discover,
    Search,
    MyList,
    History,
    /// one site's list from a video page link (actress, genre, series, maker, tag)
    Tag,
    Settings,
    Detail,
}

/// A list behind a video page link: one site, a path under its base.
#[derive(Clone, Debug, PartialEq)]
struct Tag {
    src: usize,
    path: String,
    title: String,
}

enum Msg {
    List(u64, Result<Vec<Video>, String>),
    Detail(u64, Result<Detail, String>),
    /// next chunk of the Discover feed
    More(u64, Result<Vec<Video>, String>),
    /// source test finished (index into SOURCES)
    Test(usize, Vec<Step>),
    /// a Home row (index into ROWS)
    Row(usize, Result<Vec<Video>, String>),
    /// "More from …" under the video page
    Related(u64, Result<Vec<Video>, String>),
}

/// Home rows: title, list path. Each shows one line of cards from both sites.
const ROWS: &[(&str, &str)] = &[("Hot today", "today-hot"), ("New releases", "new"), ("Popular this week", "weekly-hot"), ("Top this month", "monthly-hot")];

#[derive(Default)]
struct Row {
    list: Vec<Video>,
    loading: bool,
    error: Option<String>,
}

pub struct EroApp {
    page: Page,
    /// page the video page goes back to
    back: Page,
    /// query the list on screen was loaded for ("" = newest)
    query: String,
    /// set: the list on screen is a video page link's list (Page::Tag) instead of New / Search
    tag: Option<Tag>,
    /// page the tag page goes back to
    tag_back: Page,
    /// search / tag list options (site query values, "" = default)
    sort: String,
    filter: String,
    /// "More from …" under the video page: (title, videos)
    related: Option<(String, Vec<Video>)>,
    related_loading: bool,
    search_text: String,
    page_no: u32,
    list: Vec<Video>,
    loading: bool,
    error: Option<String>,
    detail: Option<Detail>,
    /// the card the video page was opened from (shown while the detail loads)
    opened: Option<Video>,
    detail_loading: bool,
    detail_error: Option<String>,
    /// Discover: endless feed walking DISCOVER lists page by page
    disc: Vec<Video>,
    disc_step: usize,
    disc_loading: bool,
    disc_error: Option<String>,
    disc_seq: u64,
    player: Option<rmp_egui::PlayerView>,
    /// the video in the player (Continue watching)
    playing: Video,
    /// play as soon as the opened video's page arrives (Continue watching)
    autoplay: bool,
    /// time of the last settings save while playing
    saved_at: f64,
    rows: Vec<Row>,
    /// when the Home rows were last loaded
    rows_at: Option<std::time::Instant>,
    settings: Settings,
    /// source tests: None = running
    tests: HashMap<usize, Option<Vec<Step>>>,
    images: Images,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// bumps on every request, so late answers of old requests are dropped
    seq: u64,
    ctx: egui::Context,
}

impl EroApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup(&cc.egui_ctx);
        rmp_egui::install(cc.get_proc_address.clone());
        let (tx, rx) = channel();
        let mut app = Self {
            page: Page::Home,
            back: Page::Home,
            query: String::new(),
            tag: None,
            tag_back: Page::Detail,
            sort: String::new(),
            filter: String::new(),
            related: None,
            related_loading: false,
            search_text: String::new(),
            page_no: 1,
            list: vec![],
            loading: false,
            error: None,
            detail: None,
            opened: None,
            detail_loading: false,
            detail_error: None,
            disc: vec![],
            disc_step: 0,
            disc_loading: false,
            disc_error: None,
            disc_seq: 0,
            player: None,
            playing: Video::default(),
            autoplay: false,
            saved_at: 0.0,
            rows: ROWS.iter().map(|_| Row::default()).collect(),
            rows_at: None,
            settings: Settings::load(),
            tests: HashMap::new(),
            images: Images::new(&cc.egui_ctx),
            tx,
            rx,
            seq: 0,
            ctx: cc.egui_ctx.clone(),
        };
        cc.egui_ctx.set_zoom_factor(app.settings.ui_scale);
        app.load_rows();
        // fill Discover in the background, so the tab is ready when opened
        app.more();
        app
    }

    /// New (query "") or a search.
    fn load(&mut self, query: String, page: u32) {
        self.tag = None;
        self.query = query;
        self.reload(page);
    }

    fn load_tag(&mut self, tag: Tag) {
        if self.page != Page::Tag {
            self.tag_back = self.page;
        }
        self.page = Page::Tag;
        self.tag = Some(tag);
        self.query.clear();
        self.sort.clear();
        self.filter.clear();
        self.reload(1);
    }

    /// Where the list on screen comes from: a single site (tag pages) or all of them, and the path.
    fn target(&self) -> (Option<usize>, String) {
        match &self.tag {
            Some(t) => (Some(t.src), source::with_params(&t.path, &self.sort, &self.filter)),
            None if self.query.is_empty() => (None, "new".into()),
            None => (None, source::with_params(&source::list_path(&self.query), &self.sort, &self.filter)),
        }
    }

    /// (Re)load page `page` of the current list.
    fn reload(&mut self, page: u32) {
        self.seq += 1;
        let (seq, tx, ctx) = (self.seq, self.tx.clone(), self.ctx.clone());
        self.page_no = page;
        self.error = None;
        let (src, path) = self.target();
        // last time's list shows at once; the fresh one replaces it when it arrives
        self.list = if src.is_none() { source::cached(&path, page).unwrap_or_default() } else { vec![] };
        self.loading = true;
        std::thread::spawn(move || {
            let r = match src {
                Some(i) => SOURCES[i].list(&path, page),
                None => source::merged(&path, page),
            };
            let _ = tx.send(Msg::List(seq, r));
            ctx.request_repaint();
        });
    }

    /// "More from …": the first actress, series or genre link of the video page, from the same site.
    fn load_related(&mut self, d: &Detail) {
        self.related = None;
        let links: Vec<&(String, String)> = d.info.iter().flat_map(|i| &i.links).collect();
        let pick = ["actresses/", "series/", "genres/"].iter().find_map(|k| links.iter().find(|(_, p)| p.starts_with(k)));
        let Some((name, path)) = pick.map(|x| (*x).clone()) else { return };
        let title = format!("More from {name}");
        self.related = Some((title, vec![]));
        self.related_loading = true;
        let (seq, tx, ctx, src, code) = (self.seq, self.tx.clone(), self.ctx.clone(), d.video.src, d.video.code.clone());
        std::thread::spawn(move || {
            let r = SOURCES[src].list(&path, 1).map(|v| v.into_iter().filter(|x| x.code != code).collect());
            let _ = tx.send(Msg::Related(seq, r));
            ctx.request_repaint();
        });
    }

    /// Home rows: last time's lists at once, fresh ones as they come.
    fn load_rows(&mut self) {
        self.rows_at = Some(std::time::Instant::now());
        for (i, (_, path)) in ROWS.iter().enumerate() {
            let row = &mut self.rows[i];
            if let Some(v) = source::cached(path, 1) {
                row.list = v;
            }
            row.loading = true;
            row.error = None;
            let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(Msg::Row(i, source::merged(path, 1)));
                ctx.request_repaint();
            });
        }
    }

    fn go_home(&mut self) {
        self.page = Page::Home;
        if self.rows_at.is_none_or(|t| t.elapsed().as_secs() > 300) && !self.rows.iter().any(|r| r.loading) {
            self.load_rows();
        }
    }

    fn open(&mut self, v: Video) {
        self.autoplay = false;
        self.related = None;
        self.related_loading = false;
        self.seq += 1;
        let (seq, tx, ctx) = (self.seq, self.tx.clone(), self.ctx.clone());
        if self.page != Page::Detail {
            self.back = self.page;
        }
        self.page = Page::Detail;
        self.detail = None;
        self.detail_error = None;
        self.detail_loading = true;
        self.opened = Some(v.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Detail(seq, v.source().detail(&v)));
            ctx.request_repaint();
        });
    }

    /// Next chunk of the Discover feed: list DISCOVER[step % n], page step / n + 1, from both sites.
    fn more(&mut self) {
        self.disc_seq += 1;
        let (seq, tx, ctx) = (self.disc_seq, self.tx.clone(), self.ctx.clone());
        let (path, page) = (DISCOVER[self.disc_step % DISCOVER.len()], (self.disc_step / DISCOVER.len()) as u32 + 1);
        self.disc_loading = true;
        self.disc_error = None;
        // cached chunk first (fresh videos are appended when they come)
        for x in source::cached(path, page).unwrap_or_default() {
            if !self.disc.iter().any(|o| o.code == x.code) {
                self.disc.push(x);
            }
        }
        std::thread::spawn(move || {
            let _ = tx.send(Msg::More(seq, source::merged(path, page)));
            ctx.request_repaint();
        });
    }

    fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::List(s, r) if s == self.seq => {
                    self.loading = false;
                    match r {
                        Ok(v) => self.list = v,
                        // a cached list stays on screen when the refresh fails
                        Err(e) if self.list.is_empty() => self.error = Some(e),
                        Err(_) => {}
                    }
                }
                Msg::Related(s, r) if s == self.seq => {
                    self.related_loading = false;
                    match (r, &mut self.related) {
                        (Ok(v), Some((_, list))) => *list = v,
                        _ => self.related = None,
                    }
                }
                Msg::Row(i, r) => {
                    let row = &mut self.rows[i];
                    row.loading = false;
                    match r {
                        Ok(v) => row.list = v,
                        Err(e) if row.list.is_empty() => row.error = Some(e),
                        Err(_) => {}
                    }
                }
                Msg::Test(i, steps) => {
                    self.tests.insert(i, Some(steps));
                }
                Msg::More(s, r) if s == self.disc_seq => {
                    self.disc_loading = false;
                    match r {
                        Ok(v) => {
                            self.disc_step += 1;
                            for x in v {
                                if !self.disc.iter().any(|o| o.code == x.code) {
                                    self.disc.push(x);
                                }
                            }
                        }
                        Err(e) => self.disc_error = Some(e),
                    }
                }
                Msg::Detail(s, r) if s == self.seq => {
                    self.detail_loading = false;
                    match r {
                        Ok(d) => {
                            if std::mem::take(&mut self.autoplay) {
                                self.play(&d);
                            }
                            self.load_related(&d);
                            self.detail = Some(d);
                        }
                        Err(e) => self.detail_error = Some(e),
                    }
                }
                _ => {}
            }
        }
    }

    fn play(&mut self, d: &Detail) {
        let Some(url) = d.stream.clone() else { return };
        let start = self.settings.resume_pos(&d.video.code);
        // surrit.com wants only the Referer: an Origin header or the loopback proxy's client get HTTP 403,
        // while mpv's own HLS reader plays the `.jpeg` segments fine
        let req = rmp_egui::OpenRequest {
            url,
            title: Some(d.video.title.clone()),
            referrer: Some(d.video.referer()),
            hls_proxy: Some(false),
            start,
            volume: self.settings.volume,
            muted: self.settings.muted,
            ..Default::default()
        };
        let item = rmp_egui::QueueItem { title: d.video.code.clone(), subtitle: d.video.title.clone(), req: Some(req), skip: vec![] };
        let mut s = self.settings.view();
        s.theme = rmp_egui::Theme {
            accent: ACCENT,
            accent_hi: ACCENT_HI,
            accent_dim: Color32::from_rgb(0x8a, 0x1f, 0x4c),
            muted: MUTED,
            warn: WARN,
            panel: Color32::from_rgba_unmultiplied(18, 14, 20, 240),
        };
        self.playing = d.video.clone();
        self.player = Some(rmp_egui::PlayerView::new(&self.ctx, s, vec![item], 0));
    }

    /// Remember where the video is and the volume the user set.
    fn player_events(&mut self, events: Vec<rmp_egui::Event>) -> bool {
        let mut closed = false;
        for e in events {
            match e {
                // pos 0 before the file has started (or seeked to the resume point) says nothing
                rmp_egui::Event::Progress { pos, dur, ended, .. } if ended || pos >= 1.0 => {
                    let pos = if ended { dur } else { pos };
                    self.settings.watched(&self.playing, pos, dur);
                    // saved now and then too, so closing the whole app mid-video keeps the place
                    let now = self.ctx.input(|i| i.time);
                    if now - self.saved_at > 10.0 {
                        self.saved_at = now;
                        self.settings.save();
                    }
                }
                rmp_egui::Event::SettingsChanged => {
                    if let Some(v) = &self.player {
                        self.settings.volume = v.settings().volume;
                        self.settings.muted = v.settings().muted;
                    }
                }
                rmp_egui::Event::Closed => closed = true,
                _ => {}
            }
        }
        if closed {
            self.settings.save();
        }
        closed
    }

    fn run_test(&mut self, i: usize) {
        self.tests.insert(i, None);
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Test(i, SOURCES[i].test()));
            ctx.request_repaint();
        });
    }

    // ------------------------------------------------------------------ sidebar

    fn sidebar(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(16.0);
            let (r, resp) = ui.allocate_exact_size(vec2(46.0, 46.0), Sense::click());
            let hv = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.15);
            logo(ui.painter(), r.center(), 21.0 + hv * 2.0, ui.input(|i| i.time) as f32);
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                self.go_home();
            }
            ui.add_space(36.0);
            let items = [
                (Page::Home, icon::HOUSE, "Home"),
                (Page::New, icon::SPARKLE, "New"),
                (Page::Discover, icon::COMPASS, "Discover"),
                (Page::Search, icon::MAGNIFYING_GLASS, "Search"),
                (Page::MyList, icon::HEART, "My List"),
                (Page::History, icon::CLOCK_COUNTER_CLOCKWISE, "History"),
                (Page::Settings, icon::GEAR_SIX, "Settings"),
            ];
            let mut active_y = None;
            for (p, g, label) in items {
                if p == Page::Settings {
                    // Settings sits at the bottom of the bar, under a short divider
                    ui.add_space((ui.available_height() - 58.0 - 18.0).max(8.0));
                    let y = ui.cursor().min.y - 6.0;
                    let cx = ui.max_rect().center().x;
                    ui.painter().hline(cx - 18.0..=cx + 18.0, y, Stroke::new(1.0, LINE));
                }
                let from = |x: Page| if x == Page::Tag { self.tag_back } else { x };
                let active = self.page == p || from(self.page) == p || (self.page == Page::Detail && from(self.back) == p);
                let resp = sidebar_item(ui, g, label, active);
                if active {
                    active_y = Some(resp.rect.center().y);
                }
                if resp.clicked() {
                    match p {
                        Page::Home => self.go_home(),
                        Page::New => self.go_new(),
                        Page::Discover => {
                            self.page = Page::Discover;
                            if self.disc.is_empty() && !self.disc_loading {
                                self.more();
                            }
                        }
                        p => self.page = p,
                    }
                }
                ui.add_space(4.0);
            }
            // the accent bar slides between items instead of jumping
            if let Some(y) = active_y {
                let y = ui.ctx().animate_value_with_time(egui::Id::new("nav_bar"), y, 0.22);
                let x = ui.max_rect().min.x - 9.0;
                ui.painter().rect_filled(Rect::from_center_size(pos2(x, y), vec2(3.5, 24.0)), CornerRadius::same(2), ACCENT);
            }
        });
    }

    fn go_new(&mut self) {
        self.page = Page::New;
        if self.tag.is_some() || !self.query.is_empty() || (self.list.is_empty() && !self.loading) {
            self.load(String::new(), 1);
        }
    }

    // ------------------------------------------------------------------ pages

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let tag_title = self.tag.as_ref().map(|t| t.title.clone()).unwrap_or_default();
            let title = match (self.page, self.query.is_empty()) {
                (Page::Home, _) => "Home",
                (Page::Discover, _) => "Discover",
                (Page::MyList, _) => "My List",
                (Page::History, _) => "History",
                (Page::Tag, _) => tag_title.as_str(),
                (Page::Search, _) | (_, false) => "Search",
                _ => "New releases",
            };
            ui.add(egui::Label::new(RichText::new(title).font(bold(26.0))).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // every list mixes all sites: they show as lit, non-clickable chips
                for s in SOURCES.iter().rev() {
                    chip(ui, s.name, true);
                }
                ui.label(RichText::new(icon::GLOBE_SIMPLE).size(18.0).color(MUTED));
                let busy = match self.page {
                    Page::Home => self.rows.iter().any(|r| r.loading && !r.list.is_empty()),
                    Page::Discover => self.disc_loading && !self.disc.is_empty(),
                    _ => self.loading && !self.list.is_empty(),
                };
                if busy {
                    // a cached list is on screen while the fresh one loads
                    ui.add_space(8.0);
                    ui.label(RichText::new("Refreshing").size(12.5).color(MUTED));
                    ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                }
            });
        });
        ui.add_space(10.0);
    }

    fn home_page(&mut self, ui: &mut Ui) {
        let mut open: Option<(Video, bool)> = None;
        let mut remove = None;
        let mut see_all = None;
        let mut fav: Option<Video> = None;
        if !self.settings.watching.is_empty() {
            section(ui, icon::CLOCK_COUNTER_CLOCKWISE, "Continue watching", None);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing = vec2(18.0, 24.0);
                ui.horizontal_wrapped(|ui| {
                    for (i, w) in self.settings.watching.iter().enumerate() {
                        let (resp, x) = watched_card(ui, &mut self.images, w, i);
                        if x {
                            remove = Some(w.video.code.clone());
                        } else if resp.clicked() {
                            open = Some((w.video.clone(), true));
                        }
                    }
                });
            });
            ui.add_space(26.0);
        }
        for (i, (title, _)) in ROWS.iter().enumerate() {
            let target = if ROWS[i].1 == "new" { Page::New } else { Page::Discover };
            if section(ui, if i == 0 { icon::FIRE } else { icon::FILM_STRIP }, title, Some("See all")) {
                see_all = Some(target);
            }
            let row = &self.rows[i];
            if let Some(e) = &row.error {
                ui.label(RichText::new(format!("{}  {}", icon::WARNING, e.lines().next().unwrap_or(""))).size(12.5).color(MUTED));
            } else {
                // one line of cards: as many as fit
                let n = (((ui.available_width() + 18.0) / (CARD_W + 18.0)).floor() as usize).max(1);
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing = vec2(18.0, 24.0);
                    ui.horizontal(|ui| {
                        if row.list.is_empty() {
                            (0..n).for_each(|_| card_skeleton(ui));
                        }
                        let (o, t) = cards(ui, &mut self.images, &self.settings, row.list.iter().take(n));
                        open = open.take().or(o.map(|v| (v, false)));
                        fav = fav.take().or(t);
                    });
                });
            }
            ui.add_space(26.0);
        }
        if let Some(code) = remove {
            self.settings.watching.retain(|w| w.video.code != code);
            self.settings.save();
        }
        if let Some(v) = fav {
            self.settings.toggle_fav(&v);
        }
        if let Some((v, play)) = open {
            self.open(v);
            self.autoplay = play;
        }
        match see_all {
            Some(Page::New) => self.go_new(),
            Some(_) => {
                self.page = Page::Discover;
                if self.disc.is_empty() && !self.disc_loading {
                    self.more();
                }
            }
            None => {}
        }
    }

    fn search_page(&mut self, ui: &mut Ui) {
        let w = ui.available_width().min(760.0);
        ui.horizontal(|ui| {
            let resp = Frame::new()
                .fill(PANEL)
                .corner_radius(CornerRadius::same(24))
                .stroke(Stroke::new(1.0, LINE))
                .inner_margin(Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::MAGNIFYING_GLASS).size(20.0).color(ACCENT));
                        let te = egui::TextEdit::singleline(&mut self.search_text)
                            .hint_text("Code, title or actress  (e.g. SONE-123)")
                            .frame(Frame::NONE)
                            .font(regular(17.0))
                            .desired_width(w - 120.0);
                        let r = ui.add(te);
                        if self.page == Page::Search && self.query.is_empty() && self.list.is_empty() && !self.loading {
                            r.request_focus();
                        }
                        r
                    })
                    .inner
                });
            let enter = resp.inner.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let go = accent_button(ui, icon::ARROW_RIGHT, "Search").clicked();
            if (enter || go) && !self.search_text.trim().is_empty() {
                let q = self.search_text.trim().to_string();
                self.load(q, 1);
            }
        });
        ui.add_space(12.0);
        self.list_options(ui);
        ui.add_space(14.0);
        if !self.query.is_empty() {
            self.grid(ui);
        } else {
            empty(ui, icon::MAGNIFYING_GLASS, "Type something and press Enter");
        }
    }

    /// Sort and filter chips (search and tag pages); a change reloads the list.
    fn list_options(&mut self, ui: &mut Ui) {
        let mut changed = false;
        for (label, opts, cur) in [("Sort", SORTS, &mut self.sort), ("Filter", FILTERS, &mut self.filter)] {
            ui.horizontal_wrapped(|ui| {
                ui.add_sized([52.0, 26.0], egui::Label::new(RichText::new(label).color(DIM)));
                for (k, name) in opts {
                    if small_chip(ui, name, cur == k).clicked() && cur != k {
                        *cur = k.to_string();
                        changed = true;
                    }
                }
            });
            ui.add_space(4.0);
        }
        if changed && (self.tag.is_some() || !self.query.is_empty()) {
            self.reload(1);
        }
    }

    /// One site's list behind a video page link.
    fn tag_page(&mut self, ui: &mut Ui) {
        if ghost_button(ui, icon::ARROW_LEFT, "Back").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.page = self.tag_back;
            return;
        }
        ui.add_space(12.0);
        self.top_bar(ui);
        self.list_options(ui);
        ui.add_space(14.0);
        self.grid(ui);
    }

    /// My List / History: saved videos, newest first.
    fn saved_page(&mut self, ui: &mut Ui, history: bool) {
        self.top_bar(ui);
        let list_len = if history { self.settings.history.len() } else { self.settings.my_list.len() };
        if list_len == 0 {
            if history {
                empty(ui, icon::CLOCK_COUNTER_CLOCKWISE, "Videos you play show up here");
            } else {
                empty(ui, icon::HEART, "Tap the heart on a video to keep it here");
            }
            return;
        }
        if history {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{list_len} videos")).color(MUTED));
                if ghost_button(ui, icon::TRASH, "Clear history").clicked() {
                    self.settings.history.clear();
                    self.settings.save();
                }
            });
            ui.add_space(14.0);
        }
        let mut open = None;
        let mut fav = None;
        let mut remove = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = vec2(18.0, 24.0);
            ui.horizontal_wrapped(|ui| {
                if history {
                    for (i, w) in self.settings.history.iter().enumerate() {
                        let (resp, x) = watched_card(ui, &mut self.images, w, i);
                        if x {
                            remove = Some(w.video.code.clone());
                        } else if resp.clicked() {
                            open = Some(w.video.clone());
                        }
                    }
                } else {
                    (open, fav) = cards(ui, &mut self.images, &self.settings, self.settings.my_list.iter());
                }
            });
        });
        if let Some(code) = remove {
            self.settings.history.retain(|w| w.video.code != code);
            self.settings.save();
        }
        if let Some(v) = fav {
            self.settings.toggle_fav(&v);
        }
        if let Some(v) = open {
            self.open(v);
        }
    }

    fn grid(&mut self, ui: &mut Ui) {
        if let Some(e) = self.error.clone() {
            error_box(ui, &e);
            ui.add_space(8.0);
            if accent_button(ui, icon::ARROW_CLOCKWISE, "Try again").clicked() {
                self.reload(self.page_no);
            }
            return;
        }
        let mut clicked = None;
        let mut fav = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = vec2(18.0, 24.0);
            ui.horizontal_wrapped(|ui| {
                if self.loading && self.list.is_empty() {
                    for _ in 0..12 {
                        card_skeleton(ui);
                    }
                    return;
                }
                (clicked, fav) = cards(ui, &mut self.images, &self.settings, self.list.iter());
            });
        });
        if !self.loading && self.list.is_empty() {
            empty(ui, icon::FILM_SLATE, "Nothing found");
        }
        if let Some(v) = fav {
            self.settings.toggle_fav(&v);
        }
        if let Some(v) = clicked {
            self.open(v);
        }
        // pages
        if !self.loading && (self.page_no > 1 || !self.list.is_empty()) {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                let w = 240.0;
                ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                if self.page_no > 1 && ghost_button(ui, icon::CARET_LEFT, "Prev").clicked() {
                    self.reload(self.page_no - 1);
                }
                ui.label(RichText::new(format!("Page {}", self.page_no)).font(bold(15.0)).color(ACCENT_HI));
                if !self.list.is_empty() && ghost_button(ui, icon::CARET_RIGHT, "Next").clicked() {
                    self.reload(self.page_no + 1);
                }
            });
        }
    }

    /// Endless feed: when the bottom comes into view, the next chunk loads.
    fn discover_page(&mut self, ui: &mut Ui) {
        let mut clicked = None;
        let mut fav = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = vec2(18.0, 24.0);
            ui.horizontal_wrapped(|ui| {
                (clicked, fav) = cards(ui, &mut self.images, &self.settings, self.disc.iter());
                if self.disc_loading {
                    for _ in 0..(if self.disc.is_empty() { 12 } else { 6 }) {
                        card_skeleton(ui);
                    }
                }
            });
        });
        if let Some(v) = fav {
            self.settings.toggle_fav(&v);
        }
        if let Some(v) = clicked {
            self.open(v);
        }
        ui.add_space(20.0);
        if let Some(e) = self.disc_error.clone() {
            error_box(ui, &e);
            ui.add_space(8.0);
            if accent_button(ui, icon::ARROW_CLOCKWISE, "Try again").clicked() {
                self.more();
            }
        } else if !self.disc_loading {
            // sentinel at the bottom: visible means the user scrolled down to it
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            if ui.is_rect_visible(r) {
                self.more();
            }
        }
    }

    fn settings_page(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Settings").font(bold(26.0)));
        ui.add_space(14.0);
        let mut changed = false;
        let st = &mut self.settings;

        card(ui, |ui| {
            ui.label(RichText::new(format!("{}  General", icon::SLIDERS_HORIZONTAL)).font(bold(18.0)));
            ui.add_space(6.0);
            egui::Grid::new("general").num_columns(2).spacing([28.0, 12.0]).show(ui, |ui| {
                ui.label("UI scale");
                if ui.add(egui::Slider::new(&mut st.ui_scale, 0.8..=1.6).step_by(0.05).fixed_decimals(2)).changed() {
                    self.ctx.set_zoom_factor(st.ui_scale);
                    changed = true;
                }
                ui.end_row();
            });
        });
        ui.add_space(14.0);

        card(ui, |ui| {
            ui.label(RichText::new(format!("{}  Playback (rmp)", icon::PLAY_CIRCLE)).font(bold(18.0)));
            ui.label(RichText::new("Videos play in the built-in rmp player; the stream offers 360p to 720p and picks the best one.").size(12.5).color(MUTED));
            ui.add_space(6.0);
            egui::Grid::new("play").num_columns(2).spacing([28.0, 12.0]).show(ui, |ui| {
                ui.label("Player");
                match rmp_egui::engine_problem() {
                    None => ui.label(RichText::new(format!("{} libmpv found: GPU decoding, all codecs", icon::CHECK_CIRCLE)).size(12.5).color(GOOD)),
                    Some(why) => ui
                        .label(RichText::new(format!("{} libmpv-2.dll is missing next to eroav.exe: playing with the basic Windows engine", icon::WARNING)).size(12.5).color(WARN))
                        .on_hover_text(why),
                };
                ui.end_row();

                ui.label("Engine");
                ui.horizontal(|ui| {
                    for (k, name) in [("auto", "Automatic"), ("mpv", "libmpv"), ("native", "Windows (Media Foundation)")] {
                        if ui.selectable_label(st.engine == k, name).clicked() {
                            st.engine = k.into();
                            changed = true;
                        }
                    }
                });
                ui.end_row();

                ui.label("Hardware decoding");
                egui::ComboBox::from_id_salt("hwdec").selected_text(st.hwdec.clone()).show_ui(ui, |ui| {
                    for k in ["auto-safe", "auto", "d3d11va", "d3d11va-copy", "nvdec", "dxva2-copy", "no"] {
                        changed |= ui.selectable_value(&mut st.hwdec, k.to_string(), k).changed();
                    }
                });
                ui.end_row();

                ui.label("Picture quality");
                ui.horizontal(|ui| {
                    for (k, name) in [("fast", "Fast"), ("balanced", "Balanced"), ("high", "High")] {
                        if ui.selectable_label(st.quality == k, name).clicked() {
                            st.quality = k.into();
                            changed = true;
                        }
                    }
                });
                ui.end_row();

                ui.label("Picture");
                ui.vertical(|ui| {
                    changed |= ui.checkbox(&mut st.smooth_motion, "Smooth motion (interpolation, needs a steady refresh rate)").changed();
                    changed |= ui.checkbox(&mut st.deband, "Remove colour banding (deband)").changed();
                });
                ui.end_row();

                ui.label("Audio");
                changed |= ui.checkbox(&mut st.stereo, "Mix surround down to stereo (headphones / TV speakers)").changed();
                ui.end_row();

                ui.label("Behaviour");
                ui.vertical(|ui| {
                    changed |= ui.checkbox(&mut st.resume, "Resume where I stopped").changed();
                    changed |= ui.checkbox(&mut st.start_fullscreen, "Start in full screen").changed();
                });
                ui.end_row();

                ui.label("Seek step");
                changed |= ui.add(egui::Slider::new(&mut st.seek_step, 5.0..=30.0).step_by(5.0).suffix(" s")).changed();
                ui.end_row();
            });
            if !st.watching.is_empty() {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} videos in Continue watching", st.watching.len())).size(12.5).color(DIM));
                    if ui.small_button("Clear").clicked() {
                        st.watching.clear();
                        changed = true;
                    }
                });
            }
        });
        if changed {
            self.settings.save();
        }
        ui.add_space(14.0);

        let mut test = None;
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}  Sources", icon::GLOBE_SIMPLE)).font(bold(18.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ghost_button(ui, icon::FLASK, "Test all").clicked() {
                        test = Some(usize::MAX);
                    }
                });
            });
            ui.label(RichText::new("Test = newest list, video page, stream link, stream download. Both sources are merged in every list.").size(12.5).color(MUTED));
            ui.add_space(8.0);
            for (i, src) in SOURCES.iter().enumerate() {
                Frame::new().fill(PANEL).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(12, 10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let dot = match self.tests.get(&i) {
                            Some(Some(steps)) if steps.iter().all(|s| s.ok) => GOOD,
                            Some(Some(_)) => BAD,
                            _ => DIM,
                        };
                        ui.label(RichText::new(icon::CIRCLE).size(11.0).color(dot));
                        ui.label(RichText::new(src.name).font(bold(15.0)));
                        ui.label(RichText::new(src.base).size(12.5).color(MUTED));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(format!("{}  Test", icon::FLASK)).clicked() {
                                test = Some(i);
                            }
                        });
                    });
                    match self.tests.get(&i) {
                        Some(None) => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(RichText::new("Testing…").color(MUTED));
                            });
                        }
                        Some(Some(steps)) => {
                            for s in steps {
                                let (g, c) = if s.ok { (icon::CHECK_CIRCLE, GOOD) } else { (icon::X_CIRCLE, BAD) };
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new(g).color(c));
                                    ui.label(RichText::new(s.name).strong());
                                    ui.label(RichText::new(&s.detail).color(MUTED));
                                });
                            }
                        }
                        None => {}
                    }
                });
                ui.add_space(4.0);
            }
        });
        match test {
            Some(usize::MAX) => (0..SOURCES.len()).for_each(|i| self.run_test(i)),
            Some(i) => self.run_test(i),
            None => {}
        }
        ui.add_space(14.0);
        ui.label(RichText::new(format!("Data folder: {}", crate::images::data_dir().display())).size(12.0).color(DIM));
    }

    fn detail_page(&mut self, ui: &mut Ui) {
        if ghost_button(ui, icon::ARROW_LEFT, "Back").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.page = self.back;
            return;
        }
        ui.add_space(12.0);
        let Some(v) = self.detail.as_ref().map(|d| d.video.clone()).or_else(|| self.opened.clone()) else { return };
        let referer = v.referer();
        let mut play = false;
        let mut tag: Option<Tag> = None;
        let mut fav = false;
        ui.horizontal_top(|ui| {
            let w = (ui.available_width() * 0.5).clamp(320.0, 720.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(w, w * 0.67), Sense::click());
            let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.18);
            cover(ui, &mut self.images, &v.cover, &referer, rect, 10);
            // glowing play button over the cover
            let pulse = (ui.input(|i| i.time) as f32 * 2.4).sin() * 0.5 + 0.5;
            let p = ui.painter();
            p.rect_filled(rect, CornerRadius::same(10), Color32::from_black_alpha((60.0 + 60.0 * h) as u8));
            if self.detail.as_ref().is_some_and(|d| d.stream.is_some()) {
                let r = 34.0 + 6.0 * h;
                p.circle_filled(rect.center(), r + 8.0 * pulse, ACCENT.gamma_multiply(0.18 * (1.0 - pulse)));
                p.circle_filled(rect.center(), r, ACCENT);
                p.text(rect.center() + vec2(2.0, 0.0), Align2::CENTER_CENTER, icon::PLAY, regular(30.0), Color32::WHITE);
                ui.ctx().request_repaint();
                play |= resp.on_hover_cursor(CursorIcon::PointingHand).clicked();
            }
            ui.add_space(24.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    badge(ui, &v.code);
                    if !v.duration.is_empty() {
                        ui.label(RichText::new(format!("{}  {}", icon::CLOCK, v.duration)).color(MUTED));
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new(&v.title).font(bold(22.0)));
                ui.add_space(10.0);
                if self.detail_loading {
                    for w in [0.9, 0.7, 0.8] {
                        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width() * w, 14.0), Sense::hover());
                        shimmer(ui, r, 4);
                    }
                } else if let Some(e) = &self.detail_error {
                    error_box(ui, e);
                } else if let Some(d) = &self.detail {
                    ui.horizontal(|ui| {
                        match &d.stream {
                            Some(_) => play |= accent_button(ui, icon::PLAY, "Play").clicked(),
                            None => error_box(ui, "No playable stream was found on this page"),
                        }
                        let saved = self.settings.is_fav(&v.code);
                        fav = ghost_button(ui, icon::HEART, if saved { "In My List" } else { "Add to My List" }).clicked();
                    });
                    ui.add_space(10.0);
                    for line in &d.info {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&line.key).color(DIM));
                            if line.links.is_empty() {
                                ui.label(RichText::new(&line.value).color(TEXT));
                            }
                            // actress / genre / series ...: each opens its list
                            for (name, path) in &line.links {
                                if small_chip(ui, name, false).on_hover_text(format!("All videos: {name}")).clicked() {
                                    tag = Some(Tag { src: v.src, path: path.clone(), title: format!("{}: {name}", line.key) });
                                }
                            }
                        });
                    }
                    if !d.description.is_empty() {
                        ui.add_space(8.0);
                        ui.label(RichText::new(&d.description).color(MUTED));
                    }
                }
            });
        });
        // More from …
        if let Some((title, list)) = &self.related {
            ui.add_space(30.0);
            section(ui, icon::FILM_STRIP, title, None);
            let mut open = None;
            let mut fav2 = None;
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing = vec2(18.0, 24.0);
                ui.horizontal_wrapped(|ui| {
                    if self.related_loading {
                        (0..6).for_each(|_| card_skeleton(ui));
                    }
                    (open, fav2) = cards(ui, &mut self.images, &self.settings, list.iter());
                });
            });
            if !self.related_loading && list.is_empty() {
                ui.label(RichText::new("No other videos").color(MUTED));
            }
            if let Some(v) = fav2 {
                self.settings.toggle_fav(&v);
            }
            if let Some(v) = open {
                self.open(v);
            }
        }
        if fav {
            self.settings.toggle_fav(&v);
        }
        if let Some(t) = tag {
            self.load_tag(t);
        }
        if play && let Some(d) = self.detail.clone() {
            self.play(&d);
        }
    }
}

impl eframe::App for EroApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.images.pump(&ctx);
        self.drain();

        if let Some(view) = &mut self.player {
            let mut events = vec![];
            egui::CentralPanel::default().frame(Frame::new().fill(Color32::BLACK)).show(ui, |ui| {
                events = view.ui(ui);
            });
            if self.player_events(events) {
                self.player = None;
            }
            return;
        }

        egui::Panel::left("nav")
            .exact_size(84.0)
            .resizable(false)
            .frame(Frame::new().fill(BAR).inner_margin(Margin::symmetric(10, 0)).stroke(Stroke::new(1.0, Color32::from_rgb(0x22, 0x1a, 0x24))))
            .show(ui, |ui| self.sidebar(ui));

        egui::CentralPanel::default().frame(Frame::new().fill(BG)).show(ui, |ui| {
            let salt = match self.page {
                Page::Detail => format!("detail-{}", self.opened.as_ref().map_or("", |v| v.url.as_str())),
                Page::Discover => "discover".into(), // the feed grows in place: no re-animation
                p => format!("{p:?}-{}-{}-{:?}-{}-{}", self.query, self.page_no, self.tag.as_ref().map(|t| &t.path), self.sort, self.filter),
            };
            // fade + slide the page in when it changes
            let now = ui.input(|i| i.time);
            let key = egui::Id::new("page_anim");
            let (prev, t0): (String, f64) = ui.ctx().data(|d| d.get_temp(key)).unwrap_or_default();
            let t0 = if prev != salt {
                ui.ctx().data_mut(|d| d.insert_temp(key, (salt.clone(), now)));
                now
            } else {
                t0
            };
            let k = (((now - t0) / 0.3) as f32).clamp(0.0, 1.0);
            let ease = 1.0 - (1.0 - k).powi(3);
            if k < 1.0 {
                ui.ctx().request_repaint();
            }
            ui.multiply_opacity(ease);
            egui::ScrollArea::vertical().id_salt(&salt).auto_shrink([false, false]).show(ui, |ui| {
                Frame::new().inner_margin(Margin { left: 32, right: 24, top: 22, bottom: 40 }).show(ui, |ui| {
                    ui.add_space((1.0 - ease) * 18.0);
                    match self.page {
                        Page::Detail => self.detail_page(ui),
                        Page::Home => {
                            self.top_bar(ui);
                            self.home_page(ui);
                        }
                        Page::New => {
                            self.top_bar(ui);
                            self.grid(ui);
                        }
                        Page::Discover => {
                            self.top_bar(ui);
                            self.discover_page(ui);
                        }
                        Page::Search => {
                            self.top_bar(ui);
                            self.search_page(ui);
                        }
                        Page::Settings => self.settings_page(ui),
                        Page::Tag => self.tag_page(ui),
                        Page::MyList => self.saved_page(ui, false),
                        Page::History => self.saved_page(ui, true),
                    }
                });
            });
        });
    }
}

// ---------------------------------------------------------------------- widgets

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()), f(a.a(), b.a()))
}

fn gradient(p: &egui::Painter, rect: Rect, a: Color32, b: Color32, vertical: bool) {
    let mut m = Mesh::default();
    let (tl, tr, bl, br) = if vertical { (a, a, b, b) } else { (a, b, a, b) };
    for (pos, color) in [(rect.left_top(), tl), (rect.right_top(), tr), (rect.left_bottom(), bl), (rect.right_bottom(), br)] {
        m.vertices.push(Vertex { pos, uv: WHITE_UV, color });
    }
    m.add_triangle(0, 1, 2);
    m.add_triangle(1, 3, 2);
    p.add(egui::Shape::mesh(m));
}

/// Loading placeholder: a soft light sweeping across.
fn shimmer(ui: &Ui, rect: Rect, radius: u8) {
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(radius), PANEL);
    let t = ui.input(|i| i.time) as f32;
    let w = rect.width().max(1.0);
    let band = (w * 0.45).max(60.0);
    let x = rect.min.x - band + ((t * 0.9) % 1.6) / 1.6 * (w + band * 2.0);
    let clip = p.with_clip_rect(rect);
    let hi = Color32::from_rgba_unmultiplied(255, 120, 180, 14);
    gradient(&clip, Rect::from_min_max(pos2(x, rect.min.y), pos2(x + band / 2.0, rect.max.y)), Color32::TRANSPARENT, hi, false);
    gradient(&clip, Rect::from_min_max(pos2(x + band / 2.0, rect.min.y), pos2(x + band, rect.max.y)), hi, Color32::TRANSPARENT, false);
    ui.ctx().request_repaint();
}

/// UV rect that crops the texture to fill `target` (object-fit: cover).
fn cover_uv(tex: egui::Vec2, target: egui::Vec2) -> Rect {
    let (tr, r) = (tex.x / tex.y.max(1.0), target.x / target.y.max(1.0));
    if tr > r {
        let w = r / tr;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = tr / r;
        Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
    }
}

/// Cover image: shimmer while loading, then fades in.
fn cover(ui: &Ui, images: &mut Images, url: &str, referer: &str, rect: Rect, radius: u8) {
    let tex = (!url.is_empty()).then(|| images.get(url, referer).map(|t| (t.id(), t.size_vec2()))).flatten();
    let p = ui.painter();
    match tex {
        Some((id, size)) => {
            let a = ui.ctx().animate_bool_with_time(egui::Id::new(("img_in", url)), true, 0.35);
            p.rect_filled(rect, CornerRadius::same(radius), PANEL);
            p.add(egui::Shape::Rect(
                egui::epaint::RectShape::filled(rect, CornerRadius::same(radius), Color32::WHITE.gamma_multiply(a)).with_texture(id, cover_uv(size, rect.size())),
            ));
        }
        None if url.is_empty() || images.failed(url) => {
            p.rect_filled(rect, CornerRadius::same(radius), PANEL);
            p.text(rect.center(), Align2::CENTER_CENTER, icon::FILM_SLATE, regular(rect.height() * 0.22), DIM);
        }
        None => shimmer(ui, rect, radius),
    }
}

fn wrapped(ui: &Ui, text: &str, font: egui::FontId, color: Color32, width: f32, rows: usize) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_string(), font, color, width);
    job.wrap = egui::text::TextWrapping { max_rows: rows, break_anywhere: true, overflow_character: Some('…'), max_width: width };
    ui.fonts_mut(|f| f.layout_job(job))
}

/// Video card: cards pop in one after another; on hover the cover lifts, zooms a little, darkens and a
/// play button grows in, with a pink outline.
fn video_card(ui: &mut Ui, images: &mut Images, v: &Video, index: usize) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(CARD_W, CARD_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    // staggered entrance
    let now = ui.input(|i| i.time); // read before data_mut: both lock the context
    let born = ui.ctx().data_mut(|d| *d.get_temp_mut_or_insert_with(resp.id.with("born"), || now));
    // index % 24: cards appended deep in the Discover feed must not wait seconds for their turn
    let k = (((now - born - (index % 24) as f64 * 0.03) / 0.35) as f32).clamp(0.0, 1.0);
    if k < 1.0 {
        ui.ctx().request_repaint();
    }
    let enter = 1.0 - (1.0 - k).powi(3);
    let h = ui.ctx().animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.16);
    // lifts on hover, slides up while entering
    let crect = Rect::from_min_size(rect.min - vec2(0.0, 5.0 * h) + vec2(0.0, (1.0 - enter) * 14.0), vec2(CARD_W, COVER_H));
    let p = ui.painter().clone();
    let alpha = enter;
    if h > 0.0 {
        // pink glow under the lifted card
        p.rect_filled(crect.translate(vec2(0.0, 8.0)).expand(3.0 * h), CornerRadius::same(12), ACCENT.gamma_multiply(0.12 * h));
    }
    // a child that takes no layout space (ui.scope would push the next cards down)
    let mut layer = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    layer.multiply_opacity(alpha);
    cover(&layer, images, &v.cover, &v.referer(), crect.expand(3.0 * h), 8);
    if h > 0.0 {
        let r = crect.expand(3.0 * h);
        p.rect_filled(r, CornerRadius::same(8), Color32::from_black_alpha((100.0 * h) as u8));
        p.circle_filled(r.center(), 16.0 + 10.0 * h, ACCENT.gamma_multiply(h));
        p.text(r.center() + vec2(1.5, 0.0), Align2::CENTER_CENTER, icon::PLAY, regular(14.0 + 8.0 * h), Color32::WHITE.gamma_multiply(h));
        p.rect_stroke(r, CornerRadius::same(8), Stroke::new(2.0, ACCENT.gamma_multiply(h)), StrokeKind::Inside);
    }
    if !v.duration.is_empty() {
        let g = wrapped(ui, &format!("{} {}", icon::CLOCK, v.duration), regular(11.5), Color32::WHITE, CARD_W, 1);
        let br = Rect::from_min_size(crect.right_bottom() - g.size() - vec2(16.0, 12.0), g.size() + vec2(10.0, 4.0));
        p.rect_filled(br, CornerRadius::same(4), Color32::from_black_alpha((190.0 * alpha) as u8));
        p.galley(br.min + vec2(5.0, 2.0), g, Color32::WHITE);
    }
    if !v.code.is_empty() {
        let g = wrapped(ui, &v.code, bold(11.5), Color32::WHITE, CARD_W, 1);
        let br = Rect::from_min_size(crect.min + vec2(8.0, 8.0), g.size() + vec2(10.0, 4.0));
        p.rect_filled(br, CornerRadius::same(4), ACCENT.gamma_multiply(alpha));
        p.galley(br.min + vec2(5.0, 2.0), g, Color32::WHITE);
    }
    let col = lerp_color(TEXT, ACCENT_HI, h).gamma_multiply(alpha);
    let tg = wrapped(ui, &v.title, regular(14.0), col, CARD_W, 2);
    p.galley(pos2(rect.min.x, rect.min.y + COVER_H + 8.0), tg, col);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(&v.title)
}

/// Cards with a heart each (My List). Returns (opened, heart clicked).
fn cards<'a>(ui: &mut Ui, images: &mut Images, st: &Settings, list: impl Iterator<Item = &'a Video>) -> (Option<Video>, Option<Video>) {
    let (mut open, mut fav) = (None, None);
    for (i, v) in list.enumerate() {
        let resp = video_card(ui, images, v, i);
        if heart(ui, &resp, st.is_fav(&v.code)) {
            fav = Some(v.clone());
        } else if resp.clicked() {
            open = Some(v.clone());
        }
    }
    (open, fav)
}

/// Heart at the top right of a card's cover: always shown (filled) when saved, on hover otherwise.
fn heart(ui: &mut Ui, card: &Response, saved: bool) -> bool {
    if !ui.is_rect_visible(card.rect) {
        return false;
    }
    let c = pos2(card.rect.min.x + CARD_W - 18.0, card.rect.min.y + 18.0);
    let r = ui.interact(Rect::from_center_size(c, vec2(26.0, 26.0)), card.id.with("heart"), Sense::click());
    let h = ui.ctx().animate_bool_with_time(r.id, r.hovered(), 0.12);
    if saved || card.hovered() || r.hovered() {
        let p = ui.painter();
        let bg = if saved { ACCENT } else { lerp_color(Color32::from_black_alpha(190), ACCENT, h) };
        p.circle_filled(c, 13.0 + 1.5 * h, bg);
        p.text(c + vec2(0.0, 1.0), Align2::CENTER_CENTER, icon::HEART, regular(14.0 + h), Color32::WHITE);
    }
    r.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(if saved { "Remove from My List" } else { "Add to My List" }).clicked()
}

/// Continue watching card: a video card with how far it was watched. Returns (card, remove clicked).
fn watched_card(ui: &mut Ui, images: &mut Images, w: &crate::settings::Watched, index: usize) -> (Response, bool) {
    let resp = video_card(ui, images, &w.video, index);
    if !ui.is_rect_visible(resp.rect) {
        return (resp, false);
    }
    let cover = Rect::from_min_size(resp.rect.min, vec2(CARD_W, COVER_H));
    let p = ui.painter();
    // progress bar along the bottom of the cover
    let bar = Rect::from_min_max(pos2(cover.min.x + 8.0, cover.max.y - 9.0), pos2(cover.max.x - 8.0, cover.max.y - 5.0));
    p.rect_filled(bar, CornerRadius::same(2), Color32::from_white_alpha(60));
    p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * w.progress(), bar.height())), CornerRadius::same(2), ACCENT);
    let left = ((w.dur - w.pos).max(0.0) / 60.0).round() as u32;
    let label = if w.finished() { format!("{} Watched", icon::CHECK) } else { format!("{} {left} min left", icon::PLAY) };
    let g = wrapped(ui, &label, bold(11.5), Color32::WHITE, CARD_W, 1);
    let br = Rect::from_min_size(pos2(cover.min.x + 8.0, cover.max.y - 16.0 - g.size().y), g.size() + vec2(10.0, 4.0));
    p.rect_filled(br, CornerRadius::same(4), Color32::from_black_alpha(190));
    p.galley(br.min + vec2(5.0, 2.0), g, Color32::WHITE);
    // remove button, shown on hover
    let x = Rect::from_center_size(pos2(cover.max.x - 18.0, cover.min.y + 18.0), vec2(24.0, 24.0));
    let xr = ui.interact(x, resp.id.with("remove"), Sense::click());
    let mut removed = false;
    if resp.hovered() || xr.hovered() {
        let h = if xr.hovered() { 1.0 } else { 0.0 };
        p.circle_filled(x.center(), 12.0, lerp_color(Color32::from_black_alpha(200), ACCENT, h));
        p.text(x.center(), Align2::CENTER_CENTER, icon::X, regular(13.0), Color32::WHITE);
        removed = xr.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Remove from Continue watching").clicked();
    }
    (resp, removed)
}

/// Section heading on Home; returns true when its "See all" link was clicked.
fn section(ui: &mut Ui, glyph: &str, title: &str, link: Option<&str>) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(glyph).size(20.0).color(ACCENT));
        ui.label(RichText::new(title).font(bold(19.0)));
        if let Some(l) = link {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.add(egui::Label::new(RichText::new(format!("{l}  {}", icon::CARET_RIGHT)).color(MUTED)).sense(Sense::click()));
                if r.hovered() {
                    ui.painter().hline(r.rect.x_range(), r.rect.max.y, Stroke::new(1.0, ACCENT_HI));
                }
                clicked = r.on_hover_cursor(CursorIcon::PointingHand).clicked();
            });
        }
    });
    ui.add_space(10.0);
    clicked
}

fn card_skeleton(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(CARD_W, CARD_H), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    shimmer(ui, Rect::from_min_size(rect.min, vec2(CARD_W, COVER_H)), 8);
    shimmer(ui, Rect::from_min_size(rect.min + vec2(0.0, COVER_H + 10.0), vec2(CARD_W * 0.9, 12.0)), 4);
    shimmer(ui, Rect::from_min_size(rect.min + vec2(0.0, COVER_H + 28.0), vec2(CARD_W * 0.55, 10.0)), 4);
}

/// Sidebar entry: icon over a small label; highlight fades in on hover / when active.
fn sidebar_item(ui: &mut Ui, glyph: &str, label: &str, active: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(64.0, 58.0), Sense::click());
    let a = ui.ctx().animate_bool_with_time(resp.id.with("act"), active, 0.18);
    let h = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
    let p = ui.painter();
    if a > 0.0 {
        p.rect_filled(rect.shrink(4.0 * (1.0 - a)), CornerRadius::same(12), SELECT.gamma_multiply(a));
    }
    if h > 0.0 && !active {
        p.rect_filled(rect, CornerRadius::same(12), PANEL.gamma_multiply(h));
    }
    let col = lerp_color(lerp_color(MUTED, TEXT, h), ACCENT_HI, a);
    p.text(rect.center() - vec2(0.0, 8.0 + 2.0 * h), Align2::CENTER_CENTER, glyph, regular(22.0), col);
    p.text(rect.center() + vec2(0.0, 15.0), Align2::CENTER_CENTER, label, regular(11.0), col);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// App logo: a pink disc with a slowly turning ring and a play mark.
fn logo(p: &egui::Painter, c: egui::Pos2, r: f32, t: f32) {
    p.circle_filled(c, r + 3.0, Color32::from_rgb(0x3d, 0x14, 0x2a));
    let n = 40;
    let pts: Vec<_> = (0..n)
        .map(|i| {
            let a = t * 0.8 + i as f32 / n as f32 * std::f32::consts::TAU * 0.7;
            c + vec2(a.cos(), a.sin()) * (r + 2.0)
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(2.0, ACCENT_HI)));
    p.circle_filled(c, r - 2.0, ACCENT);
    p.text(c + vec2(1.5, 0.0), Align2::CENTER_CENTER, icon::PLAY, regular(r * 0.95), Color32::WHITE);
}

fn chip(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let g = wrapped(ui, text, regular(13.5), TEXT, 400.0, 1);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(26.0, 14.0), Sense::click());
    let a = ui.ctx().animate_bool_with_time(resp.id.with("sel"), selected, 0.2);
    let h = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(255), lerp_color(lerp_color(PANEL, PANEL_HI, h), ACCENT, a));
    p.rect_stroke(rect, CornerRadius::same(255), Stroke::new(1.0, lerp_color(LINE, ACCENT_HI, a.max(h * 0.6))), StrokeKind::Inside);
    p.galley(rect.min + vec2(13.0, 7.0), g, Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Smaller chip for options and links.
fn small_chip(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let g = wrapped(ui, text, regular(12.5), TEXT, 400.0, 1);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(20.0, 10.0), Sense::click());
    let a = ui.ctx().animate_bool_with_time(resp.id.with("sel"), selected, 0.16);
    let h = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(255), lerp_color(lerp_color(PANEL, PANEL_HI, h), ACCENT, a));
    p.rect_stroke(rect, CornerRadius::same(255), Stroke::new(1.0, lerp_color(LINE, ACCENT_HI, a.max(h * 0.6))), StrokeKind::Inside);
    p.galley(rect.min + vec2(10.0, 5.0), g, lerp_color(TEXT, Color32::WHITE, a));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn accent_button(ui: &mut Ui, glyph: &str, text: &str) -> Response {
    let g = wrapped(ui, &format!("{glyph}  {text}"), bold(15.0), Color32::WHITE, 400.0, 1);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(36.0, 20.0), Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.14);
    let press = if resp.is_pointer_button_down_on() { 0.96 } else { 1.0 };
    let r = Rect::from_center_size(rect.center(), rect.size() * press);
    let p = ui.painter();
    if h > 0.0 {
        p.rect_filled(r.expand(4.0 * h), CornerRadius::same(255), ACCENT.gamma_multiply(0.2 * h));
    }
    p.rect_filled(r, CornerRadius::same(255), lerp_color(ACCENT, ACCENT_HI, h));
    p.galley(r.center() - g.size() / 2.0, g, Color32::WHITE);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn ghost_button(ui: &mut Ui, glyph: &str, text: &str) -> Response {
    let g = wrapped(ui, &format!("{glyph}  {text}"), regular(14.5), TEXT, 400.0, 1);
    let (rect, resp) = ui.allocate_exact_size(g.size() + vec2(28.0, 16.0), Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.14);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(255), lerp_color(PANEL, SELECT, h));
    p.rect_stroke(rect, CornerRadius::same(255), Stroke::new(1.0, lerp_color(LINE, ACCENT, h)), StrokeKind::Inside);
    p.galley(rect.center() - g.size() / 2.0, g, lerp_color(TEXT, ACCENT_HI, h));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Rounded panel around a settings section.
fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::new().fill(CARD).stroke(Stroke::new(1.0, LINE)).corner_radius(CornerRadius::same(12)).inner_margin(Margin::same(18)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn badge(ui: &mut Ui, text: &str) {
    Frame::new().fill(ACCENT).corner_radius(CornerRadius::same(4)).inner_margin(Margin::symmetric(8, 3)).show(ui, |ui| {
        ui.label(RichText::new(text).font(bold(13.0)).color(Color32::WHITE));
    });
}

fn error_box(ui: &mut Ui, text: &str) {
    Frame::new()
        .fill(Color32::from_rgb(0x2a, 0x12, 0x1c))
        .stroke(Stroke::new(1.0, BAD))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.label(RichText::new(format!("{}  {text}", icon::WARNING)).color(Color32::from_rgb(0xff, 0xb4, 0xc8)));
        });
}

fn empty(ui: &mut Ui, glyph: &str, text: &str) {
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        let t = ui.input(|i| i.time) as f32;
        ui.add_space((t * 2.0).sin() * 4.0 + 4.0); // gentle float
        ui.label(RichText::new(glyph).size(54.0).color(SELECT.gamma_multiply(2.0)));
        ui.label(RichText::new(text).color(MUTED));
        ui.ctx().request_repaint();
    });
}
