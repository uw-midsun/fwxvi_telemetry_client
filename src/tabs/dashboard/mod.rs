// Phase 2 — dashboard charts and panel editor
// Stub for Phase 1 compilation.

pub struct DashboardTab;

impl DashboardTab {
    pub fn new() -> Self { Self }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.centered_and_justified(|ui| {
            ui.label("Dashboard — coming in Phase 2");
        });
    }
}
