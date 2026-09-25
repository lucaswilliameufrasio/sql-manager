use std::{
    future::Future,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use futures_util::TryStreamExt;
use tokio::runtime::Runtime;
use tokio_postgres::{Client, Config, SimpleQueryMessage, config::SslMode, tls::MakeTlsConnect};
use tokio_postgres_rustls::MakeRustlsConnect;
use zeroize::Zeroize;

use crate::schema::{ColumnInfo, TABLE_PAGE_SIZE, TableData, quote_identifier};
use crate::{
    connection::{ConnectionProfile, TlsMode},
    ssh_tunnel::SshTunnel,
};

const MAX_RESULT_ROWS: usize = 1_000;

pub struct PostgresSession {
    commands: Sender<Command>,
    pub events: Receiver<Event>,
    read_only: bool,
}

pub enum Event {
    Connected,
    Databases(Result<Vec<DatabaseInfo>, String>),
    Schemas(Vec<String>),
    Tables { schema: String, tables: Vec<String> },
    TableData(Result<TableData, String>),
    QueryProgress(QueryOutput),
    Query(Result<QueryOutput, String>),
    Disconnected(String),
}

#[derive(Clone, Debug)]
pub struct DatabaseInfo {
    pub name: String,
    pub is_template: bool,
    pub allows_connections: bool,
    pub has_connect_privilege: bool,
}

impl DatabaseInfo {
    pub fn is_connectable(&self) -> bool {
        self.allows_connections && self.has_connect_privilege
    }
}

#[derive(Clone)]
pub struct QueryOutput {
    pub result_sets: Vec<ResultSet>,
    pub summary: String,
}

#[derive(Clone)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
}

enum Command {
    ListSchemas,
    ListTables(String),
    LoadTable {
        schema: String,
        table: String,
        offset: u64,
    },
    Query(String, bool),
}

impl PostgresSession {
    pub fn connect(profile: ConnectionProfile, mut password: String) -> Self {
        let read_only = profile.read_only;
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();

        thread::spawn(move || {
            let runtime = match Runtime::new() {
                Ok(runtime) => runtime,
                Err(error) => {
                    password.zeroize();
                    let _ = event_sender.send(Event::Disconnected(format!(
                        "Could not start the async runtime: {error}"
                    )));
                    return;
                }
            };

            let config = build_config(&profile, &password);
            let mut tunnel = match profile
                .ssh_tunnel
                .as_ref()
                .map(|ssh| SshTunnel::start(ssh, &profile.host, profile.port))
                .transpose()
            {
                Ok(tunnel) => tunnel,
                Err(error) => {
                    password.zeroize();
                    let _ = event_sender.send(Event::Disconnected(error));
                    return;
                }
            };
            let tls_disabled = profile.tls_mode == TlsMode::Disable;
            if let Some(tunnel_ref) = tunnel.as_ref() {
                let stream = match runtime.block_on(tokio::net::TcpStream::connect((
                    "127.0.0.1",
                    tunnel_ref.local_port,
                ))) {
                    Ok(stream) => stream,
                    Err(error) => {
                        password.zeroize();
                        let _ = event_sender.send(Event::Disconnected(format!(
                            "Could not connect through the SSH tunnel: {error}"
                        )));
                        return;
                    }
                };

                if tls_disabled {
                    let result =
                        runtime.block_on(config.connect_raw(stream, tokio_postgres::NoTls));
                    password.zeroize();
                    handle_connection_result(
                        runtime,
                        result,
                        command_receiver,
                        event_sender,
                        tunnel.take(),
                    );
                } else {
                    let _ = rustls::crypto::ring::default_provider().install_default();
                    let mut tls = MakeRustlsConnect::with_webpki_roots();
                    let connector = <MakeRustlsConnect as MakeTlsConnect<tokio::net::TcpStream>>::make_tls_connect(
                        &mut tls,
                        &profile.host,
                    )
                        .expect("Rustls TLS connector is infallible");
                    let result = runtime.block_on(config.connect_raw(stream, connector));
                    password.zeroize();
                    handle_connection_result(
                        runtime,
                        result,
                        command_receiver,
                        event_sender,
                        tunnel.take(),
                    );
                }
            } else if tls_disabled {
                let result = runtime.block_on(config.connect(tokio_postgres::NoTls));
                password.zeroize();
                handle_connection_result(runtime, result, command_receiver, event_sender, tunnel);
            } else {
                let _ = rustls::crypto::ring::default_provider().install_default();
                let result =
                    runtime.block_on(config.connect(MakeRustlsConnect::with_webpki_roots()));
                password.zeroize();
                handle_connection_result(runtime, result, command_receiver, event_sender, tunnel);
            }
        });

        Self {
            commands: command_sender,
            events: event_receiver,
            read_only,
        }
    }

