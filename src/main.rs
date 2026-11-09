fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("KeybindChanger")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "KeybindChanger",
        options,
        Box::new(|_cc| Ok(Box::new(KeybindChangerApp::default()))),
    )
}

#[derive(Default)]
struct KeybindChangerApp;

impl eframe::App for KeybindChangerApp {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        eframe::egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("KeybindChanger");
        });
    }
}
