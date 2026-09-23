mod backup;
mod connection;
mod database;
mod secrets;
mod storage;

use std::{
    fs,
    sync::mpsc::{self, Receiver},
};

use backup::{decrypt_profiles, encrypt_profiles};
use connection::{ConnectionDraft, ConnectionProfile, TlsMode, test_connection};
use database::{DatabaseSession, Event as DatabaseEvent, QueryOutput};
use eframe::egui;
use rfd::FileDialog;
use uuid::Uuid;
use zeroize::Zeroize;

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
    backup_dialog: Option<BackupDialog>,
    session: Option<DatabaseSession>,
    schemas: Vec<String>,
    tables: Vec<String>,
    selected_schema: Option<String>,
    sql: String,
    query_result: Option<QueryOutput>,
    query_running: bool,
}

#[derive(Clone, Copy)]
enum BackupAction {
    Export,
    Import,
}

struct BackupDialog {
    action: BackupAction,
    password: String,
    confirmation: String,
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
            backup_dialog: None,
            session: None,
            schemas: Vec::new(),
            tables: Vec::new(),
            selected_schema: None,
            sql: String::from("SELECT current_database(), current_user;"),
            query_result: None,
            query_running: false,
        }
    }
}

impl eframe::App for SqlManagerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.refresh_database_events();

        egui::Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SQL Manager");
                ui.separator();
                ui.label("PostgreSQL connections");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Import backup").clicked() {
                        self.open_backup_dialog(BackupAction::Import);
                    }
                    if ui.button("Export backup").clicked() {
                        self.open_backup_dialog(BackupAction::Export);
                    }
                });
            });
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

                if self.session.is_some() {
                    ui.separator();
                    ui.heading("Schemas");
                    let mut requested_schema = None;
                    for schema in &self.schemas {
                        if ui
                            .selectable_label(self.selected_schema.as_ref() == Some(schema), schema)
                            .clicked()
                        {
                            requested_schema = Some(schema.clone());
                        }
                    }
                    if let Some(schema) = requested_schema {
                        self.selected_schema = Some(schema.clone());
                        self.tables.clear();
                        if let Some(session) = &self.session {
                            let _ = session.list_tables(schema);
                        }
                    }

                    for table in &self.tables {
                        if ui.button(table).clicked()
                            && let Some(schema) = &self.selected_schema
                        {
                            self.sql = format!(
                                "SELECT * FROM {}.{} LIMIT 100;",
                                quote_identifier(schema),
                                quote_identifier(table)
                            );
                        }
                    }
                }
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

                if self.session.is_none() {
                    if ui.button("Connect").clicked() {
                        self.start_database_session();
                    }
                } else if ui.button("Disconnect").clicked() {
                    self.session = None;
                    self.schemas.clear();
                    self.tables.clear();
                    self.selected_schema = None;
                    self.query_result = None;
                    self.status = String::from("Disconnected");
                }
            });

            if self.session.is_some() {
                ui.separator();
                ui.heading("SQL workspace");
                ui.add(
                    egui::TextEdit::multiline(&mut self.sql)
                        .code_editor()
                        .desired_rows(10)
                        .desired_width(f32::INFINITY),
                );
                if ui
                    .add_enabled(
                        !self.query_running && !self.sql.trim().is_empty(),
                        egui::Button::new(if self.query_running {
                            "Running…"
                        } else {
                            "Run query"
                        }),
                    )
                    .clicked()
                    && let Some(session) = &self.session
                {
                    match session.execute(self.sql.clone()) {
                        Ok(()) => {
                            self.query_running = true;
                            self.status = String::from("Running query…");
                        }
                        Err(error) => self.status = format!("Could not submit query: {error}"),
                    }
                }

                if let Some(result) = &self.query_result {
                    ui.label(&result.summary);
                    egui::ScrollArea::both().max_height(260.0).show(ui, |ui| {
                        for (result_index, result_set) in result.result_sets.iter().enumerate() {
                            ui.label(format!("Result {}", result_index + 1));
                            egui::Grid::new(("query_result", result_index))
                                .striped(true)
                                .show(ui, |ui| {
                                    for column in &result_set.columns {
                                        ui.strong(column);
                                    }
                                    ui.end_row();
                                    for row in &result_set.rows {
                                        for value in row {
                                            ui.label(value.as_deref().unwrap_or("NULL"));
                                        }
                                        ui.end_row();
                                    }
                                });
                            if result_set.truncated {
                                ui.label("Result limited to the first 1,000 rows.");
                            }
                        }
                    });
                }
            }

            ui.add_space(12.0);
            self.refresh_test_status();
            ui.label(&self.status);
        });

        self.show_backup_dialog(ui.ctx());
    }
}

impl SqlManagerApp {
    fn start_database_session(&mut self) {
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

        self.schemas.clear();
        self.tables.clear();
        self.selected_schema = None;
        self.query_result = None;
        self.session = Some(DatabaseSession::connect(profile, password));
        self.status = String::from("Connecting to PostgreSQL…");
    }

