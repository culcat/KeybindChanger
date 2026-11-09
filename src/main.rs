mod app;
mod apps;
mod hypr;
mod model;

use gtk4 as gtk;

fn main() -> gtk::glib::ExitCode {
    app::run()
}
