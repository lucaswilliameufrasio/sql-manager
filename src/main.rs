use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "SQL Manager",
        options,
        Box::new(|_creation_context| Ok(Box::<SqlManagerApp>::default())),
    )
}

#[derive(Default)]
struct SqlManagerApp;

impl eframe::App for SqlManagerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.heading("SQL Manager");
        ui.label("A lightweight desktop SQL client, starting with PostgreSQL.");
        ui.separator();
        ui.label("The connection workspace is coming in the next milestone.");
    }
}