    pub fn list_tables(&self, schema: String) -> Result<(), String> {
        self.commands
            .send(Command::ListTables(schema))
            .map_err(|error| error.to_string())
    }

    pub fn list_schemas(&self) -> Result<(), String> {
        self.commands
            .send(Command::ListSchemas)
            .map_err(|error| error.to_string())
    }

    pub fn execute(&self, sql: String) -> Result<(), String> {
        self.commands
            .send(Command::Query(sql, self.read_only))
            .map_err(|error| error.to_string())
    }

    pub fn load_table(&self, schema: String, table: String, offset: u64) -> Result<(), String> {
        self.commands
            .send(Command::LoadTable {
                schema,
                table,
                offset,
            })
            .map_err(|error| error.to_string())
    }
}

impl crate::engine::DatabaseSession for PostgresSession {
    fn list_schemas(&self) -> Result<(), String> {
        PostgresSession::list_schemas(self)
    }

    fn list_tables(&self, schema: String) -> Result<(), String> {
        PostgresSession::list_tables(self, schema)
    }

    fn execute(&self, sql: String) -> Result<(), String> {
        PostgresSession::execute(self, sql)
    }

    fn load_table(&self, schema: String, table: String, offset: u64) -> Result<(), String> {
        PostgresSession::load_table(self, schema, table, offset)
    }

    fn drain_events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

fn build_config(profile: &ConnectionProfile, password: &str) -> Config {
    let ssl_mode = match profile.tls_mode {
        TlsMode::Require => SslMode::Require,
        TlsMode::Prefer => SslMode::Prefer,
        TlsMode::Disable => SslMode::Disable,
    };
    let mut config = Config::new();
    config
        .host(&profile.host)
        .port(profile.port)
        .dbname(&profile.database)
        .user(&profile.username)
        .password(password)
        .ssl_mode(ssl_mode);
    config
}

fn run_session<C>(
    runtime: Runtime,
    mut client: Client,
    connection: C,
    commands: Receiver<Command>,
    events: Sender<Event>,
    _tunnel: Option<SshTunnel>,
) where
    C: Future<Output = Result<(), tokio_postgres::Error>> + Send + 'static,
{
    let connection_events = events.clone();
    runtime.spawn(async move {
        if let Err(error) = connection.await {
            let _ = connection_events.send(Event::Disconnected(error.to_string()));
        }
    });

    let _ = events.send(Event::Connected);
    runtime.block_on(async move {
        match list_databases(&client).await {
            Ok(databases) => {
                let _ = events.send(Event::Databases(Ok(databases)));
            }
            Err(error) => {
                let _ = events.send(Event::Databases(Err(error.to_string())));
            }
        }

        match list_schemas(&client).await {
            Ok(schemas) => {
                let _ = events.send(Event::Schemas(schemas));
            }
            Err(error) => {
                let _ = events.send(Event::Disconnected(error.to_string()));
                return;
            }
        }

        while let Ok(command) = commands.recv() {
            match command {
                Command::ListSchemas => match list_schemas(&client).await {
                    Ok(schemas) => {
                        let _ = events.send(Event::Schemas(schemas));
                    }
                    Err(error) => {
                        let _ = events.send(Event::Disconnected(error.to_string()));
                        return;
                    }
                },
                Command::ListTables(schema) => match list_tables(&client, &schema).await {
                    Ok(tables) => {
                        let _ = events.send(Event::Tables { schema, tables });
                    }
                    Err(error) => {
                        let _ = events.send(Event::Disconnected(error.to_string()));
                        return;
                    }
                },
                Command::LoadTable {
                    schema,
                    table,
                    offset,
                } => {
                    let result = load_table(&client, &schema, &table, offset)
                        .await
                        .map_err(|error| error.to_string());
                    let _ = events.send(Event::TableData(result));
                }
                Command::Query(sql, read_only) => {
                    let result = execute_query(&mut client, &sql, read_only, &events).await;
                    let _ = events.send(Event::Query(result));
                }
            }
        }
    });
}

async fn list_databases(client: &Client) -> Result<Vec<DatabaseInfo>, tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT datname, datistemplate, datallowconn, \
                    has_database_privilege(datname, 'CONNECT') \
             FROM pg_catalog.pg_database \
             ORDER BY datname",
            &[],
        )
        .await?;

    Ok(rows
        .into_iter()
        .map(|row| DatabaseInfo {
            name: row.get(0),
            is_template: row.get(1),
            allows_connections: row.get(2),
            has_connect_privilege: row.get(3),
        })
        .collect())
}

