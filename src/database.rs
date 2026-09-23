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

pub struct DatabaseSession {
    commands: Sender<Command>,
    pub events: Receiver<Event>,
}

pub enum Event {
    Connected,
    Schemas(Vec<String>),
    Tables { schema: String, tables: Vec<String> },
    TableData(Result<TableData, String>),
    Query(Result<QueryOutput, String>),
    Disconnected(String),
}

pub struct QueryOutput {
    pub result_sets: Vec<ResultSet>,
    pub summary: String,
}

pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub truncated: bool,
}

enum Command {
    ListTables(String),
    LoadTable {
        schema: String,
        table: String,
        offset: u64,
    },
    Query(String),
}

impl DatabaseSession {
    pub fn connect(profile: ConnectionProfile, mut password: String) -> Self {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();

        thread::spawn(move || {
            let runtime = match Runtime::new() {
                Ok(runtime) => runtime,
                Err(error) => {
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
        }
    }

    pub fn list_tables(&self, schema: String) -> Result<(), String> {
        self.commands
            .send(Command::ListTables(schema))
            .map_err(|error| error.to_string())
    }

    pub fn execute(&self, sql: String) -> Result<(), String> {
        self.commands
            .send(Command::Query(sql))
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
    client: Client,
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
                Command::Query(sql) => {
                    let result = execute_query(&client, &sql).await;
                    let _ = events.send(Event::Query(result));
                }
            }
        }
    });
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

async fn execute_query(client: &Client, sql: &str) -> Result<QueryOutput, String> {
    let stream = client
        .simple_query_raw(sql)
        .await
        .map_err(|error| error.to_string())?;
    let mut stream = std::pin::pin!(stream);
    let mut result_sets = Vec::<ResultSet>::new();
    let mut command_count = 0_u64;

    while let Some(message) = stream
        .as_mut()
        .try_next()
        .await
        .map_err(|error| error.to_string())?
    {
        match message {
            SimpleQueryMessage::RowDescription(columns) => {
                result_sets.push(ResultSet {
                    columns: columns
                        .iter()
                        .map(|column| column.name().to_owned())
                        .collect(),
                    rows: Vec::new(),
                    truncated: false,
                });
            }
            SimpleQueryMessage::Row(row) => {
                if result_sets.is_empty() {
                    result_sets.push(ResultSet {
                        columns: row
                            .columns()
                            .iter()
                            .map(|column| column.name().to_owned())
                            .collect(),
                        rows: Vec::new(),
                        truncated: false,
                    });
                }
                if let Some(result_set) = result_sets.last_mut() {
                    if result_set.rows.len() < MAX_RESULT_ROWS {
                        result_set.rows.push(
                            (0..row.len())
                                .map(|index| row.get(index).map(str::to_owned))
                                .collect(),
                        );
                    } else {
                        result_set.truncated = true;
                    }
                }
            }
            SimpleQueryMessage::CommandComplete(_) => command_count += 1,
            _ => {}
        }
    }

    let row_count = result_sets.iter().map(|set| set.rows.len()).sum::<usize>();
    let truncated = result_sets.iter().any(|set| set.truncated);
    let summary = if row_count > 0 {
        let suffix = if truncated {
            " (first 1,000 rows shown per result set)"
        } else {
            ""
        };
        format!("{row_count} rows returned{suffix}")
    } else {
        format!("{command_count} statement(s) completed")
    };

    Ok(QueryOutput {
        result_sets,
        summary,
    })
}
