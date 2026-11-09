//! GTK4 user interface.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;

use gtk::prelude::*;
use gtk::{gio, pango};

use crate::apps::{self, DesktopApp};
use crate::hypr::Store;
use crate::model::{Binding, BindingDraft, DisabledBinding, KeyCombo, Mods, Source, normalize_key};

pub fn run() -> gtk::glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("io.github.culcat.KeybindChanger")
        .build();
    app.connect_activate(build_ui);
    app.run()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    All,
    Applications,
    WindowManagement,
    Custom,
    Disabled,
}

impl Filter {
    fn from_index(index: u32) -> Self {
        match index {
            1 => Filter::Applications,
            2 => Filter::WindowManagement,
            3 => Filter::Custom,
            4 => Filter::Disabled,
            _ => Filter::All,
        }
    }

    fn matches(self, binding: &Binding) -> bool {
        match self {
            Filter::All => true,
            Filter::Applications => binding.is_application(),
            Filter::WindowManagement => !binding.is_application(),
            Filter::Custom => binding.source == Source::Managed,
            Filter::Disabled => false,
        }
    }
}

struct Ui {
    store: Store,
    search: String,
    filter: Filter,
    apps: Option<Vec<DesktopApp>>,
}

#[derive(Clone)]
struct Ctx {
    ui: Rc<RefCell<Ui>>,
    window: gtk::ApplicationWindow,
    list: gtk::ListBox,
    count: gtk::Label,
    status: gtk::Label,
}

enum RowData {
    Enabled(Binding),
    Disabled(DisabledBinding),
}

impl Ctx {
    fn set_status(&self, text: impl Into<String>) {
        self.status.set_text(&text.into());
    }

    fn refresh(&self) {
        let (items, total, summary, filter_disabled) = {
            let ui = self.ui.borrow();
            let query = ui.search.trim().to_lowercase();
            let mut items = Vec::new();

            if ui.filter != Filter::Disabled {
                for binding in &ui.store.bindings {
                    if !ui.filter.matches(binding) {
                        continue;
                    }
                    if !query.is_empty() && !binding_matches(binding, &query) {
                        continue;
                    }
                    items.push(RowData::Enabled(binding.clone()));
                }
            }

            let include_disabled = matches!(ui.filter, Filter::All | Filter::Disabled);
            if include_disabled {
                for disabled in &ui.store.disabled {
                    if !query.is_empty() && !disabled_matches(disabled, &query) {
                        continue;
                    }
                    items.push(RowData::Disabled(disabled.clone()));
                }
            }

            (
                items,
                ui.store.bindings.len() + ui.store.disabled.len(),
                ui.store.config_summary(),
                ui.filter == Filter::Disabled,
            )
        };

        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        if items.is_empty() {
            let empty = gtk::ListBoxRow::new();
            empty.set_activatable(false);
            let label = gtk::Label::new(Some(if filter_disabled {
                "No disabled bindings."
            } else {
                "No bindings match your search."
            }));
            label.add_css_class("dim-label");
            label.set_margin_top(36);
            label.set_margin_bottom(36);
            empty.set_child(Some(&label));
            self.list.append(&empty);
        }

        let shown = items.len();
        for item in items {
            match item {
                RowData::Enabled(binding) => {
                    let row = build_binding_row(self, binding);
                    self.list.append(&row);
                }
                RowData::Disabled(disabled) => {
                    let row = build_disabled_row(self, disabled);
                    self.list.append(&row);
                }
            }
        }

        self.count.set_text(&format!("{shown} / {total} bindings"));
        self.count.set_tooltip_text(Some(&summary));
    }

    fn reload_from_disk(&self, note: &str) {
        let (store, reload_status) = {
            let running = crate::hypr::hyprland_running();
            let ui = self.ui.borrow_mut();
            if running {
                let result = ui.store.reload();
                (Store::load(), Some(result))
            } else {
                let store = Store::load_offline(ui.store.paths.clone());
                (store, None)
            }
        };

        let mut status = match reload_status {
            Some(Ok(message)) if note.is_empty() => message,
            Some(Ok(message)) => format!("{note} — {message}"),
            Some(Err(error)) if note.is_empty() => format!("reload failed: {error}"),
            Some(Err(error)) => format!("{note} — reload failed: {error}"),
            None if note.is_empty() => "Configuration reloaded".to_string(),
            None => note.to_string(),
        };
        if status.trim().is_empty() {
            status = "Reloaded configuration".to_string();
        }

        self.ui.borrow_mut().store = store;
        self.refresh();
        self.set_status(status);
    }