    fn refresh_database_events(&mut self) {
        let events = self
            .session
            .as_ref()
            .map(|session| session.events.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();

        for event in events {
            match event {
                DatabaseEvent::Connected => self.status = String::from("Connected to PostgreSQL"),
                DatabaseEvent::Schemas(schemas) => self.schemas = schemas,
                DatabaseEvent::Tables { schema, tables } => {
                    if self.selected_schema.as_deref() == Some(&schema) {
                        self.tables = tables;
                    }
                }
                DatabaseEvent::Query(Ok(result)) => {
                    self.query_running = false;
                    self.status = result.summary.clone();
                    self.query_result = Some(result);
                }
                DatabaseEvent::Query(Err(error)) => {
                    self.query_running = false;
                    self.status = format!("Query failed: {error}");
                    self.query_result = None;
                }
                DatabaseEvent::Disconnected(error) => {
                    self.session = None;
                    self.schemas.clear();
                    self.tables.clear();
                    self.selected_schema = None;
                    self.query_running = false;
                    self.status = format!("Disconnected: {error}");
                }
            }
        }
    }

    fn open_backup_dialog(&mut self, action: BackupAction) {
        self.backup_dialog = Some(BackupDialog {
            action,
            password: String::new(),
            confirmation: String::new(),
        });
    }

    fn show_backup_dialog(&mut self, context: &egui::Context) {
        let Some(dialog) = self.backup_dialog.as_mut() else {
            return;
        };

        let action = dialog.action;
        let mut close = false;
        let mut confirm = false;
        let title = match action {
            BackupAction::Export => "Export encrypted backup",
            BackupAction::Import => "Import encrypted backup",
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label("The backup contains connection settings and saved passwords.");
                ui.add_space(8.0);
                ui.label("Backup password");
                ui.add(egui::TextEdit::singleline(&mut dialog.password).password(true));

                if matches!(action, BackupAction::Export) {
                    ui.label("Confirm password");
                    ui.add(egui::TextEdit::singleline(&mut dialog.confirmation).password(true));
                    ui.label("Use at least 12 characters. This password cannot be recovered.");
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let ready = !dialog.password.is_empty()
                        && (matches!(action, BackupAction::Import)
                            || (dialog.password.len() >= 12
                                && dialog.password == dialog.confirmation));
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(match action {
                                BackupAction::Export => "Choose file and export",
                                BackupAction::Import => "Choose file and import",
                            }),
                        )
                        .clicked()
                    {
                        confirm = true;
                    }
                });
            });

        if confirm {
            let mut password = dialog.password.clone();
            self.finish_backup(action, &password);
            password.zeroize();
            close = true;
        }
        if close && let Some(mut dialog) = self.backup_dialog.take() {
            dialog.password.zeroize();
            dialog.confirmation.zeroize();
        }
    }

    fn finish_backup(&mut self, action: BackupAction, password: &str) {
        match action {
            BackupAction::Export => {
                let Some(path) = FileDialog::new()
                    .add_filter("SQL Manager encrypted backup", &["sqlmbackup"])
                    .set_file_name("sql-manager.sqlmbackup")
                    .save_file()
                else {
                    return;
                };

                match encrypt_profiles(&self.profiles, password)
                    .and_then(|bytes| fs::write(path, bytes).map_err(|error| error.to_string()))
                {
                    Ok(()) => self.status = String::from("Encrypted backup exported"),
                    Err(error) => self.status = format!("Could not export backup: {error}"),
                }
            }
            BackupAction::Import => {
                let Some(path) = FileDialog::new()
                    .add_filter("SQL Manager encrypted backup", &["sqlmbackup"])
                    .pick_file()
                else {
                    return;
                };

                let imported = fs::read(path)
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| decrypt_profiles(&bytes, password));
                let imported = match imported {
                    Ok(imported) => imported,
                    Err(error) => {
                        self.status = format!("Could not import backup: {error}");
                        return;
                    }
                };

                let mut merged = self.profiles.clone();
                for (profile, secret) in imported {
                    let draft = ConnectionDraft::from(&profile);
                    if let Err(error) = draft.to_profile(Some(profile.id)) {
                        self.status = format!("Backup contains an invalid connection: {error}");
                        return;
                    }
                    if let Some(mut secret) = secret {
                        let result = secrets::save_password(profile.id, &secret);
                        secret.zeroize();
                        if let Err(error) = result {
                            self.status =
                                format!("Could not restore a connection password: {error}");
                            return;
                        }
                    }

                    if let Some(existing) = merged.iter_mut().find(|item| item.id == profile.id) {
                        *existing = profile;
                    } else {
                        merged.push(profile);
                    }
                }

                match storage::save_profiles(&merged) {
                    Ok(()) => {
                        self.profiles = merged;
                        self.status = String::from("Encrypted backup imported and merged");
                    }
                    Err(error) => {
                        self.status = format!("Could not save restored connections: {error}");
                    }
                }
            }
        }
    }

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

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::quote_identifier;

    #[test]
    fn quotes_identifiers_without_interpreting_sql() {
        assert_eq!(quote_identifier("public"), "\"public\"");
        assert_eq!(
            quote_identifier("items\"; DROP TABLE users; --"),
            "\"items\"\"; DROP TABLE users; --\""
        );
    }
}
