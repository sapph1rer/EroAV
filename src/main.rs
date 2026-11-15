//! EroAV — desktop AV viewer (Rust + egui): lists videos from MissAV mirrors and plays them in-app.
//!
//!   eroav                  open the GUI
//!   eroav --probe [query]  console test: merged lists (and Discover feeds), then the stream link of the first video

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod browser;
mod dns;
mod images;
mod settings;
mod source;
#[allow(dead_code)]
mod theme;

fn probe(query: &str) {
    let mut paths = vec![source::list_path(query)];
    if query.is_empty() {
        paths.extend(source::DISCOVER.iter().map(|s| s.to_string()));
    }
    for path in paths {
        println!("
=== {path}");
        match source::merged(&path, 1) {
            Ok(v) => {
                println!("{} video(s)", v.len());
                for x in v.iter().take(4) {
                    println!("  [{}] {} | {} | {}", x.source().name, x.code, x.duration, x.title);
                }
            }
            Err(e) => println!("error: {e}"),
        }
    }
    if let Ok(v) = source::merged(&source::list_path(query), 1)
        && let Some(first) = v.first()
    {
        match first.source().detail(first) {
            Ok(d) => println!("
detail: {}
  stream: {:?}
  info: {:?}", d.video.title, d.stream, d.info),
            Err(e) => println!("detail error: {e}"),
        }
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--probe") {
        #[cfg(windows)]
        {
            // release builds have no console: borrow the parent's
            unsafe extern "system" {
                fn AttachConsole(pid: u32) -> i32;
            }
            unsafe { AttachConsole(u32::MAX) };
        }
        probe(&args[1..].join(" "));
        return Ok(());
    }
    let options = eframe::NativeOptions {
        // glow: the built-in player (libmpv) draws through OpenGL
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("EroAV")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([900.0, 600.0])
            .with_icon(app_icon()),
        ..Default::default()
    };
    eframe::run_native("EroAV", options, Box::new(|cc| Ok(Box::new(app::EroApp::new(cc)))))
}

/// Window icon drawn in code: pink disc with a white play triangle.
fn app_icon() -> egui::IconData {
    let n = 64u32;
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    let c = n as f32 / 2.0;
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
            let inside = (fx * fx + fy * fy).sqrt() < c - 1.0;
            // triangle pointing right: x from -10..14, |y| < (14 - x) * 0.6
            let tri = fx > -10.0 && fx < 15.0 && fy.abs() < (15.0 - fx) * 0.55;
            let px = match (inside, tri) {
                (true, true) => [255, 255, 255, 255],
                (true, false) => [255, 61, 139, 255],
                _ => [0, 0, 0, 0],
            };
            rgba.extend_from_slice(&px);
        }
    }
    egui::IconData { rgba, width: n, height: n }
}