fn handle_connection_result<C>(
    runtime: Runtime,
    result: Result<(Client, C), tokio_postgres::Error>,
    commands: Receiver<Command>,
    events: Sender<Event>,
    tunnel: Option<SshTunnel>,
) where
    C: Future<Output = Result<(), tokio_postgres::Error>> + Send + 'static,
{
    match result {
        Ok((client, connection)) => {
            run_session(runtime, client, connection, commands, events, tunnel)
        }
        Err(error) => {
            let _ = events.send(Event::Disconnected(error.to_string()));
        }
    }
}

async fn load_table(
    client: &Client,
    schema: &str,
    table: &str,
    offset: u64,
) -> Result<TableData, tokio_postgres::Error> {
    let column_rows = client
        .query(
            "SELECT column_name, data_type, is_nullable, column_default \
             FROM information_schema.columns \
             WHERE table_schema = $1 AND table_name = $2 \
             ORDER BY ordinal_position",
            &[&schema, &table],
        )
        .await?;
    let columns = column_rows
        .into_iter()
        .map(|row| ColumnInfo {
            name: row.get(0),
            data_type: row.get(1),
            nullable: row.get::<_, String>(2) == "YES",
            default: row.get(3),
        })
        .collect::<Vec<_>>();

    let key_rows = client
        .query(
            "SELECT kcu.column_name \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON tc.constraint_name = kcu.constraint_name \
              AND tc.table_schema = kcu.table_schema \
              AND tc.table_name = kcu.table_name \
             WHERE tc.constraint_type = 'PRIMARY KEY' \
               AND tc.table_schema = $1 AND tc.table_name = $2 \
             ORDER BY kcu.ordinal_position",
            &[&schema, &table],
        )
        .await?;
    let primary_key = key_rows
        .into_iter()
        .map(|row| row.get(0))
        .collect::<Vec<String>>();

    let order_by = if primary_key.is_empty() {
        String::new()
    } else {
        format!(
            " ORDER BY {}",
            primary_key
                .iter()
                .map(|key| quote_identifier(key))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let query = format!(
        "SELECT * FROM {}.{}{} LIMIT {} OFFSET {}",
        quote_identifier(schema),
        quote_identifier(table),
        order_by,
        TABLE_PAGE_SIZE + 1,
        offset
    );
    let messages = client.simple_query(&query).await?;
    let mut result_columns = Vec::new();
    let mut rows = Vec::new();
    for message in messages {
        match message {
            SimpleQueryMessage::RowDescription(description) => {
                result_columns = description
                    .iter()
                    .map(|column| column.name().to_owned())
                    .collect();
            }
            SimpleQueryMessage::Row(row) => rows.push(
                (0..row.len())
                    .map(|index| row.get(index).map(str::to_owned))
                    .collect::<Vec<_>>(),
            ),
            _ => {}
        }
    }

    let has_more = rows.len() > TABLE_PAGE_SIZE;
    rows.truncate(TABLE_PAGE_SIZE);
    let columns = if result_columns.len() == columns.len() {
        columns
    } else {
        result_columns
            .into_iter()
            .map(|name| ColumnInfo {
                name,
                data_type: String::new(),
                nullable: true,
                default: None,
            })
            .collect()
    };

    Ok(TableData {
        schema: schema.to_owned(),
        name: table.to_owned(),
        offset,
        columns,
        primary_key,
        rows,
        has_more,
    })
}

async fn list_schemas(client: &Client) -> Result<Vec<String>, tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT schema_name \
             FROM information_schema.schemata \
             WHERE schema_name NOT IN ('pg_catalog', 'information_schema') \
             ORDER BY schema_name",
            &[],
        )
        .await?;
    Ok(rows.into_iter().map(|row| row.get(0)).collect())
}

