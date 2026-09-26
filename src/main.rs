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
    time::{Duration, Instant},
};

use backup::{decrypt_profiles, encrypt_profiles};
use connection::{ConnectionDraft, ConnectionProfile, TlsMode};
use database::{DatabaseInfo, Event as DatabaseEvent, QueryOutput, TableLoadStage};
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([1024.0, 680.0]),
        ..Default::default()
    };

    eframe::run_native(
        "SQL Manager",
        options,
        Box::new(|creation_context| {
            apply_app_theme(&creation_context.egui_ctx);
            Ok(Box::<SqlManagerApp>::default())
        }),
    )
}

fn apply_app_theme(context: &egui::Context) {
    context.set_theme(egui::Theme::Dark);
    let mut style = (*context.style_of(egui::Theme::Dark)).clone();
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(20, 22, 28);
    visuals.window_fill = egui::Color32::from_rgb(25, 28, 35);
    visuals.extreme_bg_color = egui::Color32::from_rgb(13, 15, 20);
    visuals.faint_bg_color = egui::Color32::from_rgb(30, 33, 41);
    visuals.code_bg_color = egui::Color32::from_rgb(16, 18, 23);
    visuals.text_edit_bg_color = Some(egui::Color32::from_rgb(16, 18, 23));
    visuals.override_text_color = Some(egui::Color32::from_rgb(229, 232, 239));
    visuals.weak_text_color = Some(egui::Color32::from_rgb(139, 146, 160));
    visuals.hyperlink_color = egui::Color32::from_rgb(143, 167, 255);
    visuals.selection.bg_fill = egui::Color32::from_rgb(60, 84, 158);
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(154, 174, 255));
    visuals.window_corner_radius = egui::CornerRadius::same(10);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(25, 28, 35);
    visuals.widgets.noninteractive.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(43, 47, 58));
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(34, 38, 47);
    visuals.widgets.inactive.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(53, 59, 72));
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(48, 55, 69);
    visuals.widgets.hovered.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(91, 111, 165));
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(66, 91, 168);
    visuals.widgets.active.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(135, 157, 233));
    visuals.widgets.open.bg_fill = egui::Color32::from_rgb(38, 43, 54);
    style.visuals = visuals;
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(13.0, 8.0);
    style.spacing.interact_size = egui::vec2(40.0, 30.0);
    style.spacing.window_margin = egui::Margin::same(16);
    context.set_style_of(egui::Theme::Dark, style);
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
    query_started_at: Option<Instant>,
    query_elapsed: Option<Duration>,
    current_table: Option<TableData>,
    table_loading: bool,
    table_loading_stage: Option<TableLoadStage>,
    table_load_started_at: Option<Instant>,
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
            query_started_at: None,
            query_elapsed: None,
            current_table: None,
            table_loading: false,
            table_loading_stage: None,
            table_load_started_at: None,
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

        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(17, 19, 25))
                    .inner_margin(egui::Margin::symmetric(18, 10))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(43, 47, 57))),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("▦")
                            .size(24.0)
                            .color(egui::Color32::from_rgb(143, 167, 255)),
                    );
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("SQL Manager").strong().size(16.0));
                        ui.label(egui::RichText::new("DATABASE WORKSPACE").weak().size(9.0));
                    });
                    ui.separator();
                    if self.screen == AppScreen::Workspace {
                        if ui.selectable_label(false, "Connections").clicked() {
                            self.screen = AppScreen::Connections;
                        }
                        if let Some(key) = &self.active_session_key {
                            let active_name = self
                                .session_profiles
                                .get(&key.profile_id)
                                .map_or("PostgreSQL", |profile| profile.name.as_str());
                            ui.separator();
                            ui.label(
                                egui::RichText::new("●")
                                    .color(egui::Color32::from_rgb(105, 207, 157)),
                            );
                            let full_session_name = format!("{active_name} / {}", key.database);
                            ui.label(
                                egui::RichText::new(compact_label(&full_session_name, 42)).strong(),
                            )
                            .on_hover_text(full_session_name);
                            if self.active_session_is_read_only() {
                                ui.label(
                                    egui::RichText::new("READ ONLY")
                                        .small()
                                        .color(egui::Color32::from_rgb(235, 190, 112)),
                                );
                            }
                        }
                        if ui.button("Disconnect").clicked() {
                            self.disconnect_session();
                        }
                    } else {
                        ui.separator();
                        ui.label(egui::RichText::new("Connection Manager").strong());
                        if self.active_session_is_connected()
                            && ui.button("Open workspace  ↗").clicked()
                        {
                            self.screen = AppScreen::Workspace;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("Backup  ▾", |ui| {
                            if ui.button("Import encrypted backup…").clicked() {
                                self.open_backup_dialog(BackupAction::Import);
                                ui.close();
                            }
                            if ui.button("Export encrypted backup…").clicked() {
                                self.open_backup_dialog(BackupAction::Export);
                                ui.close();
                            }
                        });
                    });
                });
            });

        egui::Panel::left("connections")
            .resizable(true)
            .default_size(252.0)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(20, 22, 28))
                    .inner_margin(egui::Margin::same(14))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(43, 47, 57))),
            )
            .show(ui, |ui| match self.screen {
                AppScreen::Connections => {
                    let mut selected = self.selected_profile_id;
                    let mut create_profile = false;
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("CONNECTIONS")
                                .strong()
                                .size(11.0)
                                .weak(),
                        );
                        if ui.small_button("＋ New").clicked() {
                            create_profile = true;
                        }
                    });
                    ui.add_space(6.0);

                    for profile in &self.profiles {
                        let mut profile_selected = false;
                        egui::Frame::new()
                            .fill(if selected == Some(profile.id) {
                                egui::Color32::from_rgb(34, 41, 57)
                            } else {
                                egui::Color32::from_rgb(25, 28, 35)
                            })
                            .stroke(egui::Stroke::new(
                                1.0,
                                if selected == Some(profile.id) {
                                    egui::Color32::from_rgb(78, 101, 163)
                                } else {
                                    egui::Color32::from_rgb(43, 47, 57)
                                },
                            ))
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if ui
                                    .selectable_label(
                                        selected == Some(profile.id),
                                        egui::RichText::new(&profile.name).strong(),
                                    )
                                    .clicked()
                                {
                                    profile_selected = true;
                                }
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} · {}",
                                        profile.host, profile.database
                                    ))
                                    .small()
                                    .weak(),
                                );
                            });
                        ui.add_space(5.0);
                        if profile_selected {
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

                    ui.add_space(18.0);
                    ui.label(
                        egui::RichText::new("OPEN SESSIONS")
                            .strong()
                            .size(11.0)
                            .weak(),
                    );
                    ui.add_space(6.0);
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
                        let display_label = if connected {
                            compact_label(&label, 30)
                        } else {
                            format!("◌  {} · Connecting", compact_label(&label, 30))
                        };
                        if ui
                            .selectable_label(
                                self.active_session_key.as_ref() == Some(&key),
                                display_label,
                            )
                            .on_hover_text(label)
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

                    ui.label(
                        egui::RichText::new("OPEN SESSIONS")
                            .strong()
                            .size(11.0)
                            .weak(),
                    );
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
                        let connected = self.connected_sessions.contains(&key);
                        let display_label = if connected {
                            format!("●  {}", compact_label(&label, 30))
                        } else {
                            format!("◌  {} · Connecting", compact_label(&label, 30))
                        };
                        if ui
                            .selectable_label(
                                self.active_session_key.as_ref() == Some(&key),
                                display_label,
                            )
                            .on_hover_text(label)
                            .clicked()
                        {
                            requested_session = Some(key);
                        }
                    }
                    if let Some(key) = requested_session {
                        self.activate_session(key);
                    }

                    ui.add_space(20.0);
                    ui.label(
                        egui::RichText::new("DATABASE NAVIGATOR")
                            .strong()
                            .size(11.0)
                            .weak(),
                    );
                    ui.add_space(7.0);
                    ui.label(egui::RichText::new(format!("▣  {}", active_key.database)).strong());
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
                    ui.label(egui::RichText::new("DATABASES").small().weak());
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
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new("SCHEMAS").small().weak());

                    let mut requested_schema = None;
                    egui::ScrollArea::vertical()
                        .id_salt("schema_list")
                        .max_height(170.0)
                        .show(ui, |ui| {
                            for schema in &self.schemas {
                                if ui
                                    .selectable_label(
                                        self.selected_schema.as_ref() == Some(schema),
                                        format!("◫  {schema}"),
                                    )
                                    .clicked()
                                {
                                    requested_schema = Some(schema.clone());
                                }
                            }
                        });
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
                            ui.label(egui::RichText::new("TABLES").small().weak());
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
                            && let Some(schema) = self.selected_schema.clone()
                        {
                            self.start_table_page_load(schema, table.clone(), 0);
                            if self.table_loading {
                                self.workspace_tab = WorkspaceTab::TableData;
                            }
                        }
                    }
                }
            });

        self.refresh_test_status();
        egui::Panel::bottom("status_bar")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(16, 18, 23))
                    .inner_margin(egui::Margin::symmetric(14, 7))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(43, 47, 57))),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let is_error = ["failed", "could not", "disconnected", "error"]
                        .iter()
                        .any(|marker| self.status.to_lowercase().contains(marker));
                    let status_color = if is_error {
                        egui::Color32::from_rgb(235, 112, 126)
                    } else {
                        egui::Color32::from_rgb(105, 207, 157)
                    };
                    ui.label(egui::RichText::new("●").color(status_color));
                    ui.label(egui::RichText::new(&self.status).small());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("SQL Manager · PostgreSQL")
                                .weak()
                                .small(),
                        );
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(20, 22, 28))
                    .inner_margin(egui::Margin::same(22)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("workspace_page")
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.screen {
                        AppScreen::Connections => self.show_connection_manager(ui),
                        AppScreen::Workspace => self.show_workspace(ui),
                    });
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
        self.query_started_at = None;
        self.query_elapsed = None;
        self.current_table = None;
        self.table_loading = false;
        self.table_loading_stage = None;
        self.table_load_started_at = None;
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

        ui.heading(
            egui::RichText::new("Connect to a database")
                .size(25.0)
                .strong(),
        );
        ui.label(
            egui::RichText::new("Choose a saved profile, test it, or create a new connection.")
                .weak(),
        );
        ui.add_space(20.0);

        let profile = self
            .selected_profile_id
            .and_then(|id| self.profiles.iter().find(|profile| profile.id == id))
            .cloned();
        let Some(profile) = profile else {
            egui::Frame::new()
                .fill(egui::Color32::from_rgb(25, 28, 35))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(47, 52, 64)))
                .corner_radius(egui::CornerRadius::same(12))
                .inner_margin(egui::Margin::same(26))
                .show(ui, |ui| {
                    ui.set_min_height(190.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("◉")
                                .size(30.0)
                                .color(egui::Color32::from_rgb(143, 167, 255)),
                        );
                        ui.add_space(8.0);
                        ui.heading("Your connections start here");
                        ui.label(
                            egui::RichText::new(
                                "Save a PostgreSQL profile to open a secure, persistent workspace.",
                            )
                            .weak(),
                        );
                        ui.add_space(14.0);
                        if ui
                            .button(
                                egui::RichText::new("＋  Create connection")
                                    .color(egui::Color32::WHITE),
                            )
                            .clicked()
                        {
                            self.selected_profile_id = None;
                            self.draft = ConnectionDraft::default();
                            self.connection_editor_open = true;
                        }
                    });
                });
            return;
        };

        egui::Frame::new()
            .fill(egui::Color32::from_rgb(25, 28, 35))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(47, 52, 64)))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("▣")
                            .size(23.0)
                            .color(egui::Color32::from_rgb(143, 167, 255)),
                    );
                    ui.vertical(|ui| {
                        ui.heading(egui::RichText::new(&profile.name).size(20.0));
                        ui.label(
                            egui::RichText::new(format!("{} · {}", profile.host, profile.database))
                                .weak(),
                        );
                    });
                });
                ui.add_space(16.0);
                egui::Grid::new("profile_summary")
                    .num_columns(4)
                    .spacing([16.0, 12.0])
                    .show(ui, |ui| {
                        ui.label("Engine");
                        ui.strong(profile.engine.label());
                        ui.label("Host");
                        ui.strong(format!("{}:{}", profile.host, profile.port));
                        ui.end_row();
                        ui.label("Database");
                        ui.strong(&profile.database);
                        ui.label("Username");
                        ui.strong(&profile.username);
                        ui.end_row();
                        ui.label("TLS");
                        ui.strong(profile.tls_mode.label());
                        ui.label("Execution");
                        ui.strong(if profile.read_only {
                            "Read Only"
                        } else {
                            "Standard"
                        });
                        ui.end_row();
                        if let Some(ssh) = &profile.ssh_tunnel {
                            ui.label("SSH tunnel");
                            ui.strong(format!("{}@{}:{}", ssh.username, ssh.host, ssh.port));
                            ui.label("Database browser");
                            ui.strong(if profile.show_all_databases {
                                "All databases"
                            } else {
                                "Selected only"
                            });
                            ui.end_row();
                        }
                    });
            });

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button("Edit profile").clicked() {
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
                if ui.button("Open workspace").clicked() {
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
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Connect to database")
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(74, 100, 184))
                        .corner_radius(egui::CornerRadius::same(7)),
                    )
                    .clicked()
                {
                    self.open_database_for_profile(profile.id, profile.database.clone());
                }
            }
        });
    }

    fn show_connection_editor(&mut self, ui: &mut egui::Ui) {
        ui.heading(
            egui::RichText::new(if self.selected_profile_id.is_some() {
                "Connection profile"
            } else {
                "New connection"
            })
            .size(25.0)
            .strong(),
        );
        ui.label(egui::RichText::new("Configure endpoint, access and workspace defaults.").weak());
        ui.add_space(18.0);

        let mut parse_connection_url = false;
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(25, 28, 35))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(47, 52, 64)))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(egui::Margin::same(18))
            .show(ui, |ui| {
                egui::Grid::new("connection_form")
                    .num_columns(2)
                    .spacing([16.0, 12.0])
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
                        ui.add(
                            egui::TextEdit::singleline(&mut self.draft.port).desired_width(90.0),
                        );
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
                                    ui.selectable_value(
                                        &mut self.draft.tls_mode,
                                        mode,
                                        mode.label(),
                                    );
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
                                egui::TextEdit::singleline(&mut self.draft.ssh_port)
                                    .desired_width(90.0),
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
                            ui.label(
                                "Uses ssh-agent or this key file; trust the SSH host key first.",
                            );
                            ui.end_row();
                        }
                    });
            });

        if parse_connection_url {
            self.status = match self.draft.apply_connection_url() {
                Ok(()) => String::from("Connection fields filled from URL"),
                Err(error) => format!("Could not parse connection URL: {error}"),
            };
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button("Save profile").clicked() {
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
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Save and connect")
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(74, 100, 184))
                    .corner_radius(egui::CornerRadius::same(7)),
                )
                .clicked()
                && self.save_connection()
            {
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
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(25, 28, 35))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(47, 52, 64)))
            .corner_radius(egui::CornerRadius::same(11))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("SQL QUERY").strong().size(11.0).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.active_session_is_read_only() {
                            ui.label(
                                egui::RichText::new("READ ONLY")
                                    .small()
                                    .color(egui::Color32::from_rgb(235, 190, 112)),
                            );
                        }
                        ui.label(egui::RichText::new("PostgreSQL").small().weak());
                    });
                });
                ui.add_space(8.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.sql)
                        .code_editor()
                        .desired_rows(15)
                        .desired_width(f32::INFINITY),
                );
            });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.query_running && !self.sql.trim().is_empty(),
                    egui::Button::new(
                        egui::RichText::new(if self.active_session_is_read_only() {
                            "▶  Run read-only query"
                        } else {
                            "▶  Run query"
                        })
                        .strong()
                        .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(74, 100, 184))
                    .corner_radius(egui::CornerRadius::same(7)),
                )
                .clicked()
            {
                self.start_sql_query();
            }
            if let Some(started_at) = self.query_started_at {
                ui.spinner();
                ui.label(format!(
                    "Running · {:.1}s",
                    started_at.elapsed().as_secs_f32()
                ));
            } else if let Some(elapsed) = self.query_elapsed {
                ui.label(format!("Completed in {:.2}s", elapsed.as_secs_f64()));
            }
        });

        self.show_query_results(ui);
    }

    fn start_sql_query(&mut self) {
        if self.query_running || self.sql.trim().is_empty() {
            return;
        }
        let Some(session) = self.active_session() else {
            self.status = String::from("Open a database session before running SQL");
            return;
        };
        match session.execute(self.sql.clone()) {
            Ok(()) => {
                self.query_running = true;
                self.query_started_at = Some(Instant::now());
                self.query_elapsed = None;
                self.status = String::from("Running query…");
            }
            Err(error) => self.status = format!("Could not submit query: {error}"),
        }
    }

    fn show_query_results(&self, ui: &mut egui::Ui) {
        let Some(result) = &self.query_result else {
            return;
        };
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.heading("Results");
            ui.label(egui::RichText::new(&result.summary).weak());
        });

        for (result_index, result_set) in result.result_sets.iter().enumerate() {
            if !result_set.columns.is_empty() {
                let available_width = ui.available_width().max(360.0);
                let column_width = if result_set.columns.len() == 1 {
                    available_width
                } else {
                    (available_width / result_set.columns.len() as f32).clamp(150.0, 320.0)
                };
                egui::ScrollArea::both()
                    .id_salt(("query_result", result_index))
                    .max_height(360.0)
                    .show_rows(ui, 28.0, result_set.rows.len() + 1, |ui, visible_rows| {
                        egui::Grid::new(("query_result_grid", result_index))
                            .striped(true)
                            .show(ui, |ui| {
                                for row_index in visible_rows {
                                    if row_index == 0 {
                                        for column in &result_set.columns {
                                            ui.add_sized(
                                                [column_width, 24.0],
                                                egui::Label::new(
                                                    egui::RichText::new(column).strong(),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(column);
                                        }
                                    } else if let Some(row) = result_set.rows.get(row_index - 1) {
                                        for value in row {
                                            let text = value.as_deref().unwrap_or("NULL");
                                            ui.add_sized(
                                                [column_width, 24.0],
                                                egui::Label::new(text).truncate(),
                                            )
                                            .on_hover_text(text);
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            }
            if result_set.truncated {
                ui.label("Showing the first 1,000 rows for this result set.");
            }
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
                DatabaseEvent::TableDataProgress(stage) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.table_loading_stage = Some(stage);
                    }
                }
                DatabaseEvent::TableData(Ok(table)) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.table_loading = false;
                        self.table_loading_stage = None;
                        let elapsed = self
                            .table_load_started_at
                            .take()
                            .map(|start| start.elapsed());
                        self.status = elapsed.map_or_else(
                            || format!("Loaded {}.{}", table.schema, table.name),
                            |elapsed| {
                                format!(
                                    "Loaded {}.{} · {:.2}s",
                                    table.schema,
                                    table.name,
                                    elapsed.as_secs_f64()
                                )
                            },
                        );
                        self.current_table = Some(table);
                    }
                }
                DatabaseEvent::TableData(Err(error)) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.table_loading = false;
                        self.table_loading_stage = None;
                        let elapsed = self
                            .table_load_started_at
                            .take()
                            .map(|start| start.elapsed());
                        self.status = elapsed.map_or_else(
                            || format!("Could not load table: {error}"),
                            |elapsed| {
                                format!(
                                    "Could not load table after {:.2}s: {error}",
                                    elapsed.as_secs_f64()
                                )
                            },
                        );
                    }
                }
                DatabaseEvent::QueryProgress(result) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.query_result = Some(result);
                        self.status = String::from("First result batch ready · query continues");
                    }
                }
                DatabaseEvent::Query(Ok(result)) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.query_running = false;
                        self.query_elapsed =
                            self.query_started_at.take().map(|start| start.elapsed());
                        self.status = self.query_elapsed.map_or_else(
                            || result.summary.clone(),
                            |elapsed| format!("{} · {:.2}s", result.summary, elapsed.as_secs_f64()),
                        );
                        self.query_result = Some(result);
                    }
                }
                DatabaseEvent::Query(Err(error)) => {
                    if self.active_session_key.as_ref() == Some(&key) {
                        self.query_running = false;
                        self.query_elapsed =
                            self.query_started_at.take().map(|start| start.elapsed());
                        self.status = self.query_elapsed.map_or_else(
                            || format!("Query failed: {error}"),
                            |elapsed| {
                                format!("Query failed after {:.2}s: {error}", elapsed.as_secs_f64())
                            },
                        );
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
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    self.table_loading_stage
                        .map(TableLoadStage::label)
                        .unwrap_or("Starting table data request"),
                );
                if let Some(started) = self.table_load_started_at {
                    ui.label(
                        egui::RichText::new(format!("{:.1}s", started.elapsed().as_secs_f32()))
                            .weak(),
                    );
                }
            });
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
        let schema = table.schema.clone();
        let name = table.name.clone();
        self.start_table_page_load(schema, name, offset);
    }

    fn start_table_page_load(&mut self, schema: String, table: String, offset: u64) {
        let result = self
            .active_session()
            .map(|session| session.load_table(schema, table, offset));
        match result {
            Some(Ok(())) => {
                self.table_loading = true;
                self.table_loading_stage = Some(TableLoadStage::Metadata);
                self.table_load_started_at = Some(Instant::now());
            }
            Some(Err(error)) => self.status = format!("Could not load table page: {error}"),
            None => self.status = String::from("Open a database session before loading table data"),
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
            self.table_loading_stage = Some(TableLoadStage::Metadata);
            self.table_load_started_at = Some(Instant::now());
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
                self.table_loading_stage = Some(TableLoadStage::Metadata);
                self.table_load_started_at = Some(Instant::now());
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

fn compact_label(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let compact = chars
        .by_ref()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    if chars.next().is_some() {
        format!("{compact}…")
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod ui_tests {
    use std::{env, thread, time::Instant};

    use super::{SqlManagerApp, apply_app_theme, database::QueryOutput, database::ResultSet, egui};
    use crate::{connection::ConnectionDraft, database::PostgresSession};

    #[test]
    fn query_results_render_only_visible_rows() {
        let app = SqlManagerApp {
            query_result: Some(QueryOutput {
                summary: String::from("1,000 rows returned"),
                result_sets: vec![ResultSet {
                    columns: vec![String::from("id"), String::from("payload")],
                    rows: (0..1_000)
                        .map(|index| vec![Some(index.to_string()), Some(String::from("value"))])
                        .collect(),
                    truncated: true,
                }],
            }),
            ..SqlManagerApp::default()
        };

        let context = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_100.0, 760.0),
            )),
            ..Default::default()
        };
        let output = context.run_ui(input, |ui| app.show_query_results(ui));
        let paint_shape_count = output.shapes.len();
        output.drop_without_applying_deltas();

        assert!(
            paint_shape_count < 500,
            "a 1,000-row result created {} paint shapes; rows should be virtualized",
            paint_shape_count
        );
    }

    #[test]
    fn workspace_scrolls_when_editor_and_results_exceed_window_height() {
        let key = super::SessionKey {
            profile_id: uuid::Uuid::new_v4(),
            database: String::from("test"),
        };
        let mut app = SqlManagerApp {
            screen: super::AppScreen::Workspace,
            active_session_key: Some(key.clone()),
            connected_sessions: std::collections::HashSet::from([key]),
            query_result: Some(QueryOutput {
                summary: String::from("1,000 rows returned"),
                result_sets: vec![ResultSet {
                    columns: vec![String::from("id"), String::from("payload")],
                    rows: (0..1_000)
                        .map(|index| vec![Some(index.to_string()), Some(String::from("value"))])
                        .collect(),
                    truncated: true,
                }],
            }),
            ..SqlManagerApp::default()
        };

        let context = egui::Context::default();
        apply_app_theme(&context);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_100.0, 480.0),
            )),
            ..Default::default()
        };
        let output = context.run_ui(input, |ui| {
            let page = egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| app.show_workspace(ui));
            assert!(
                page.content_size.y > page.inner_rect.height(),
                "short workspace viewport did not expose a vertical scroll range"
            );
        });
        output.drop_without_applying_deltas();
    }

    #[test]
    #[ignore = "requires SQL_MANAGER_E2E_DATABASE_URL and a local PostgreSQL server"]
    fn workspace_e2e_shows_partial_query_results_before_completion() {
        let mut draft = ConnectionDraft {
            connection_url: env::var("SQL_MANAGER_E2E_DATABASE_URL")
                .expect("set SQL_MANAGER_E2E_DATABASE_URL"),
            ..ConnectionDraft::default()
        };
        draft
            .apply_connection_url()
            .expect("valid PostgreSQL E2E URL");
        let password = draft.password.clone();
        let profile = draft.to_profile(None).expect("valid profile");
        let key = super::SessionKey {
            profile_id: profile.id,
            database: profile.database.clone(),
        };

        let mut app = SqlManagerApp::default();
        app.sessions.insert(
            key.clone(),
            Box::new(PostgresSession::connect(profile.clone(), password)),
        );
        app.active_session_key = Some(key.clone());
        app.session_profiles.insert(profile.id, profile);
        app.session_read_only.insert(key.clone(), false);

        let connect_deadline = Instant::now() + std::time::Duration::from_secs(15);
        while !app.connected_sessions.contains(&key) {
            app.refresh_database_events();
            assert!(
                Instant::now() < connect_deadline,
                "workspace failed to connect"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }

        app.sql = String::from("SELECT i, pg_sleep(0.001) FROM generate_series(1, 10000) i");
        app.start_sql_query();
        let first_batch_started = Instant::now();
        let first_batch_deadline = Instant::now() + std::time::Duration::from_secs(8);
        while app.query_result.is_none() {
            app.refresh_database_events();
            assert!(
                Instant::now() < first_batch_deadline,
                "workspace did not display a partial result before the full query finished"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }
        let first_batch_elapsed = first_batch_started.elapsed();
        assert!(
            app.query_running,
            "partial output should arrive while SQL continues"
        );
        assert!(app.query_result.as_ref().is_some_and(|result| {
            result
                .result_sets
                .first()
                .is_some_and(|set| set.rows.len() == 1_000)
        }));

        let context = egui::Context::default();
        apply_app_theme(&context);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1_100.0, 760.0),
            )),
            ..Default::default()
        };
        let render_output = context.run_ui(input, |ui| app.show_query_results(ui));
        let rendered_shapes = render_output.shapes.len();
        render_output.drop_without_applying_deltas();
        assert!(
            rendered_shapes < 500,
            "workspace painted {rendered_shapes} shapes for its virtualized first batch"
        );

        let completion_deadline = Instant::now() + std::time::Duration::from_secs(30);
        while app.query_running {
            app.refresh_database_events();
            assert!(
                Instant::now() < completion_deadline,
                "workspace query did not complete"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.query_elapsed.is_some());
        eprintln!(
            "Workspace PostgreSQL E2E — first batch {first_batch_elapsed:?}; painted {rendered_shapes} shapes"
        );
    }
}
