// Hide the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod auth;
mod theme;
mod db;
mod net;
mod updater;

use std::path::PathBuf;

fn data_dir() -> PathBuf {
    // %APPDATA%\OffSales\POS\data on Windows. Survives app updates.
    directories::ProjectDirs::from("", "OffSales", "POS")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn main() -> eframe::Result<()> {
    // `--db <path>` lets you point at a different database (e.g. for testing).
    let args: Vec<String> = std::env::args().collect();
    let db_path = args
        .iter()
        .position(|a| a == "--db")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir().join("pos.db"));

    // `--tab reports` opens on a given page (sell, products, sales, reports, settings).
    let start_tab = args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).cloned();

    let mut viewport = eframe::egui::ViewportBuilder::default();
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport: viewport
            .with_title(format!("Off Sales POS v{}", updater::CURRENT_VERSION))
            .with_min_inner_size([560.0, 520.0])
            .with_maximized(true),
        ..Default::default()
    };

    eframe::run_native(
        "Off Sales POS",
        options,
        Box::new(move |cc| Ok(Box::new(app::PosApp::new(cc, db_path, start_tab.as_deref())))),
    )
}