async fn list_tables(client: &Client, schema: &str) -> Result<Vec<String>, tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT table_name \
             FROM information_schema.tables \
             WHERE table_schema = $1 \
             ORDER BY table_name",
            &[&schema],
        )
        .await?;
    Ok(rows.into_iter().map(|row| row.get(0)).collect())
}

async fn execute_query(
    client: &mut Client,
    sql: &str,
    read_only: bool,
    events: &Sender<Event>,
) -> Result<QueryOutput, String> {
    if read_only {
        validate_read_only_sql(sql)?;
        client
            .batch_execute("BEGIN TRANSACTION READ ONLY")
            .await
            .map_err(|error| postgres_error_text(&error))?;
    }

    let result = stream_query(client, sql, events).await;
    if read_only {
        let transaction_command = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        if let Err(finish_error) = client.batch_execute(transaction_command).await {
            let finish_error = postgres_error_text(&finish_error);
            return Err(match result {
                Ok(_) => format!(
                    "Query succeeded but could not commit read-only transaction: {finish_error}"
                ),
                Err(query_error) => format!(
                    "{query_error}; could not roll back read-only transaction: {finish_error}"
                ),
            });
        }
    }
    result
}

async fn stream_query(
    client: &Client,
    sql: &str,
    events: &Sender<Event>,
) -> Result<QueryOutput, String> {
    let stream = client
        .simple_query_raw(sql)
        .await
        .map_err(|error| postgres_error_text(&error))?;
    let mut stream = std::pin::pin!(stream);
    let mut output = QueryAccumulator::default();
    while let Some(message) = stream
        .as_mut()
        .try_next()
        .await
        .map_err(|error| postgres_error_text(&error))?
    {
        if output.push(message) {
            let _ = events.send(Event::QueryProgress(output.snapshot()));
        }
    }
    Ok(output.finish())
}

fn postgres_error_text(error: &tokio_postgres::Error) -> String {
    error.as_db_error().map_or_else(
        || error.to_string(),
        |database_error| {
            format!(
                "{} (SQLSTATE {:?})",
                database_error.message(),
                database_error.code()
            )
        },
    )
}

