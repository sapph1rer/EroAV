//! User settings (settings.json in the data folder) and the rmp player options built from them.

use serde::{Deserialize, Serialize};
use crate::source::Video;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ui_scale: f32,
    /// "auto" | "mpv" | "native"
    pub engine: String,
    /// mpv hwdec: auto-safe | auto | d3d11va | d3d11va-copy | nvdec | no
    pub hwdec: String,
    /// mpv render quality: fast | balanced | high
    pub quality: String,
    pub smooth_motion: bool,
    pub deband: bool,
    /// mix surround down to stereo
    pub stereo: bool,
    pub start_fullscreen: bool,
    pub seek_step: f64,
    /// last volume / mute set in the player
    pub volume: f64,
    pub muted: bool,
    /// start a video where it was left
    pub resume: bool,
    /// Continue watching, most recent first
    pub watching: Vec<Watched>,
    /// every video played (finished ones too), most recent first
    pub history: Vec<Watched>,
    /// favourites, last added first
    pub my_list: Vec<Video>,
}

/// A video left part-way through.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Watched {
    pub video: Video,
    /// seconds
    pub pos: f64,
    pub dur: f64,
}

impl Watched {
    /// reached the credits (or the end)
    pub fn finished(&self) -> bool {
        self.dur > 0.0 && self.pos > self.dur - 120.0
    }

    /// 0..1 of the video seen
    pub fn progress(&self) -> f32 {
        if self.dur > 0.0 { (self.pos / self.dur).clamp(0.0, 1.0) as f32 } else { 0.0 }
    }
}

impl Default for Settings {
    fn default() -> Self {
        let m = rmp_egui::MpvConfig::default();
        Settings {
            ui_scale: 1.0,
            engine: "auto".into(),
            hwdec: m.hwdec,
            quality: m.quality,
            smooth_motion: m.smooth_motion,
            deband: m.deband,
            stereo: false,
            start_fullscreen: false,
            seek_step: 10.0,
            volume: 100.0,
            muted: false,
            resume: true,
            watching: Vec::new(),
            history: Vec::new(),
            my_list: Vec::new(),
        }
    }
}

fn file() -> std::path::PathBuf {
    crate::images::data_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::create_dir_all(crate::images::data_dir());
            let _ = std::fs::write(file(), b);
        }
    }

    /// Where to start `code` (0 = beginning).
    pub fn resume_pos(&self, code: &str) -> f64 {
        if !self.resume {
            return 0.0;
        }
        self.watching.iter().find(|w| w.video.code == code).map_or(0.0, |w| w.pos)
    }

    /// Player progress: the video moves to the front of Continue watching. The first 30 s and the last
    /// 2 minutes (credits) do not count: such a video leaves the list.
    pub fn watched(&mut self, v: &Video, pos: f64, dur: f64) {
        self.history.retain(|w| w.video.code != v.code);
        self.history.insert(0, Watched { video: v.clone(), pos, dur });
        self.history.truncate(300);
        self.watching.retain(|w| w.video.code != v.code);
        if pos >= 30.0 && !(dur > 0.0 && pos > dur - 120.0) {
            self.watching.insert(0, Watched { video: v.clone(), pos, dur });
            self.watching.truncate(40);
        }
    }

    pub fn is_fav(&self, code: &str) -> bool {
        self.my_list.iter().any(|v| v.code == code)
    }

    /// Add to / remove from My List.
    pub fn toggle_fav(&mut self, v: &Video) {
        if self.is_fav(&v.code) {
            self.my_list.retain(|x| x.code != v.code);
        } else {
            self.my_list.insert(0, v.clone());
        }
        self.save();
    }

    /// Player options for rmp (theme is set by the caller).
    pub fn view(&self) -> rmp_egui::ViewSettings {
        let mut s = rmp_egui::ViewSettings::default();
        s.engine = match self.engine.as_str() {
            "mpv" => rmp_egui::EngineKind::Mpv,
            "native" => rmp_egui::EngineKind::Native,
            _ => rmp_egui::EngineKind::Auto,
        };
        s.mpv.hwdec = self.hwdec.clone();
        s.mpv.quality = self.quality.clone();
        s.mpv.smooth_motion = self.smooth_motion;
        s.mpv.deband = self.deband;
        s.mpv.audio_channels = if self.stereo { "stereo".into() } else { "auto".into() };
        s.start_fullscreen = self.start_fullscreen;
        s.seek_step = self.seek_step;
        s.volume = self.volume;
        s.muted = self.muted;
        s.autoplay_next = false;
        if let Some(p) = crate::dns::proxy_url() {
            // the video host's name is resolved over DNS-over-HTTPS too (src/dns.rs)
            s.mpv.extra.push(("http-proxy".into(), p));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_watching() {
        let v = |c: &str| Video { code: c.into(), ..Default::default() };
        let mut s = Settings::default();
        s.watched(&v("A"), 600.0, 7200.0);
        s.watched(&v("B"), 10.0, 7200.0); // too early: not listed
        s.watched(&v("C"), 900.0, 7200.0);
        assert_eq!(s.watching.iter().map(|w| w.video.code.as_str()).collect::<Vec<_>>(), ["C", "A"]);
        assert_eq!(s.resume_pos("A"), 600.0);
        s.watched(&v("A"), 7150.0, 7200.0); // credits: finished, leaves the list
        assert_eq!(s.watching.len(), 1);
        assert_eq!(s.resume_pos("A"), 0.0);
        // history keeps everything, finished first in line
        assert_eq!(s.history.iter().map(|w| w.video.code.as_str()).collect::<Vec<_>>(), ["A", "C", "B"]);
        assert!(s.history[0].finished());
    }
}
