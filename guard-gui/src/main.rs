//! `guard-gui [--repo <path>]`: the desktop window over the guard binary.

// A release build is a windowed app: no console window behind it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use guard_gui::app::{GuardApp, Launch};
use guard_gui::args::{self, ArgsError};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let options = match args::parse_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(ArgsError::Help) => {
            println!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("guard")
        .with_inner_size([1360.0, 860.0])
        .with_min_inner_size([760.0, 480.0]);
    let mut native = eframe::NativeOptions::default();
    if let Some(shot) = &options.screenshot {
        let [w, h] = shot.size;
        viewport = viewport
            .with_inner_size([w as f32, h as f32])
            .with_resizable(false);
        // The harness keeps the user's saved recent projects and layout out of
        // reach: its own store, next to the PNG.
        native.persist_window = false;
        let dir = shot
            .path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.clone());
        native.persistence_path = Some(dir.join("guard-gui-screenshot.ron"));
    }
    native.viewport = viewport;
    let launch = Launch {
        repo: options.repo,
        cwd,
        screenshot: options.screenshot,
    };
    let result = eframe::run_native(
        "guard",
        native,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(GuardApp::new(cc, launch)))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: cannot open the window: {e}");
            ExitCode::from(3)
        }
    }
}
