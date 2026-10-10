#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod git;
mod store;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Agency Desktop")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([900.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Agency Desktop",
        options,
        Box::new(|cc| Ok(Box::new(app::AgencyApp::new(cc)))),
    )
}
