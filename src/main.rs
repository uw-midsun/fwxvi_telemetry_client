mod app;
mod config;
mod db;
mod decoder;
mod replay;
mod tabs;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("FWXVI Telemetry Client")
            .with_inner_size([1100.0, 650.0])
            .with_min_inner_size([700.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "FWXVI Telemetry Client",
        options,
        Box::new(|cc| Ok(Box::new(app::TelemetryApp::new(cc)))),
    )
}