    fn apply_save(&self, managed_id: Option<String>, draft: &BindingDraft) -> Result<(), String> {
        let result = {
            let ui = self.ui.borrow();
            ui.store.save_override(managed_id.as_deref(), draft)
        };
        match result {
            Ok(_) => {
                self.reload_from_disk("Binding saved");
                Ok(())
            }
            Err(error) => Err(format!("Could not save the binding: {error}")),
        }
    }

    fn apply_remove(&self, managed_id: &str) -> Result<(), String> {
        let result = {
            let ui = self.ui.borrow();
            ui.store.remove_override(managed_id)
        };
        match result {
            Ok(()) => {
                self.reload_from_disk("Override removed");
                Ok(())
            }
            Err(error) => Err(format!("Could not remove the override: {error}")),
        }
    }

    fn apply_disable(&self, binding: &Binding) -> Result<(), String> {
        let result = {
            let ui = self.ui.borrow();
            ui.store.disable(binding)
        };
        match result {
            Ok(_) => {
                self.reload_from_disk("Binding disabled");
                Ok(())
            }
            Err(error) => Err(format!("Could not disable the binding: {error}")),
        }
    }

    fn apply_enable(&self, disabled: &DisabledBinding) -> Result<(), String> {
        let result = {
            let ui = self.ui.borrow();
            ui.store.enable(disabled)
        };
        match result {
            Ok(_) => {
                self.reload_from_disk("Binding restored");
                Ok(())
            }
            Err(error) => Err(format!("Could not restore the binding: {error}")),
        }
    }
}

fn binding_matches(binding: &Binding, query: &str) -> bool {
    binding.keys.canonical().to_lowercase().contains(query)
        || binding.description.to_lowercase().contains(query)
        || binding
            .command
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains(query)
        || binding
            .raw_dispatcher
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains(query)
}

fn disabled_matches(binding: &DisabledBinding, query: &str) -> bool {
    binding.keys.canonical().to_lowercase().contains(query)
        || binding.description.to_lowercase().contains(query)
        || binding
            .command
            .as_deref()
            .unwrap_or_default()
            .to_lowercase()
            .contains(query)
}

fn build_binding_row(ctx: &Ctx, binding: Binding) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_tooltip_text(
        binding
            .file
            .as_ref()
            .map(|p| p.display().to_string())
            .as_deref(),
    );

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hbox.set_margin_top(10);
    hbox.set_margin_bottom(10);
    hbox.set_margin_start(14);
    hbox.set_margin_end(14);

    let keys = gtk::Label::new(Some(&binding.keys.canonical()));
    keys.add_css_class("monospace");
    keys.set_xalign(0.0);
    keys.set_width_chars(30);
    hbox.append(&keys);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);

    let description = if binding.description.trim().is_empty() {
        "Untitled binding".to_string()
    } else {
        binding.description.clone()
    };
    let desc = gtk::Label::new(Some(&description));
    desc.set_xalign(0.0);
    desc.set_ellipsize(pango::EllipsizeMode::End);
    text.append(&desc);

    let command_text = binding
        .command
        .clone()
        .or_else(|| binding.raw_dispatcher.clone())
        .unwrap_or_else(|| "—".to_string());
    let command = gtk::Label::new(Some(&command_text));
    command.set_xalign(0.0);
    command.add_css_class("dim-label");
    command.add_css_class("monospace");
    command.add_css_class("caption");
    command.set_ellipsize(pango::EllipsizeMode::End);
    command.set_tooltip_text(Some(&command_text));
    text.append(&command);
    hbox.append(&text);

    let source = gtk::Label::new(Some(binding.display_source()));
    source.add_css_class("caption");
    source.add_css_class("dim-label");
    if binding.source == Source::Managed {
        source.add_css_class("accent");
    }
    hbox.append(&source);

    let edit = gtk::Button::with_label("Edit");
    edit.add_css_class("flat");
    edit.set_tooltip_text(Some("Edit this keybinding"));
    {
        let ctx = ctx.clone();
        let binding = binding.clone();
        edit.connect_clicked(move |_| open_editor(&ctx, Some(binding.clone())));
    }
    hbox.append(&edit);

    if binding.managed_id.is_some() {
        let revert = gtk::Button::with_label("Revert");
        revert.add_css_class("flat");
        revert.set_tooltip_text(Some("Remove the override and restore the original binding"));
        {
            let ctx = ctx.clone();
            let binding = binding.clone();
            revert.connect_clicked(move |_| confirm_revert(&ctx, &binding));
        }
        hbox.append(&revert);
    }

    let disable = gtk::Button::with_label("Disable");
    disable.add_css_class("flat");
    disable.set_tooltip_text(Some("Unbind this keybinding"));
    {
        let ctx = ctx.clone();
        let binding = binding.clone();
        disable.connect_clicked(move |_| confirm_disable(&ctx, &binding));
    }
    hbox.append(&disable);

    row.set_child(Some(&hbox));
    row
}

