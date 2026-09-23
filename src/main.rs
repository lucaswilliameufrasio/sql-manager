mod connection;
mod secrets;
mod storage;

use std::sync::mpsc::{self, Receiver};

use connection::{ConnectionDraft, ConnectionProfile, TlsMode, test_connection};
use eframe::egui;
use uuid::Uuid;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "SQL Manager",
        options,
        Box::new(|_creation_context| Ok(Box::<SqlManagerApp>::default())),
    )
}

struct SqlManagerApp {
    profiles: Vec<ConnectionProfile>,
    selected_profile_id: Option<Uuid>,
    draft: ConnectionDraft,
    status: String,
    pending_test: Option<Receiver<String>>,
}

impl Default for SqlManagerApp {
    fn default() -> Self {
        let (profiles, status) = match storage::load_profiles() {
            Ok(profiles) => (profiles, String::from("Ready")),
            Err(error) => (
                Vec::new(),
                format!("Could not load saved connections: {error}"),
            ),
        };

        Self {
            profiles,
            selected_profile_id: None,
            draft: ConnectionDraft::default(),
            status,
            pending_test: None,
        }
    }
}

impl eframe::App for SqlManagerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("header").show(ui, |ui| {
            ui.heading("SQL Manager");
            ui.label("PostgreSQL connections");
        });

        egui::Panel::left("connections")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Connections");
                    if ui.button("+").on_hover_text("New connection").clicked() {
                        self.selected_profile_id = None;
                        self.draft = ConnectionDraft::default();
                        self.status = String::from("New connection");
                    }
                });
                ui.separator();

                let mut selected = self.selected_profile_id;
                for profile in &self.profiles {
                    let response =
                        ui.selectable_value(&mut selected, Some(profile.id), &profile.name);
                    if response.clicked() {
                        self.draft = ConnectionDraft::from(profile);
                        self.draft.password = match secrets::load_password(profile.id) {
                            Ok(Some(password)) => password,
                            Ok(None) => String::new(),
                            Err(error) => {
                                self.status = format!("Could not load saved password: {error}");
                                String::new()
                            }
                        };
                    }
                }
                self.selected_profile_id = selected;
            });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(if self.selected_profile_id.is_some() {
                "Edit connection"
            } else {
                "New connection"
            });
            ui.add_space(8.0);

            egui::Grid::new("connection_form")
                .num_columns(2)
                .spacing([12.0, 10.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.draft.name);
                    ui.end_row();

                    ui.label("Host");
                    ui.text_edit_singleline(&mut self.draft.host);
                    ui.end_row();

                    ui.label("Port");
                    ui.add(egui::TextEdit::singleline(&mut self.draft.port).desired_width(90.0));
                    ui.end_row();

                    ui.label("Database");
                    ui.text_edit_singleline(&mut self.draft.database);
                    ui.end_row();

                    ui.label("Username");
                    ui.text_edit_singleline(&mut self.draft.username);
                    ui.end_row();

                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut self.draft.password).password(true));
                    ui.end_row();

                    ui.label("SSL mode");
                    egui::ComboBox::from_id_salt("ssl_mode")
                        .selected_text(self.draft.tls_mode.label())
                        .show_ui(ui, |ui| {
                            for mode in TlsMode::ALL {
                                ui.selectable_value(&mut self.draft.tls_mode, mode, mode.label());
                            }
                        });
                    ui.end_row();
                });

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Save connection").clicked() {
                    self.save_connection();
                }

                let testing = self.pending_test.is_some();
                if ui
                    .add_enabled(!testing, egui::Button::new("Test connection"))
                    .clicked()
                {
                    self.start_connection_test(ui.ctx());
                }
            });

            ui.add_space(12.0);
            self.refresh_test_status();
            ui.label(&self.status);
        });
    }
}

impl SqlManagerApp {
    fn save_connection(&mut self) {
        let profile = match self.draft.to_profile(self.selected_profile_id) {
            Ok(profile) => profile,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        if !self.draft.password.is_empty()
            && let Err(error) = secrets::save_password(profile.id, &self.draft.password)
        {
            self.status = format!("Could not save password to the system keyring: {error}");
            return;
        }

        if let Err(error) = storage::save_profile(&profile, &mut self.profiles) {
            self.status = format!("Could not save connection: {error}");
            return;
        }

        self.selected_profile_id = Some(profile.id);
        self.status = format!("Saved connection ‘{}’", profile.name);
    }

    fn start_connection_test(&mut self, context: &egui::Context) {
        let profile = match self.draft.to_profile(self.selected_profile_id) {
            Ok(profile) => profile,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let password = if self.draft.password.is_empty() {
            match secrets::load_password(profile.id) {
                Ok(Some(password)) => password,
                Ok(None) => String::new(),
                Err(error) => {
                    self.status =
                        format!("Could not load password from the system keyring: {error}");
                    return;
                }
            }
        } else {
            self.draft.password.clone()
        };

        let (sender, receiver) = mpsc::channel();
        let context = context.clone();
        self.pending_test = Some(receiver);
        self.status = String::from("Testing connection…");

        std::thread::spawn(move || {
            let message = match tokio::runtime::Runtime::new() {
                Ok(runtime) => match runtime.block_on(test_connection(&profile, &password)) {
                    Ok(server_version) => format!("Connected successfully — {server_version}"),
                    Err(error) => format!("Connection failed: {error}"),
                },
                Err(error) => format!("Could not start the async runtime: {error}"),
            };
            let _ = sender.send(message);
            context.request_repaint();
        });
    }

    fn refresh_test_status(&mut self) {
        let Some(receiver) = &self.pending_test else {
            return;
        };

        match receiver.try_recv() {
            Ok(message) => {
                self.status = message;
                self.pending_test = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.status = String::from("Connection test stopped unexpectedly");
                self.pending_test = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}
