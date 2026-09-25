mod backup;
mod connection;
mod database;
mod engine;
mod schema;
mod schema_operations;
mod secrets;
mod ssh_tunnel;
mod storage;

use std::{
    collections::{HashMap, HashSet},
    fs,
    sync::mpsc::{self, Receiver},
};

use backup::{decrypt_profiles, encrypt_profiles};
use connection::{ConnectionDraft, ConnectionProfile, TlsMode};
use database::{DatabaseInfo, Event as DatabaseEvent, QueryOutput};
use eframe::egui;
use engine::{DatabaseSession, adapter};
use rfd::FileDialog;
use schema::{EditedCell, TableData, delete_sql, insert_sql, update_sql};
use schema_operations::{
    ColumnType, NewColumn, add_column_sql, create_table_sql, drop_column_sql, drop_table_sql,
    rename_column_sql, rename_table_sql,
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "SQL Manager",
        options,
        Box::new(|_creation_context| Ok(Box::<SqlManagerApp>::default())),
    )
}

struct SqlManagerApp {
    screen: AppScreen,
    workspace_tab: WorkspaceTab,
    profiles: Vec<ConnectionProfile>,
    selected_profile_id: Option<Uuid>,
    connection_editor_open: bool,
    draft: ConnectionDraft,
    status: String,
    pending_test: Option<Receiver<String>>,
    backup_dialog: Option<BackupDialog>,
    sessions: HashMap<SessionKey, Box<dyn DatabaseSession>>,
    active_session_key: Option<SessionKey>,
    connected_sessions: HashSet<SessionKey>,
    connecting_sessions: HashSet<SessionKey>,
    session_read_only: HashMap<SessionKey, bool>,
    session_profiles: HashMap<Uuid, ConnectionProfile>,
    session_passwords: HashMap<Uuid, Zeroizing<String>>,
    database_catalogs: HashMap<Uuid, Vec<DatabaseInfo>>,
    schemas: Vec<String>,
    tables: Vec<String>,
    selected_schema: Option<String>,
    selected_table: Option<String>,
    sql: String,
    query_result: Option<QueryOutput>,
    query_running: bool,
    current_table: Option<TableData>,
    table_loading: bool,
    row_editor: Option<RowEditor>,
    pending_delete_row: Option<usize>,
    schema_dialog: Option<SchemaDialog>,
    selected_column: Option<String>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct SessionKey {
    profile_id: Uuid,
    database: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AppScreen {
    #[default]
    Connections,
    Workspace,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum WorkspaceTab {
    #[default]
    Sql,
    TableData,
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

enum RowEditorMode {
    Insert,
    Update { original: Vec<Option<String>> },
}

struct RowEditor {
    mode: RowEditorMode,
    cells: Vec<EditedCell>,
}

#[derive(Clone, Copy)]
enum SchemaAction {
    CreateTable,
    RenameTable,
    DropTable,
    AddColumn,
    RenameColumn,
    DropColumn,
}

struct SchemaDialog {
    action: SchemaAction,
    schema: String,
    table: String,
    object_name: String,
    new_name: String,
    columns: Vec<NewColumn>,
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
            screen: AppScreen::Connections,
            workspace_tab: WorkspaceTab::Sql,
            profiles,
            selected_profile_id: None,
            connection_editor_open: false,
            draft: ConnectionDraft::default(),
            status,
            pending_test: None,
            backup_dialog: None,
            sessions: HashMap::new(),
            active_session_key: None,
            connected_sessions: HashSet::new(),
            connecting_sessions: HashSet::new(),
            session_read_only: HashMap::new(),
            session_profiles: HashMap::new(),
            session_passwords: HashMap::new(),
            database_catalogs: HashMap::new(),
            schemas: Vec::new(),
            tables: Vec::new(),
            selected_schema: None,
            selected_table: None,
            sql: String::from("SELECT current_database(), current_user;"),
            query_result: None,
            query_running: false,
            current_table: None,
            table_loading: false,
            row_editor: None,
            pending_delete_row: None,
            schema_dialog: None,
            selected_column: None,
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
                if self.screen == AppScreen::Workspace {
                    if ui.button("Connections").clicked() {
                        self.screen = AppScreen::Connections;
                    }
                    if let Some(key) = &self.active_session_key {
                        let active_name = self
                            .session_profiles
                            .get(&key.profile_id)
                            .map_or("PostgreSQL", |profile| profile.name.as_str());
                        ui.label(format!("{active_name} · {}", key.database));
                    }
                    if ui.button("Disconnect").clicked() {
                        self.disconnect_session();
                    }
                } else {
                    ui.label("Connection Manager");
                    if self.active_session_is_connected()
                        && ui.button("Return to Workspace").clicked()
                    {
                        self.screen = AppScreen::Workspace;
                    }
                }
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
            .show(ui, |ui| match self.screen {
                AppScreen::Connections => {
                    let mut selected = self.selected_profile_id;
                    let mut create_profile = false;
                    ui.horizontal(|ui| {
                        ui.heading("Saved connections");
                        if ui.small_button("New").clicked() {
                            create_profile = true;
                        }
                    });
                    ui.separator();

                    for profile in &self.profiles {
                        let response =
                            ui.selectable_label(selected == Some(profile.id), &profile.name);
                        if response.clicked() {
                            selected = Some(profile.id);
                            self.draft = ConnectionDraft::from(profile);
                            self.draft.password = match secrets::load_password(profile.id) {
                                Ok(Some(password)) => password,
                                Ok(None) => String::new(),
                                Err(error) => {
                                    self.status = format!("Could not load saved password: {error}");
                                    String::new()
                                }
                            };
                            self.connection_editor_open = false;
                        }
                    }
                    self.selected_profile_id = selected;

                    if create_profile {
                        self.selected_profile_id = None;
                        self.draft = ConnectionDraft::default();
                        self.connection_editor_open = true;
                        self.status = String::from("New connection");
                    }

                    ui.separator();
                    ui.heading("Open sessions");
                    let mut open_sessions = self
                        .sessions
                        .keys()
                        .map(|key| {
                            let name = self
                                .session_profiles
                                .get(&key.profile_id)
                                .map_or("PostgreSQL", |profile| profile.name.as_str());
                            (key.clone(), format!("{name} · {}", key.database))
                        })
                        .collect::<Vec<_>>();
                    open_sessions.sort_by(|left, right| left.1.cmp(&right.1));
                    let mut activate = None;
                    for (key, label) in open_sessions {
                        let connected = self.connected_sessions.contains(&key);
                        let label = if connected {
                            label
                        } else {
                            format!("{label} · Connecting…")
                        };
                        if ui
                            .selectable_label(self.active_session_key.as_ref() == Some(&key), label)
                            .clicked()
                        {
                            activate = Some(key);
                        }
                    }
                    if let Some(key) = activate {
                        self.activate_session(key);
                    }
                }
                AppScreen::Workspace => {
                    let Some(active_key) = self.active_session_key.clone() else {
                        ui.label("No active database session");
                        return;
                    };

                    ui.heading("Open database sessions");
                    let mut open_sessions = self
                        .sessions
                        .keys()
                        .map(|key| {
                            let profile_name = self
                                .session_profiles
                                .get(&key.profile_id)
                                .map_or("PostgreSQL", |profile| profile.name.as_str());
                            (key.clone(), format!("{profile_name} · {}", key.database))
                        })
                        .collect::<Vec<_>>();
                    open_sessions.sort_by(|left, right| left.1.cmp(&right.1));
                    let mut requested_session = None;
                    for (key, label) in open_sessions {
                        if ui
                            .selectable_label(self.active_session_key.as_ref() == Some(&key), label)
                            .clicked()
                        {
                            requested_session = Some(key);
                        }
                    }
                    if let Some(key) = requested_session {
                        self.activate_session(key);
                    }

                    ui.heading("Database Browser");
                    ui.label(&active_key.database);
                    ui.separator();

                    let show_all = self
                        .session_profiles
                        .get(&active_key.profile_id)
                        .is_none_or(|profile| profile.show_all_databases);
                    let mut show_all_next = show_all;
                    if ui
                        .checkbox(&mut show_all_next, "Show all databases")
                        .changed()
                    {
                        self.update_show_all_databases(active_key.profile_id, show_all_next);
                    }

                    let databases = self
                        .database_catalogs
                        .get(&active_key.profile_id)
                        .cloned()
                        .unwrap_or_default();
                    let mut requested_database = None;
                    for database in databases {
                        if !show_all && database.name != active_key.database {
                            continue;
                        }
                        let label = if database.is_connectable() {
                            database.name.clone()
                        } else if database.is_template {
                            format!("{} (template database)", database.name)
                        } else if !database.allows_connections {
                            format!("{} (connections disabled)", database.name)
                        } else {
                            format!("{} (no CONNECT privilege)", database.name)
                        };
                        let mut clicked = false;
                        ui.add_enabled_ui(database.is_connectable(), |ui| {
                            clicked = ui
                                .selectable_label(database.name == active_key.database, label)
                                .clicked();
                        });
                        if clicked {
                            requested_database = Some(database.name);
                        }
                    }
                    if let Some(database) = requested_database {
                        self.open_database_for_profile(active_key.profile_id, database);
                    }
                    ui.separator();

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
                        self.selected_table = None;
                        self.current_table = None;
                        self.selected_column = None;
                        if let Some(session) = self.active_session() {
                            let _ = session.list_tables(schema);
                        }
                    }

                    if self.selected_schema.is_some() {
                        ui.horizontal(|ui| {
                            ui.label("Tables");
                            if ui
                                .add_enabled(
                                    !self.active_session_is_read_only(),
                                    egui::Button::new("+ Table"),
                                )
                                .clicked()
                            {
                                self.open_schema_dialog(SchemaAction::CreateTable);
                            }
                        });
                    }

                    let mut requested_table = None;
                    for table in &self.tables {
                        if ui
                            .selectable_label(self.selected_table.as_ref() == Some(table), table)
                            .clicked()
                        {
                            requested_table = Some(table.clone());
                        }
                    }
                    if let Some(table) = requested_table
                        && let Some(schema) = self.selected_schema.clone()
                    {
                        self.selected_column = None;
                        self.current_table = None;
                        self.selected_table = Some(table.clone());
                        self.sql = format!(
                            "SELECT * FROM {}.{} LIMIT 100;",
                            schema::quote_identifier(&schema),
                            schema::quote_identifier(&table)
                        );
                    }

                    if let Some(table) = &self.selected_table {
                        ui.separator();
                        ui.label(table);
                        if ui.button("View Data").clicked()
                            && let (Some(session), Some(schema)) =
                                (self.active_session(), self.selected_schema.clone())
                        {
                            match session.load_table(schema, table.clone(), 0) {
                                Ok(()) => {
                                    self.table_loading = true;
                                    self.workspace_tab = WorkspaceTab::TableData;
                                }
                                Err(error) => {
                                    self.status = format!("Could not open table data: {error}");
                                }
                            }
                        }
                    }
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            match self.screen {
                AppScreen::Connections => self.show_connection_manager(ui),
                AppScreen::Workspace => self.show_workspace(ui),
            }
            ui.add_space(12.0);
            self.refresh_test_status();
            ui.label(&self.status);
        });

        self.show_backup_dialog(ui.ctx());
        self.show_row_editor(ui.ctx());
        self.show_delete_confirmation(ui.ctx());
        self.show_schema_dialog(ui.ctx());
    }
}

impl SqlManagerApp {
    fn active_session(&self) -> Option<&dyn DatabaseSession> {
        self.active_session_key
            .as_ref()
            .and_then(|key| self.sessions.get(key).map(Box::as_ref))
    }

    fn active_session_is_connected(&self) -> bool {
        self.active_session_key
            .as_ref()
            .is_some_and(|key| self.connected_sessions.contains(key))
    }

    fn active_session_is_read_only(&self) -> bool {
        self.active_session_key
            .as_ref()
            .and_then(|key| self.session_read_only.get(key))
            .copied()
            .unwrap_or(false)
    }

    fn reset_workspace_view(&mut self) {
        self.workspace_tab = WorkspaceTab::Sql;
        self.schemas.clear();
        self.tables.clear();
        self.selected_schema = None;
        self.selected_table = None;
        self.sql = String::from("SELECT current_database(), current_user;");
        self.query_result = None;
        self.query_running = false;
        self.current_table = None;
        self.table_loading = false;
        self.row_editor = None;
        self.pending_delete_row = None;
        self.schema_dialog = None;
        self.selected_column = None;
    }

    fn activate_session(&mut self, key: SessionKey) {
        self.active_session_key = Some(key.clone());
        self.selected_profile_id = Some(key.profile_id);
        self.reset_workspace_view();

        if self.connected_sessions.contains(&key) {
            self.screen = AppScreen::Workspace;
            if let Some(session) = self.sessions.get(&key) {
                let _ = session.list_schemas();
            }
            self.status = format!("Connected to {}", key.database);
        } else {
            self.screen = AppScreen::Connections;
            self.status = format!("Connecting to {}…", key.database);
        }
    }

    fn open_database_session(
        &mut self,
        profile: ConnectionProfile,
        database: String,
        password: String,
    ) {
        let profile_id = profile.id;
        let key = SessionKey {
            profile_id,
            database: database.clone(),
        };

        if self.sessions.contains_key(&key) {
            if self.session_read_only.get(&key) == Some(&profile.read_only) {
                self.activate_session(key);
                return;
            }
            self.close_session(&key);
        }

        self.session_profiles.insert(profile_id, profile.clone());
        self.session_read_only
            .insert(key.clone(), profile.read_only);

        let mut password = Zeroizing::new(password);
        let cached_password = self
            .session_passwords
            .entry(profile_id)
            .or_insert_with(|| Zeroizing::new(password.to_string()))
            .to_string();
        password.zeroize();

        let mut target = profile;
        target.database = database.clone();
        let engine = target.engine;
        self.sessions.insert(
            key.clone(),
            adapter(engine).connect(target, cached_password),
        );
        self.connecting_sessions.insert(key.clone());
        self.active_session_key = Some(key.clone());
        self.selected_profile_id = Some(profile_id);
        self.reset_workspace_view();
        self.status = format!("Connecting to {database}…");
    }

    fn open_database_for_profile(&mut self, profile_id: Uuid, database: String) {
        let profile = self.session_profiles.get(&profile_id).cloned().or_else(|| {
            self.profiles
                .iter()
                .find(|profile| profile.id == profile_id)
                .cloned()
        });
        let Some(profile) = profile else {
            self.status = String::from("Connection profile is no longer available");
            return;
        };

        let password = match self.session_passwords.get(&profile_id) {
            Some(password) => password.to_string(),
            None => match secrets::load_password(profile_id) {
                Ok(Some(password)) => password,
                Ok(None) => String::new(),
                Err(error) => {
                    self.status =
                        format!("Could not load password from the system keyring: {error}");
                    return;
                }
            },
        };
        self.open_database_session(profile, database, password);
    }

    fn update_show_all_databases(&mut self, profile_id: Uuid, show_all: bool) {
        let Some(index) = self
            .profiles
            .iter()
            .position(|profile| profile.id == profile_id)
        else {
            self.status = String::from("Save this profile before changing its database browser");
            return;
        };

        let mut profile = self.profiles[index].clone();
        profile.show_all_databases = show_all;
        match storage::save_profile(&profile, &mut self.profiles) {
            Ok(()) => {
                if let Some(session_profile) = self.session_profiles.get_mut(&profile_id) {
                    session_profile.show_all_databases = show_all;
                }
                self.status = if show_all {
                    String::from("Showing all server databases")
                } else {
                    String::from("Showing the selected database only")
                };
            }
            Err(error) => self.status = format!("Could not save database browser setting: {error}"),
        }
    }

    fn close_session(&mut self, key: &SessionKey) {
        self.sessions.remove(key);
        self.connected_sessions.remove(key);
        self.connecting_sessions.remove(key);
        self.session_read_only.remove(key);
        if self.active_session_key.as_ref() == Some(key) {
            self.active_session_key = None;
            self.screen = AppScreen::Connections;
            self.selected_profile_id = self
                .profiles
                .iter()
                .any(|profile| profile.id == key.profile_id)
                .then_some(key.profile_id);
            self.reset_workspace_view();
            self.status = format!("Disconnected from {}", key.database);
        }

        let profile_still_open = self
            .sessions
            .keys()
            .any(|open_key| open_key.profile_id == key.profile_id);
        if !profile_still_open {
            self.session_profiles.remove(&key.profile_id);
            self.session_passwords.remove(&key.profile_id);
            self.database_catalogs.remove(&key.profile_id);
        }
    }

    fn disconnect_session(&mut self) {
        if let Some(key) = self.active_session_key.clone() {
            self.close_session(&key);
        } else {
            self.screen = AppScreen::Connections;
            self.reset_workspace_view();
            self.status = String::from("No active database session");
        }
    }

    fn show_connection_manager(&mut self, ui: &mut egui::Ui) {
        if self.connection_editor_open {
            self.show_connection_editor(ui);
            return;
        }

        ui.heading("Connection Manager");
        ui.label("Create a PostgreSQL profile or select one from the Connections list.");
        ui.add_space(12.0);

        let profile = self
            .selected_profile_id
            .and_then(|id| self.profiles.iter().find(|profile| profile.id == id))
            .cloned();
        let Some(profile) = profile else {
            ui.label("Select a saved connection, or create a new one.");
            if ui.button("New connection").clicked() {
                self.selected_profile_id = None;
                self.draft = ConnectionDraft::default();
                self.connection_editor_open = true;
            }
            return;
        };

        ui.group(|ui| {
            ui.heading(&profile.name);
            egui::Grid::new("profile_summary")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Engine");
                    ui.label(profile.engine.label());
                    ui.end_row();
                    ui.label("Host");
                    ui.label(format!("{}:{}", profile.host, profile.port));
                    ui.end_row();
                    ui.label("Database");
                    ui.label(&profile.database);
                    ui.end_row();
                    ui.label("Username");
                    ui.label(&profile.username);
                    ui.end_row();
                    ui.label("TLS");
                    ui.label(profile.tls_mode.label());
                    ui.end_row();
                    ui.label("Show all databases");
                    ui.label(if profile.show_all_databases {
                        "Yes"
                    } else {
                        "No"
                    });
                    ui.end_row();
                    ui.label("Read Only");
                    ui.label(if profile.read_only {
                        "Enabled"
                    } else {
                        "Disabled"
                    });
                    ui.end_row();
                    if let Some(ssh) = &profile.ssh_tunnel {
                        ui.label("SSH tunnel");
                        ui.label(format!("{}@{}:{}", ssh.username, ssh.host, ssh.port));
                        ui.end_row();
                    }
                });
        });

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button("Edit connection").clicked() {
                self.draft = ConnectionDraft::from(&profile);
                self.draft.password = match secrets::load_password(profile.id) {
                    Ok(Some(password)) => password,
                    Ok(None) => String::new(),
                    Err(error) => {
                        self.status = format!("Could not load saved password: {error}");
                        String::new()
                    }
                };
                self.connection_editor_open = true;
            }

            if ui
                .add_enabled(
                    self.pending_test.is_none(),
                    egui::Button::new("Test connection"),
                )
                .clicked()
            {
                self.start_connection_test(ui.ctx());
            }

            let default_session = SessionKey {
                profile_id: profile.id,
                database: profile.database.clone(),
            };
            if self.connected_sessions.contains(&default_session) {
                if ui.button("Open Workspace").clicked() {
                    self.activate_session(default_session.clone());
                }
                if self.active_session_key.as_ref() == Some(&default_session)
                    && ui.button("Close Session").clicked()
                {
                    self.close_session(&default_session);
                }
            } else if self.connecting_sessions.contains(&default_session) {
                ui.label(format!("Connecting to {}…", profile.database));
                if ui.button("Cancel").clicked() {
                    self.close_session(&default_session);
                }
            } else {
                if ui.button("Connect").clicked() {
                    self.open_database_for_profile(profile.id, profile.database.clone());
                }
            }
        });
    }

    fn show_connection_editor(&mut self, ui: &mut egui::Ui) {
        ui.heading(if self.selected_profile_id.is_some() {
            "Edit connection"
        } else {
            "New connection"
        });
        ui.add_space(8.0);

        let mut parse_connection_url = false;
        egui::Grid::new("connection_form")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label("Connection URL");
                ui.horizontal(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.draft.connection_url)
                            .hint_text("postgresql://user:password@host:5432/database")
                            .desired_width(380.0),
                    );
                    if response.changed() {
                        self.draft.connection_url_applied = false;
                    }
                    if ui.button("Fill fields").clicked() {
                        parse_connection_url = true;
                    }
                });
                ui.end_row();

                ui.label("Name");
                ui.text_edit_singleline(&mut self.draft.name);
                ui.end_row();

                ui.label("Engine");
                ui.label(self.draft.engine.label());
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

                ui.label("Database browser");
                ui.checkbox(
                    &mut self.draft.show_all_databases,
                    "Show all server databases",
                );
                ui.end_row();

                ui.label("Execution mode");
                ui.checkbox(&mut self.draft.read_only, "Read-only SQL session");
                ui.end_row();

                ui.label("SSH tunnel");
                ui.checkbox(&mut self.draft.ssh_enabled, "Use OpenSSH tunnel");
                ui.end_row();

                if self.draft.ssh_enabled {
                    ui.label("SSH host");
                    ui.text_edit_singleline(&mut self.draft.ssh_host);
                    ui.end_row();

                    ui.label("SSH port");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.ssh_port).desired_width(90.0),
                    );
                    ui.end_row();

                    ui.label("SSH username");
                    ui.text_edit_singleline(&mut self.draft.ssh_username);
                    ui.end_row();

                    ui.label("SSH identity");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.ssh_identity_file)
                                .desired_width(220.0),
                        );
                        if ui.button("Browse").clicked()
                            && let Some(path) = FileDialog::new().pick_file()
                        {
                            self.draft.ssh_identity_file = path.display().to_string();
                        }
                    });
                    ui.end_row();

                    ui.label("");
                    ui.label("Uses ssh-agent or this key file; trust the SSH host key first.");
                    ui.end_row();
                }
            });

        if parse_connection_url {
            self.status = match self.draft.apply_connection_url() {
                Ok(()) => String::from("Connection fields filled from URL"),
                Err(error) => format!("Could not parse connection URL: {error}"),
            };
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button("Save connection").clicked() {
                self.save_connection();
            }
            if ui
                .add_enabled(
                    self.pending_test.is_none(),
                    egui::Button::new("Test connection"),
                )
                .clicked()
            {
                self.start_connection_test(ui.ctx());
            }
            if ui.button("Save & Connect").clicked() && self.save_connection() {
                self.start_database_session();
            }
            if let Some(key) = self.active_session_key.clone()
                && self.connecting_sessions.contains(&key)
                && ui.button("Cancel connection").clicked()
            {
                self.close_session(&key);
            }
            if ui.button("Cancel").clicked() {
                self.connection_editor_open = false;
                if let Some(id) = self.selected_profile_id
                    && let Some(profile) = self.profiles.iter().find(|profile| profile.id == id)
                {
                    self.draft = ConnectionDraft::from(profile);
                    self.draft.password = secrets::load_password(id)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                } else {
                    self.draft = ConnectionDraft::default();
                }
            }
        });
    }

    fn show_workspace(&mut self, ui: &mut egui::Ui) {
        let Some(active_key) = self.active_session_key.as_ref() else {
            ui.heading("Workspace");
            ui.label("Connect to a database to open the workspace.");
            if ui.button("Open Connection Manager").clicked() {
                self.screen = AppScreen::Connections;
            }
            return;
        };
        if !self.connected_sessions.contains(active_key) {
            ui.heading("Opening database session…");
            ui.label(&self.status);
            return;
        }

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.workspace_tab, WorkspaceTab::Sql, "SQL Editor");
            ui.add_enabled_ui(self.current_table.is_some() || self.table_loading, |ui| {
                ui.selectable_value(
                    &mut self.workspace_tab,
                    WorkspaceTab::TableData,
                    "Table Data",
                );
            });
        });
        ui.separator();

        match self.workspace_tab {
            WorkspaceTab::Sql => self.show_sql_editor(ui),
            WorkspaceTab::TableData => self.show_table_data(ui),
        }
    }

    fn show_sql_editor(&mut self, ui: &mut egui::Ui) {
        ui.heading("SQL Editor");
        if self.active_session_is_read_only() {
            ui.label("Read-only mode: SQL runs in a PostgreSQL read-only transaction.");
        }
        ui.add(
            egui::TextEdit::multiline(&mut self.sql)
                .code_editor()
                .desired_rows(12)
                .desired_width(f32::INFINITY),
        );
        if ui
            .add_enabled(
                !self.query_running && !self.sql.trim().is_empty(),
                egui::Button::new(if self.query_running {
                    "Running…"
                } else if self.active_session_is_read_only() {
                    "Run read-only query"
                } else {
                    "Run query"
                }),
            )
            .clicked()
            && let Some(session) = self.active_session()
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
            ui.add_space(8.0);
            ui.heading("Results");
            ui.label(&result.summary);
            egui::ScrollArea::both().max_height(360.0).show(ui, |ui| {
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

    fn apply_pending_connection_url(&mut self) -> bool {
        if self.draft.connection_url.trim().is_empty() || self.draft.connection_url_applied {
            return true;
        }

        match self.draft.apply_connection_url() {
            Ok(()) => true,
            Err(error) => {
                self.status = format!("Could not parse connection URL: {error}");
                false
            }
        }
    }

    fn start_database_session(&mut self) {
        if !self.apply_pending_connection_url() {
            return;
        }
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

        let database = profile.database.clone();
        self.open_database_session(profile, database, password);
    }

    fn refresh_database_events(&mut self) {
        let events = self
            .sessions
            .iter_mut()
            .flat_map(|(key, session)| {
                session
                    .drain_events()
                    .into_iter()
                    .map(|event| (key.clone(), event))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        for (key, event) in events {
            match event {
                DatabaseEvent::Connected => {
                    self.connecting_sessions.remove(&key);
                    self.connected_sessions.insert(key.clone());
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.selected_profile_id = Some(key.profile_id);
                        self.connection_editor_open = false;
                        self.screen = AppScreen::Workspace;
                        self.workspace_tab = WorkspaceTab::Sql;
                        self.status = format!("Connected to {}", key.database);
                    }
                }
                DatabaseEvent::Databases(Ok(databases)) => {
                    self.database_catalogs.insert(key.profile_id, databases);
                }
                DatabaseEvent::Databases(Err(error)) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.status = format!("Could not list databases: {error}");
                    }
                }
                DatabaseEvent::Schemas(schemas) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.schemas = schemas;
                    }
                }
                DatabaseEvent::Tables { schema, tables } => {
                    if self.active_session_key.as_ref() == Some(&key)
                        && self.selected_schema.as_deref() == Some(&schema)
                    {
                        self.tables = tables;
                    }
                }
                DatabaseEvent::TableData(Ok(table)) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.table_loading = false;
                        self.status = format!("Loaded {}.{}", table.schema, table.name);
                        self.current_table = Some(table);
                    }
                }
                DatabaseEvent::TableData(Err(error)) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.table_loading = false;
                        self.status = format!("Could not load table: {error}");
                    }
                }
                DatabaseEvent::Query(Ok(result)) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.query_running = false;
                        self.status = result.summary.clone();
                        self.query_result = Some(result);
                    }
                }
                DatabaseEvent::Query(Err(error)) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.query_running = false;
                        self.status = format!("Query failed: {error}");
                        self.query_result = None;
                    }
                }
                DatabaseEvent::Disconnected(error) => {
                    let was_active = self.active_session_key.as_ref() == Some(&key);
                    self.close_session(&key);
                    if was_active {
                        self.status = format!("Disconnected from {}: {error}", key.database);
                    }
                }
            }
        }
    }

    fn show_table_data(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.heading("Table data");
        let read_only = self.active_session_is_read_only();
        if read_only {
            ui.label("Read-only mode is enabled for this connection profile.");
        }
        if self.table_loading {
            ui.label("Loading table data…");
        }

        let Some(table) = self.current_table.clone() else {
            return;
        };

        let mut requested_schema_action = None;
        ui.horizontal(|ui| {
            ui.label(format!("{}.{}", table.schema, table.name));
            if ui.button("Reload").clicked() {
                self.request_table_page(table.offset);
            }
            if ui
                .add_enabled(!read_only, egui::Button::new("Insert row"))
                .clicked()
            {
                self.row_editor = Some(RowEditor {
                    mode: RowEditorMode::Insert,
                    cells: table
                        .columns
                        .iter()
                        .map(|_column| EditedCell {
                            use_default: true,
                            ..EditedCell::default()
                        })
                        .collect(),
                });
            }
            if table.primary_key.is_empty() {
                ui.label("Rows are read-only because this table has no primary key.");
            }
            ui.separator();
            if ui
                .add_enabled(!read_only, egui::Button::new("Rename table"))
                .clicked()
            {
                requested_schema_action = Some(SchemaAction::RenameTable);
            }
            if ui
                .add_enabled(!read_only, egui::Button::new("Drop table"))
                .clicked()
            {
                requested_schema_action = Some(SchemaAction::DropTable);
            }
            if ui
                .add_enabled(!read_only, egui::Button::new("Add column"))
                .clicked()
            {
                requested_schema_action = Some(SchemaAction::AddColumn);
            }
            egui::ComboBox::from_id_salt("selected_column")
                .selected_text(self.selected_column.as_deref().unwrap_or("Select column"))
                .show_ui(ui, |ui| {
                    for column in &table.columns {
                        ui.selectable_value(
                            &mut self.selected_column,
                            Some(column.name.clone()),
                            &column.name,
                        );
                    }
                });
            if ui
                .add_enabled(
                    !read_only && self.selected_column.is_some(),
                    egui::Button::new("Rename column"),
                )
                .clicked()
            {
                requested_schema_action = Some(SchemaAction::RenameColumn);
            }
            if ui
                .add_enabled(
                    !read_only && self.selected_column.is_some(),
                    egui::Button::new("Drop column"),
                )
                .clicked()
            {
                requested_schema_action = Some(SchemaAction::DropColumn);
            }
        });

        let mut edit_row = None;
        let mut delete_row = None;
        egui::ScrollArea::both().max_height(320.0).show(ui, |ui| {
            egui::Grid::new("table_data_grid")
                .striped(true)
                .show(ui, |ui| {
                    for column in &table.columns {
                        ui.strong(format!("{} ({})", column.name, column.data_type));
                    }
                    if !table.primary_key.is_empty() {
                        ui.strong("Actions");
                    }
                    ui.end_row();

                    for (index, row) in table.rows.iter().enumerate() {
                        for value in row {
                            ui.label(value.as_deref().unwrap_or("NULL"));
                        }
                        if !table.primary_key.is_empty() {
                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(!read_only, egui::Button::new("Edit"))
                                    .clicked()
                                {
                                    edit_row = Some(index);
                                }
                                if ui
                                    .add_enabled(!read_only, egui::Button::new("Delete"))
                                    .clicked()
                                {
                                    delete_row = Some(index);
                                }
                            });
                        }
                        ui.end_row();
                    }
                });
        });

        let mut new_offset = None;
        ui.horizontal(|ui| {
            ui.label(format!(
                "Rows {}–{}",
                table.offset + 1,
                table.offset + table.rows.len() as u64
            ));
            if ui
                .add_enabled(table.offset > 0, egui::Button::new("Previous"))
                .clicked()
            {
                new_offset = Some(table.offset.saturating_sub(schema::TABLE_PAGE_SIZE as u64));
            }
            if ui
                .add_enabled(table.has_more, egui::Button::new("Next"))
                .clicked()
            {
                new_offset = Some(table.offset + schema::TABLE_PAGE_SIZE as u64);
            }
        });

        if let Some(offset) = new_offset {
            self.request_table_page(offset);
        }
        if let Some(index) = edit_row
            && let Some(table) = &self.current_table
            && let Some(original) = table.rows.get(index)
        {
            self.row_editor = Some(RowEditor {
                mode: RowEditorMode::Update {
                    original: original.clone(),
                },
                cells: original
                    .iter()
                    .map(|value| EditedCell {
                        value: value.clone().unwrap_or_default(),
                        is_null: value.is_none(),
                        use_default: false,
                    })
                    .collect(),
            });
        }
        if let Some(index) = delete_row {
            self.pending_delete_row = Some(index);
        }
        if let Some(action) = requested_schema_action {
            self.open_schema_dialog(action);
        }
    }

    fn request_table_page(&mut self, offset: u64) {
        let Some(table) = &self.current_table else {
            return;
        };
        let Some(session) = self.active_session() else {
            return;
        };
        match session.load_table(table.schema.clone(), table.name.clone(), offset) {
            Ok(()) => self.table_loading = true,
            Err(error) => self.status = format!("Could not load table page: {error}"),
        }
    }

    fn show_row_editor(&mut self, context: &egui::Context) {
        let (Some(editor), Some(table)) = (&mut self.row_editor, &self.current_table) else {
            return;
        };

        let title = match editor.mode {
            RowEditorMode::Insert => "Insert row",
            RowEditorMode::Update { .. } => "Edit row",
        };
        let mut close = false;
        let mut save = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(true)
            .show(context, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(400.0)
                    .show(ui, |ui| {
                        for (column, cell) in table.columns.iter().zip(&mut editor.cells) {
                            ui.horizontal(|ui| {
                                ui.label(format!("{} ({})", column.name, column.data_type));
                                if matches!(&editor.mode, RowEditorMode::Insert)
                                    || column.default.is_some()
                                {
                                    ui.checkbox(&mut cell.use_default, "DEFAULT");
                                }
                                if column.nullable && !cell.use_default {
                                    ui.checkbox(&mut cell.is_null, "NULL");
                                }
                                if !cell.use_default && !cell.is_null {
                                    ui.text_edit_singleline(&mut cell.value);
                                }
                            });
                        }
                    });
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if ui.button("Save row").clicked() {
                        save = true;
                    }
                });
            });

        if save {
            let sql = match &editor.mode {
                RowEditorMode::Insert => Ok(insert_sql(
                    &table.schema,
                    &table.name,
                    &table.columns,
                    &editor.cells,
                )),
                RowEditorMode::Update { original } => update_sql(
                    &table.schema,
                    &table.name,
                    &table.columns,
                    &table.primary_key,
                    original,
                    &editor.cells,
                ),
            };
            match sql {
                Ok(sql) => self.submit_table_mutation(sql),
                Err(error) => self.status = error,
            }
            close = true;
        }
        if close {
            self.row_editor = None;
        }
    }

    fn show_delete_confirmation(&mut self, context: &egui::Context) {
        let (Some(index), Some(table)) = (self.pending_delete_row, &self.current_table) else {
            return;
        };

        let mut cancel = false;
        let mut confirm = false;
        egui::Window::new("Delete row?")
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label(format!(
                    "Delete row {} from {}.{}?",
                    index + 1,
                    table.schema,
                    table.name
                ));
                ui.label("This operation cannot be undone.");
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui.button("Delete").clicked() {
                        confirm = true;
                    }
                });
            });

        if confirm && let Some(row) = table.rows.get(index) {
            match delete_sql(
                &table.schema,
                &table.name,
                &table.columns,
                &table.primary_key,
                row,
            ) {
                Ok(sql) => self.submit_table_mutation(sql),
                Err(error) => self.status = error,
            }
            self.pending_delete_row = None;
        } else if cancel {
            self.pending_delete_row = None;
        }
    }

    fn open_schema_dialog(&mut self, action: SchemaAction) {
        if self.active_session_is_read_only() {
            self.status = String::from("Schema changes are disabled in read-only mode");
            return;
        }
        let current_table = self.current_table.as_ref();
        let schema = self
            .selected_schema
            .clone()
            .or_else(|| current_table.map(|table| table.schema.clone()))
            .unwrap_or_default();
        let table = current_table
            .map(|table| table.name.clone())
            .unwrap_or_default();
        let object_name = match action {
            SchemaAction::RenameColumn | SchemaAction::DropColumn => {
                self.selected_column.clone().unwrap_or_default()
            }
            _ => String::new(),
        };
        let columns = match action {
            SchemaAction::CreateTable => vec![NewColumn {
                name: String::from("id"),
                kind: ColumnType::Integer,
                nullable: false,
                primary_key: true,
            }],
            SchemaAction::AddColumn => vec![NewColumn {
                name: String::new(),
                kind: ColumnType::Text,
                nullable: true,
                primary_key: false,
            }],
            _ => Vec::new(),
        };

        self.schema_dialog = Some(SchemaDialog {
            action,
            schema,
            table,
            object_name,
            new_name: String::new(),
            columns,
        });
    }

    fn show_schema_dialog(&mut self, context: &egui::Context) {
        let Some(dialog) = self.schema_dialog.as_mut() else {
            return;
        };

        let action = dialog.action;
        let mut close = false;
        let mut apply = false;
        egui::Window::new(schema_action_title(action))
            .collapsible(false)
            .resizable(true)
            .show(context, |ui| {
                ui.label(format!("Schema: {}", dialog.schema));
                match action {
                    SchemaAction::CreateTable => {
                        let mut remove_column = None;
                        ui.horizontal(|ui| {
                            ui.label("Table name");
                            ui.text_edit_singleline(&mut dialog.object_name);
                        });
                        ui.separator();
                        let can_remove_column = dialog.columns.len() > 1;
                        for (index, column) in dialog.columns.iter_mut().enumerate() {
                            ui.horizontal(|ui| {
                                ui.text_edit_singleline(&mut column.name);
                                egui::ComboBox::from_id_salt(("create_column_type", index))
                                    .selected_text(column.kind.label())
                                    .show_ui(ui, |ui| {
                                        for kind in ColumnType::ALL {
                                            ui.selectable_value(
                                                &mut column.kind,
                                                kind,
                                                kind.label(),
                                            );
                                        }
                                    });
                                ui.checkbox(&mut column.nullable, "Nullable");
                                ui.checkbox(&mut column.primary_key, "Primary key");
                                if can_remove_column && ui.small_button("−").clicked() {
                                    remove_column = Some(index);
                                }
                            });
                        }
                        if let Some(index) = remove_column {
                            dialog.columns.remove(index);
                        }
                        if ui.button("Add column").clicked() {
                            dialog.columns.push(NewColumn {
                                name: String::new(),
                                kind: ColumnType::Text,
                                nullable: true,
                                primary_key: false,
                            });
                        }
                    }
                    SchemaAction::RenameTable | SchemaAction::RenameColumn => {
                        let current_name = match action {
                            SchemaAction::RenameTable => &dialog.table,
                            SchemaAction::RenameColumn => &dialog.object_name,
                            _ => unreachable!(),
                        };
                        ui.label(format!("Rename ‘{current_name}’ to:"));
                        ui.text_edit_singleline(&mut dialog.new_name);
                    }
                    SchemaAction::AddColumn => {
                        if let Some(column) = dialog.columns.first_mut() {
                            ui.horizontal(|ui| {
                                ui.label("Column name");
                                ui.text_edit_singleline(&mut column.name);
                            });
                            ui.horizontal(|ui| {
                                ui.label("Type");
                                egui::ComboBox::from_id_salt("add_column_type")
                                    .selected_text(column.kind.label())
                                    .show_ui(ui, |ui| {
                                        for kind in ColumnType::ALL {
                                            ui.selectable_value(
                                                &mut column.kind,
                                                kind,
                                                kind.label(),
                                            );
                                        }
                                    });
                                ui.checkbox(&mut column.nullable, "Nullable");
                            });
                        }
                    }
                    SchemaAction::DropTable => {
                        ui.label(format!("Drop table ‘{}’?", dialog.table));
                        ui.label("This cannot be undone. Dependent objects will prevent the drop.");
                    }
                    SchemaAction::DropColumn => {
                        ui.label(format!(
                            "Drop column ‘{}’ from table ‘{}’?",
                            dialog.object_name, dialog.table
                        ));
                        ui.label("This cannot be undone. Dependent objects will prevent the drop.");
                    }
                }

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let button_label = match action {
                        SchemaAction::DropTable | SchemaAction::DropColumn => "Confirm drop",
                        _ => "Apply",
                    };
                    if ui.button(button_label).clicked() {
                        apply = true;
                    }
                });
            });

        if close {
            self.schema_dialog = None;
        } else if apply && let Some(dialog) = self.schema_dialog.take() {
            self.apply_schema_operation(dialog);
        }
    }

    fn apply_schema_operation(&mut self, dialog: SchemaDialog) {
        if self.active_session_is_read_only() {
            self.status = String::from("Schema changes are disabled in read-only mode");
            return;
        }
        let sql = match dialog.action {
            SchemaAction::CreateTable => {
                create_table_sql(&dialog.schema, &dialog.object_name, &dialog.columns)
            }
            SchemaAction::RenameTable => {
                rename_table_sql(&dialog.schema, &dialog.table, &dialog.new_name)
            }
            SchemaAction::DropTable => Ok(drop_table_sql(&dialog.schema, &dialog.table)),
            SchemaAction::AddColumn => dialog
                .columns
                .first()
                .ok_or_else(|| String::from("Column definition is missing"))
                .and_then(|column| add_column_sql(&dialog.schema, &dialog.table, column)),
            SchemaAction::RenameColumn => rename_column_sql(
                &dialog.schema,
                &dialog.table,
                &dialog.object_name,
                &dialog.new_name,
            ),
            SchemaAction::DropColumn => Ok(drop_column_sql(
                &dialog.schema,
                &dialog.table,
                &dialog.object_name,
            )),
        };
        let sql = match sql {
            Ok(sql) => sql,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        let Some(key) = self.active_session_key.clone() else {
            self.status = String::from("Connect to PostgreSQL before changing the schema");
            return;
        };
        let Some(session) = self.sessions.get(&key) else {
            self.status = String::from("Connect to PostgreSQL before changing the schema");
            return;
        };
        if let Err(error) = session.execute(sql) {
            self.status = format!("Could not submit schema operation: {error}");
            return;
        }

        let schema = dialog.schema;
        let table_to_reload = match dialog.action {
            SchemaAction::AddColumn | SchemaAction::RenameColumn | SchemaAction::DropColumn => {
                Some((
                    dialog.table,
                    self.current_table.as_ref().map_or(0, |t| t.offset),
                ))
            }
            SchemaAction::RenameTable => Some((dialog.new_name, 0)),
            SchemaAction::CreateTable | SchemaAction::DropTable => None,
        };

        let _ = session.list_tables(schema.clone());
        if let Some((table, offset)) = &table_to_reload {
            let _ = session.load_table(schema.clone(), table.clone(), *offset);
        }
        self.current_table = None;
        self.selected_column = None;
        if table_to_reload.is_some() {
            self.table_loading = true;
        }
        self.status = String::from("Applying schema change…");
    }

    fn submit_table_mutation(&mut self, sql: String) {
        if self.active_session_is_read_only() {
            self.status = String::from("Table changes are disabled in read-only mode");
            return;
        }
        let Some(table) = &self.current_table else {
            return;
        };
        let schema = table.schema.clone();
        let name = table.name.clone();
        let offset = table.offset;
        let Some(key) = self.active_session_key.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(&key) else {
            return;
        };
        let result = session.execute(sql).map(|()| {
            let _ = session.load_table(schema, name, offset);
        });
        match result {
            Ok(()) => {
                self.query_running = true;
                self.table_loading = true;
                self.status = String::from("Saving row…");
            }
            Err(error) => self.status = format!("Could not submit row change: {error}"),
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

    fn save_connection(&mut self) -> bool {
        let profile = match self.draft.to_profile(self.selected_profile_id) {
            Ok(profile) => profile,
            Err(error) => {
                self.status = error;
                return false;
            }
        };

        if !self.draft.password.is_empty()
            && let Err(error) = secrets::save_password(profile.id, &self.draft.password)
        {
            self.status = format!("Could not save password to the system keyring: {error}");
            return false;
        }

        if !self.draft.password.is_empty() {
            self.session_passwords
                .insert(profile.id, Zeroizing::new(self.draft.password.clone()));
        }

        if let Err(error) = storage::save_profile(&profile, &mut self.profiles) {
            self.status = format!("Could not save connection: {error}");
            return false;
        }

        self.selected_profile_id = Some(profile.id);
        self.connection_editor_open = false;
        self.status = format!("Saved connection ‘{}’", profile.name);
        true
    }

    fn start_connection_test(&mut self, context: &egui::Context) {
        if !self.apply_pending_connection_url() {
            return;
        }
        let profile = match self.draft.to_profile(self.selected_profile_id) {
            Ok(profile) => profile,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let mut password = if self.draft.password.is_empty() {
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
                Ok(runtime) => match runtime
                    .block_on(adapter(profile.engine).test_connection(&profile, &password))
                {
                    Ok(server_version) => format!("Connected successfully — {server_version}"),
                    Err(error) => format!("Connection failed: {error}"),
                },
                Err(error) => format!("Could not start the async runtime: {error}"),
            };
            password.zeroize();
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

fn schema_action_title(action: SchemaAction) -> &'static str {
    match action {
        SchemaAction::CreateTable => "Create table",
        SchemaAction::RenameTable => "Rename table",
        SchemaAction::DropTable => "Drop table",
        SchemaAction::AddColumn => "Add column",
        SchemaAction::RenameColumn => "Rename column",
        SchemaAction::DropColumn => "Drop column",
    }
}