fn build_disabled_row(ctx: &Ctx, disabled: DisabledBinding) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    hbox.set_margin_top(10);
    hbox.set_margin_bottom(10);
    hbox.set_margin_start(14);
    hbox.set_margin_end(14);

    let keys = gtk::Label::new(Some(&format!("⊘  {}", disabled.keys.canonical())));
    keys.add_css_class("monospace");
    keys.add_css_class("dim-label");
    keys.set_xalign(0.0);
    keys.set_width_chars(30);
    hbox.append(&keys);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let desc_text = if disabled.description.trim().is_empty() {
        "Disabled binding".to_string()
    } else {
        disabled.description.clone()
    };
    let desc = gtk::Label::new(Some(&desc_text));
    desc.set_xalign(0.0);
    desc.add_css_class("dim-label");
    desc.set_ellipsize(pango::EllipsizeMode::End);
    text.append(&desc);

    let command_text = disabled
        .command
        .clone()
        .unwrap_or_else(|| "No saved command".to_string());
    let command = gtk::Label::new(Some(&command_text));
    command.set_xalign(0.0);
    command.add_css_class("dim-label");
    command.add_css_class("monospace");
    command.add_css_class("caption");
    command.set_ellipsize(pango::EllipsizeMode::End);
    text.append(&command);
    hbox.append(&text);

    let badge = gtk::Label::new(Some("disabled"));
    badge.add_css_class("caption");
    badge.add_css_class("dim-label");
    hbox.append(&badge);

    if disabled.command.is_some() {
        let enable = gtk::Button::with_label("Enable");
        enable.add_css_class("flat");
        enable.set_tooltip_text(Some("Re-create this keybinding"));
        {
            let ctx = ctx.clone();
            let disabled = disabled.clone();
            enable.connect_clicked(move |_| {
                if let Err(error) = ctx.apply_enable(&disabled) {
                    ctx.set_status(error);
                }
            });
        }
        hbox.append(&enable);
    }

    row.set_child(Some(&hbox));
    row
}

fn confirm(
    ctx: &Ctx,
    message: &str,
    detail: &str,
    accept_label: &str,
    on_accept: impl FnOnce() + 'static,
) {
    let dialog = gtk::AlertDialog::builder()
        .message(message)
        .detail(detail)
        .buttons(["Cancel", accept_label])
        .cancel_button(0)
        .default_button(0)
        .build();

    dialog.choose(Some(&ctx.window), gio::Cancellable::NONE, move |result| {
        if result == Ok(1) {
            on_accept();
        }
    });
}

