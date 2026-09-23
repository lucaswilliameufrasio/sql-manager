use std::{
    future::Future,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use futures_util::TryStreamExt;
use tokio::runtime::Runtime;
use tokio_postgres::{Client, Config, SimpleQueryMessage, config::SslMode};
use tokio_postgres_rustls::MakeRustlsConnect;
use zeroize::Zeroize;

use crate::connection::{ConnectionProfile, TlsMode};

const MAX_RESULT_ROWS: usize = 1_000;

pub struct DatabaseSession {
    commands: Sender<Command>,
    pub events: Receiver<Event>,
}

pub enum Event {
    Connected,
    Schemas(Vec<String>),
    Tables { schema: String, tables: Vec<String> },
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
            let tls_disabled = profile.tls_mode == TlsMode::Disable;
            if tls_disabled {
                let result = runtime.block_on(config.connect(tokio_postgres::NoTls));
                password.zeroize();
                match result {
                    Ok((client, connection)) => {
                        run_session(runtime, client, connection, command_receiver, event_sender)
                    }
                    Err(error) => {
                        let _ = event_sender.send(Event::Disconnected(error.to_string()));
                    }
                }
            } else {
                let _ = rustls::crypto::ring::default_provider().install_default();
                let result =
                    runtime.block_on(config.connect(MakeRustlsConnect::with_webpki_roots()));
                password.zeroize();
                match result {
                    Ok((client, connection)) => {
                        run_session(runtime, client, connection, command_receiver, event_sender)
                    }
                    Err(error) => {
                        let _ = event_sender.send(Event::Disconnected(error.to_string()));
                    }
                }
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
                Command::Query(sql) => {
                    let result = execute_query(&client, &sql).await;
                    let _ = events.send(Event::Query(result));
                }
            }
        }
    });
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