fn validate_read_only_sql(sql: &str) -> Result<(), String> {
    let statement = sql.trim();
    let statement = statement.strip_suffix(';').unwrap_or(statement).trim_end();
    if statement.is_empty() {
        return Err(String::from("Enter a SQL statement"));
    }
    if statement.contains(';') {
        return Err(String::from(
            "Read-only mode accepts one SQL statement at a time",
        ));
    }

    let mut remaining = statement;
    loop {
        remaining = remaining.trim_start();
        if let Some(comment) = remaining.strip_prefix("--") {
            remaining = comment.split_once('\n').map_or("", |(_, rest)| rest);
            continue;
        }
        let Some(comment) = remaining.strip_prefix("/*") else {
            break;
        };
        let bytes = comment.as_bytes();
        let mut depth = 1_usize;
        let mut index = 0;
        while index + 1 < bytes.len() && depth > 0 {
            match &bytes[index..index + 2] {
                b"/*" => {
                    depth += 1;
                    index += 2;
                }
                b"*/" => {
                    depth -= 1;
                    index += 2;
                }
                _ => index += 1,
            }
        }
        if depth > 0 {
            return Err(String::from("Unterminated SQL comment"));
        }
        remaining = &comment[index..];
    }

    let keyword = remaining
        .split(|character: char| !character.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if matches!(
        keyword.as_str(),
        "BEGIN" | "COMMIT" | "END" | "ROLLBACK" | "ABORT" | "START" | "SAVEPOINT" | "RELEASE"
    ) {
        return Err(String::from(
            "Transaction-control statements are unavailable in read-only mode",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct QueryAccumulator {
    result_sets: Vec<ResultSet>,
    command_count: u64,
    progress_sent: bool,
}

impl QueryAccumulator {
    fn push(&mut self, message: SimpleQueryMessage) -> bool {
        let mut reached_result_limit = false;
        match message {
            SimpleQueryMessage::RowDescription(columns) => {
                self.result_sets.push(ResultSet {
                    columns: columns
                        .iter()
                        .map(|column| column.name().to_owned())
                        .collect(),
                    rows: Vec::new(),
                    truncated: false,
                });
            }
            SimpleQueryMessage::Row(row) => {
                if self.result_sets.is_empty() {
                    self.result_sets.push(ResultSet {
                        columns: row
                            .columns()
                            .iter()
                            .map(|column| column.name().to_owned())
                            .collect(),
                        rows: Vec::new(),
                        truncated: false,
                    });
                }
                if let Some(result_set) = self.result_sets.last_mut() {
                    if result_set.rows.len() < MAX_RESULT_ROWS {
                        result_set.rows.push(
                            (0..row.len())
                                .map(|index| row.get(index).map(str::to_owned))
                                .collect(),
                        );
                    } else {
                        result_set.truncated = true;
                        reached_result_limit = true;
                    }
                }
            }
            SimpleQueryMessage::CommandComplete(_) => self.command_count += 1,
            _ => {}
        }
        if reached_result_limit && !self.progress_sent {
            self.progress_sent = true;
            return true;
        }
        false
    }

    fn finish(self) -> QueryOutput {
        let summary = self.summary();
        QueryOutput {
            result_sets: self.result_sets,
            summary,
        }
    }

    fn snapshot(&self) -> QueryOutput {
        QueryOutput {
            result_sets: self.result_sets.clone(),
            summary: self.summary(),
        }
    }

    fn summary(&self) -> String {
        let row_count = self
            .result_sets
            .iter()
            .map(|set| set.rows.len())
            .sum::<usize>();
        let truncated = self.result_sets.iter().any(|set| set.truncated);
        if row_count > 0 {
            let suffix = if truncated {
                " (first 1,000 rows shown per result set)"
            } else {
                ""
            };
            format!("{row_count} rows returned{suffix}")
        } else {
            format!("{} statement(s) completed", self.command_count)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{env, time::Instant};

    use super::{Event, PostgresSession, QueryOutput, validate_read_only_sql};
    use crate::connection::{ConnectionDraft, ConnectionProfile};

    #[test]
    fn read_only_sql_accepts_a_single_read_statement() {
        assert!(validate_read_only_sql("SELECT current_database();").is_ok());
        assert!(
            validate_read_only_sql("-- report query\nWITH rows AS (SELECT 1) SELECT * FROM rows")
                .is_ok()
        );
    }

    #[test]
    fn read_only_sql_rejects_transaction_control_and_multiple_statements() {
        assert!(validate_read_only_sql("COMMIT").is_err());
        assert!(validate_read_only_sql("/* bypass */ BEGIN").is_err());
        assert!(validate_read_only_sql("/* outer /* inner */ */ COMMIT").is_err());
        assert!(validate_read_only_sql("SELECT 1; COMMIT").is_err());
    }

    #[test]
    #[ignore = "requires SQL_MANAGER_E2E_DATABASE_URL and a local PostgreSQL server"]
    fn postgres_session_executes_and_bounds_a_large_result_set() {
        let mut draft = ConnectionDraft {
            connection_url: env::var("SQL_MANAGER_E2E_DATABASE_URL")
                .expect("set SQL_MANAGER_E2E_DATABASE_URL"),
            ..ConnectionDraft::default()
        };
        draft
            .apply_connection_url()
            .expect("valid PostgreSQL test URL");
        let password = draft.password.clone();
        let profile = draft.to_profile(None).expect("valid connection profile");

        let (server_sleep_output, server_sleep_elapsed) =
            execute_e2e_query(profile.clone(), &password, "SELECT pg_sleep(0.2), 1");
        assert!(server_sleep_elapsed >= std::time::Duration::from_millis(200));
        assert_eq!(server_sleep_output.result_sets[0].rows.len(), 1);

        let session = connect_e2e_session(profile.clone(), &password);
        session
            .execute(String::from(
                "SELECT i, pg_sleep(0.001) FROM generate_series(1, 10000) i",
            ))
            .expect("submit streamed-result query");
        let started = Instant::now();
        let progress = loop {
            match session
                .events
                .recv_timeout(std::time::Duration::from_secs(8))
                .expect("first result batch should arrive while query is running")
            {
                Event::QueryProgress(output) => break output,
                Event::Query(result) => {
                    let _ = result.expect("query should succeed");
                    panic!("query finished without publishing a partial result batch");
                }
                Event::Disconnected(error) => panic!("database session disconnected: {error}"),
                _ => {}
            }
        };
        let first_batch_elapsed = started.elapsed();
        assert_bounded_result(&progress);

        let normal_output = loop {
            match session
                .events
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("streamed query should finish")
            {
                Event::Query(result) => break result.expect("query should succeed"),
                Event::Disconnected(error) => panic!("database session disconnected: {error}"),
                _ => {}
            }
        };
        assert_bounded_result(&normal_output);

        let (normal_output, normal_elapsed) = execute_e2e_query(
            profile.clone(),
            &password,
            "SELECT generate_series(1, 1000000)",
        );
        assert_bounded_result(&normal_output);

        let mut read_only_profile = profile.clone();
        read_only_profile.read_only = true;
        let (read_only_output, read_only_elapsed) = execute_e2e_query(
            read_only_profile.clone(),
            &password,
            "SELECT generate_series(1, 1000000)",
        );
        assert_bounded_result(&read_only_output);

        let table_name = format!("e2e_{}", uuid::Uuid::new_v4().simple());
        let normal_session = connect_e2e_session(profile, &password);
        run_e2e_session_query(
            &normal_session,
            &format!("CREATE TABLE {table_name} (id integer)"),
        )
        .expect("create read-only E2E fixture");

        let read_only_session = connect_e2e_session(read_only_profile, &password);
        let write_error = match run_e2e_session_query(
            &read_only_session,
            &format!("INSERT INTO {table_name} VALUES (1)"),
        ) {
            Ok(_) => panic!("read-only transaction unexpectedly accepted a write"),
            Err(error) => error,
        };
        assert!(
            write_error.to_lowercase().contains("read-only"),
            "expected read-only rejection, received: {write_error}"
        );
        let (row_count, _) = execute_e2e_query(
            draft.to_profile(None).expect("valid connection profile"),
            &password,
            &format!("SELECT count(*) FROM {table_name}"),
        );
        assert_eq!(row_count.result_sets[0].rows[0][0].as_deref(), Some("0"));
        run_e2e_session_query(&normal_session, &format!("DROP TABLE {table_name}"))
            .expect("clean up read-only E2E fixture");

        eprintln!(
            "PostgreSQL E2E timings — server sleep: {server_sleep_elapsed:?}, first 1,000 rows: {first_batch_elapsed:?}, normal 1M rows: {normal_elapsed:?}, read-only 1M rows: {read_only_elapsed:?}"
        );
    }

    fn execute_e2e_query(
        profile: ConnectionProfile,
        password: &str,
        sql: &str,
    ) -> (QueryOutput, std::time::Duration) {
        let session = connect_e2e_session(profile, password);

        let started = Instant::now();
        let output = run_e2e_session_query(&session, sql).expect("query should succeed");
        (output, started.elapsed())
    }

    fn run_e2e_session_query(session: &PostgresSession, sql: &str) -> Result<QueryOutput, String> {
        session.execute(sql.to_owned()).expect("submit E2E query");
        loop {
            match session
                .events
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("query should finish")
            {
                Event::Query(result) => break result,
                Event::Disconnected(error) => panic!("database session disconnected: {error}"),
                _ => {}
            }
        }
    }

    fn connect_e2e_session(profile: ConnectionProfile, password: &str) -> PostgresSession {
        let session = PostgresSession::connect(profile, password.to_owned());
        loop {
            match session
                .events
                .recv_timeout(std::time::Duration::from_secs(15))
                .expect("database session should connect")
            {
                Event::Connected => break,
                Event::Disconnected(error) => panic!("database session disconnected: {error}"),
                _ => {}
            }
        }
        session
    }

    fn assert_bounded_result(output: &QueryOutput) {
        assert_eq!(output.result_sets.len(), 1);
        assert_eq!(output.result_sets[0].rows.len(), 1_000);
        assert!(output.result_sets[0].truncated);
    }
}