fn confirm_disable(ctx: &Ctx, binding: &Binding) {
    let ctx2 = ctx.clone();
    let caller = ctx.clone();
    let binding = binding.clone();
    confirm(
        &caller,
        "Disable this keybinding?",
        &format!(
            "{} will be unbound in your config. You can restore it later from the Disabled filter.",
            binding.keys.canonical()
        ),
        "Disable",
        move || {
            if let Err(error) = ctx2.apply_disable(&binding) {
                ctx2.set_status(error);
            }
        },
    );
}

fn confirm_revert(ctx: &Ctx, binding: &Binding) {
    let Some(id) = binding.managed_id.clone() else {
        return;
    };
    let ctx2 = ctx.clone();
    let caller = ctx.clone();
    confirm(
        &caller,
        "Remove this override?",
        "The original binding will be restored.",
        "Remove",
        move || {
            if let Err(error) = ctx2.apply_remove(&id) {
                ctx2.set_status(error);
            }
        },
    );
}

fn open_editor(ctx: &Ctx, existing: Option<Binding>) {
    let draft = existing
        .as_ref()
        .map(BindingDraft::from_binding)
        .unwrap_or_default();
    let managed_id = existing.as_ref().and_then(|b| b.managed_id.clone());

    let dialog = gtk::Window::builder()
        .title(if existing.is_some() {
            "Edit keybinding"
        } else {
            "Add keybinding"
        })
        .transient_for(&ctx.window)
        .modal(true)
        .default_width(580)
        .build();
    dialog.set_resizable(false);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(20);
    root.set_margin_end(20);
    dialog.set_child(Some(&root));

    let header = gtk::Label::new(Some(if existing.is_some() {
        "Update the hotkey, its description or the command it runs."
    } else {
        "Bind a hotkey to an application or any shell command."
    }));
    header.set_xalign(0.0);
    header.add_css_class("dim-label");
    header.set_wrap(true);
    root.append(&header);

    root.append(&field_label("Hotkey"));
    let hotkey_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let hotkey_entry = gtk::Entry::new();
    hotkey_entry.set_hexpand(true);
    hotkey_entry.set_text(&draft.keys);
    hotkey_entry.set_placeholder_text(Some("SUPER + SHIFT + R"));
    hotkey_box.append(&hotkey_entry);
    let record = gtk::ToggleButton::with_label("Record");
    record.set_tooltip_text(Some("Capture the next key combination"));
    hotkey_box.append(&record);
    root.append(&hotkey_box);

    let hint = gtk::Label::new(Some(
        "Tip: click Record and press the combination you want. Esc cancels recording.",
    ));
    hint.set_xalign(0.0);
    hint.add_css_class("caption");
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    root.append(&hint);

    root.append(&field_label("Description"));
    let description_entry = gtk::Entry::new();
    description_entry.set_text(&draft.description);
    description_entry.set_placeholder_text(Some("e.g. Terminal"));
    root.append(&description_entry);

    root.append(&field_label("Command"));
    let command_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let command_entry = gtk::Entry::new();
    command_entry.set_hexpand(true);
    command_entry.set_text(&draft.command);
    command_entry.set_placeholder_text(Some("foot  —  or any shell command"));
    command_box.append(&command_entry);
    let pick = gtk::Button::with_label("Choose app…");
    command_box.append(&pick);
    root.append(&command_box);

    let flags = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    let mouse = gtk::CheckButton::with_label("Mouse binding");
    mouse.set_active(draft.mouse);
    let release = gtk::CheckButton::with_label("On release");
    release.set_active(draft.release);
    let repeat = gtk::CheckButton::with_label("Repeat while held");
    repeat.set_active(draft.repeat);
    flags.append(&mouse);
    flags.append(&release);
    flags.append(&repeat);
    root.append(&flags);

    let error = gtk::Label::new(None);
    error.set_xalign(0.0);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_visible(false);
    root.append(&error);

    let recording = Rc::new(Cell::new(false));
    {
        let recording = recording.clone();
        let hint = hint.clone();
        record.connect_toggled(move |button| {
            let active = button.is_active();
            recording.set(active);
            if active {
                hint.set_text("Recording… press a key combination. Esc cancels.");
            }
        });
    }
    {
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let recording = recording.clone();
        let record = record.clone();
        let hotkey_entry = hotkey_entry.clone();
        let hint = hint.clone();
        controller.connect_key_pressed(move |_, key, _, state| {
            if !recording.get() {
                return gtk::glib::Propagation::Proceed;
            }
            let Some(name) = key.name() else {
                return gtk::glib::Propagation::Proceed;
            };
            let name = name.as_str();
            if name.eq_ignore_ascii_case("Escape") {
                recording.set(false);
                record.set_active(false);
                hint.set_text("Recording cancelled.");
                return gtk::glib::Propagation::Stop;
            }
            if is_modifier_key(name) {
                return gtk::glib::Propagation::Proceed;
            }
            if let Some(combo) = combo_from_gtk(&key, state) {
                hotkey_entry.set_text(&combo.canonical());
                recording.set(false);
                record.set_active(false);
                hint.set_text("Hotkey captured. You can still type it manually.");
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        dialog.add_controller(controller);
    }

    {
        let ctx = ctx.clone();
        let dialog = dialog.clone();
        let command_entry = command_entry.clone();
        let description_entry = description_entry.clone();
        pick.connect_clicked(move |_| {
            open_app_picker(&ctx, &dialog, &command_entry, &description_entry);
        });
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_margin_top(6);

    if let Some(binding) = &existing {
        if let Some(id) = binding.managed_id.clone() {
            let remove = gtk::Button::with_label("Remove override");
            remove.add_css_class("destructive-action");
            let ctx2 = ctx.clone();
            let dialog2 = dialog.clone();
            remove.connect_clicked(move |_| {
                let ctx3 = ctx2.clone();
                let dialog3 = dialog2.clone();
                let id = id.clone();
                confirm(
                    &ctx2,
                    "Remove this override?",
                    "The original binding will be restored.",
                    "Remove",
                    move || match ctx3.apply_remove(&id) {
                        Ok(()) => dialog3.close(),
                        Err(error) => ctx3.set_status(error),
                    },
                );
            });
            actions.append(&remove);
        } else {
            let disable = gtk::Button::with_label("Disable binding");
            let ctx2 = ctx.clone();
            let dialog2 = dialog.clone();
            let binding = binding.clone();
            disable.connect_clicked(move |_| {
                let ctx3 = ctx2.clone();
                let dialog3 = dialog2.clone();
                let binding = binding.clone();
                confirm(
                    &ctx2,
                    "Disable this keybinding?",
                    &format!(
                        "{} will be unbound in your config.",
                        binding.keys.canonical()
                    ),
                    "Disable",
                    move || match ctx3.apply_disable(&binding) {
                        Ok(()) => dialog3.close(),
                        Err(error) => ctx3.set_status(error),
                    },
                );
            });
            actions.append(&disable);
        }
    }

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&spacer);

    let cancel = gtk::Button::with_label("Cancel");
    {
        let dialog = dialog.clone();
        cancel.connect_clicked(move |_| dialog.close());
    }
    actions.append(&cancel);

    let save = gtk::Button::with_label(if existing.is_some() { "Save" } else { "Add" });
    save.add_css_class("suggested-action");
    {
        let ctx = ctx.clone();
        let dialog = dialog.clone();
        let error = error.clone();
        let hotkey_entry = hotkey_entry.clone();
        let description_entry = description_entry.clone();
        let command_entry = command_entry.clone();
        let mouse = mouse.clone();
        let release = release.clone();
        let repeat = repeat.clone();
        save.connect_clicked(move |_| {
            let draft = BindingDraft {
                keys: hotkey_entry.text().to_string(),
                description: description_entry.text().to_string(),
                command: command_entry.text().to_string(),
                mouse: mouse.is_active(),
                release: release.is_active(),
                repeat: repeat.is_active(),
            };
            match ctx.apply_save(managed_id.clone(), &draft) {
                Ok(()) => dialog.close(),
                Err(message) => {
                    error.set_text(&message);
                    error.set_visible(true);
                }
            }
        });
    }
    actions.append(&save);
    root.append(&actions);

    hotkey_entry.grab_focus();
    dialog.present();
}

fn open_app_picker(
    ctx: &Ctx,
    parent: &gtk::Window,
    command_entry: &gtk::Entry,
    description_entry: &gtk::Entry,
) {
    let apps = {
        let mut ui = ctx.ui.borrow_mut();
        ui.apps.get_or_insert_with(apps::discover).clone()
    };

    let picker = gtk::Window::builder()
        .title("Choose application")
        .transient_for(parent)
        .modal(true)
        .default_width(640)
        .default_height(560)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(14);
    root.set_margin_bottom(14);
    root.set_margin_start(14);
    root.set_margin_end(14);
    picker.set_child(Some(&root));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search installed applications…"));
    root.append(&search);

    let scrolled = gtk::ScrolledWindow::builder().vexpand(true).build();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    scrolled.set_child(Some(&list));
    root.append(&scrolled);

    for app in &apps {
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false);
        row.set_widget_name(&format!(
            "{} {} {} {}",
            app.name, app.comment, app.exec, app.id
        ));

        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        hbox.set_margin_top(8);
        hbox.set_margin_bottom(8);
        hbox.set_margin_start(10);
        hbox.set_margin_end(10);

        let icon =
            gtk::Image::from_icon_name(app.icon.as_deref().unwrap_or("application-x-executable"));
        icon.set_pixel_size(28);
        hbox.append(&icon);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let name = gtk::Label::new(Some(&app.name));
        name.set_xalign(0.0);
        name.set_ellipsize(pango::EllipsizeMode::End);
        text.append(&name);
        let detail = if app.comment.trim().is_empty() {
            app.exec.clone()
        } else {
            format!("{} — {}", app.comment, app.exec)
        };
        let detail = gtk::Label::new(Some(&detail));
        detail.set_xalign(0.0);
        detail.add_css_class("caption");
        detail.add_css_class("dim-label");
        detail.set_ellipsize(pango::EllipsizeMode::End);
        text.append(&detail);
        hbox.append(&text);

        let use_button = gtk::Button::with_label("Use");
        use_button.add_css_class("flat");
        {
            let picker = picker.clone();
            let command_entry = command_entry.clone();
            let description_entry = description_entry.clone();
            let command = app.launch_command();
            let app_name = app.name.clone();
            use_button.connect_clicked(move |_| {
                command_entry.set_text(&command);
                if description_entry.text().trim().is_empty() {
                    description_entry.set_text(&app_name);
                }
                picker.close();
            });
        }
        hbox.append(&use_button);

        row.set_child(Some(&hbox));
        list.append(&row);
    }

    {
        let list = list.clone();
        search.connect_search_changed(move |entry| {
            let query = entry.text().to_lowercase();
            let mut child = list.first_child();
            while let Some(widget) = child {
                let next = widget.next_sibling();
                if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
                    let haystack = row.widget_name().to_lowercase();
                    row.set_visible(query.is_empty() || haystack.contains(&query));
                }
                child = next;
            }
        });
    }

    search.grab_focus();
    picker.present();
}

fn field_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_markup(&format!("<b>{}</b>", gtk::glib::markup_escape_text(text)));
    label.set_xalign(0.0);
    label
}

fn is_modifier_key(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("shift")
        || name.starts_with("control")
        || name.starts_with("alt")
        || name.starts_with("super")
        || name.starts_with("meta")
        || name.starts_with("hyper")
        || name == "caps_lock"
        || name == "num_lock"
        || name == "scroll_lock"
        || name.starts_with("iso_level")
        || name == "mode_switch"
        || name == "voidsymbol"
}

fn combo_from_gtk(key: &gtk::gdk::Key, state: gtk::gdk::ModifierType) -> Option<KeyCombo> {
    let name = key.name()?;
    let name = name.as_str();
    if is_modifier_key(name) {
        return None;
    }

    let super_mask = gtk::gdk::ModifierType::SUPER_MASK
        | gtk::gdk::ModifierType::HYPER_MASK
        | gtk::gdk::ModifierType::META_MASK;
    let mods = Mods {
        super_: state.intersects(super_mask),
        ctrl: state.contains(gtk::gdk::ModifierType::CONTROL_MASK),
        alt: state.contains(gtk::gdk::ModifierType::ALT_MASK),
        shift: state.contains(gtk::gdk::ModifierType::SHIFT_MASK),
    };

    let key = normalize_key(name);
    if key.is_empty() {
        return None;
    }
    Some(KeyCombo::new(mods, key))
}

fn build_ui(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("KeybindChanger")
        .default_width(1120)
        .default_height(760)
        .build();

    let header = gtk::HeaderBar::new();

    let title_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let title = gtk::Label::new(Some("KeybindChanger"));
    title.add_css_class("title");
    let subtitle = gtk::Label::new(Some("Hyprland keybindings"));
    subtitle.add_css_class("subtitle");
    subtitle.add_css_class("dim-label");
    title_box.append(&title);
    title_box.append(&subtitle);
    header.set_title_widget(Some(&title_box));

    let reload_button = gtk::Button::from_icon_name("view-refresh-symbolic");
    reload_button.set_tooltip_text(Some("Reload Hyprland configuration"));
    header.pack_start(&reload_button);

    let add_button = gtk::Button::from_icon_name("list-add-symbolic");
    add_button.add_css_class("suggested-action");
    add_button.set_tooltip_text(Some("Add a keybinding"));
    header.pack_end(&add_button);

    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.set_child(Some(&root));

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    toolbar.set_margin_top(12);
    toolbar.set_margin_bottom(8);
    toolbar.set_margin_start(16);
    toolbar.set_margin_end(16);

    let search = gtk::SearchEntry::new();
    search.set_hexpand(true);
    search.set_placeholder_text(Some("Search hotkeys, descriptions or commands…"));
    toolbar.append(&search);

    let filter = gtk::DropDown::from_strings(&[
        "All",
        "Applications",
        "Window management",
        "Custom",
        "Disabled",
    ]);
    filter.set_tooltip_text(Some("Filter bindings"));
    toolbar.append(&filter);
    root.append(&toolbar);

    let scrolled = gtk::ScrolledWindow::builder().vexpand(true).build();
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    scrolled.set_child(Some(&list));
    root.append(&scrolled);

    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    footer.set_margin_top(8);
    footer.set_margin_bottom(8);
    footer.set_margin_start(16);
    footer.set_margin_end(16);

    let status_label = gtk::Label::new(None);
    status_label.set_xalign(0.0);
    status_label.set_hexpand(true);
    status_label.set_ellipsize(pango::EllipsizeMode::End);
    status_label.add_css_class("caption");
    status_label.add_css_class("dim-label");
    footer.append(&status_label);

    let count = gtk::Label::new(None);
    count.add_css_class("caption");
    count.add_css_class("dim-label");
    footer.append(&count);
    root.append(&footer);

    let store = Store::load();
    let initial_status = if store.live {
        format!("Loaded {} keybindings from Hyprland", store.bindings.len())
    } else if let Some(warning) = store.warnings.first() {
        warning.clone()
    } else {
        "Showing the parsed configuration".to_string()
    };

    let ctx = Ctx {
        ui: Rc::new(RefCell::new(Ui {
            store,
            search: String::new(),
            filter: Filter::All,
            apps: None,
        })),
        window: window.clone(),
        list: list.clone(),
        count: count.clone(),
        status: status_label.clone(),
    };

    {
        let ctx = ctx.clone();
        search.connect_search_changed(move |entry| {
            ctx.ui.borrow_mut().search = entry.text().to_string();
            ctx.refresh();
        });
    }
    {
        let ctx = ctx.clone();
        filter.connect_selected_notify(move |dropdown| {
            ctx.ui.borrow_mut().filter = Filter::from_index(dropdown.selected());
            ctx.refresh();
        });
    }
    {
        let ctx = ctx.clone();
        add_button.connect_clicked(move |_| open_editor(&ctx, None));
    }
    {
        let ctx = ctx.clone();
        reload_button.connect_clicked(move |_| ctx.reload_from_disk("Manual reload"));
    }

    ctx.refresh();
    ctx.set_status(initial_status);
    window.present();
}
