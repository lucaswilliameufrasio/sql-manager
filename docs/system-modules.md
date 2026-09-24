# SQL Manager module boundaries

This document defines the UI/domain modules used to replace the current single
connection form plus appended workspace layout.

## Connection Manager

- **Entities:** saved connection profile, transient connection draft, connection-test state.
- **Primary intents:** list/select saved profiles; create, edit, delete, test, and connect to one profile.
- **Commands:** `CreateProfile`, `UpdateProfile`, `DeleteProfile`, `TestProfile`, `ConnectProfile`, `DisconnectSession`.
- **Queries:** `ListProfiles`, `LoadProfile`.
- **State:** `NoProfileSelected`, `EditingProfile`, `Testing`, `Connecting`, `Connected`, `Failed`.
- **Permissions:** local application user; database authorization remains enforced by PostgreSQL.
- **Dependencies:** profile storage, OS keyring, engine adapter, optional OpenSSH tunnel.
- **Failure modes:** invalid URL/fields, profile I/O, missing keyring secret, bad credentials, TLS rejection, SSH host-key/auth/forwarding failure.

The connection editor belongs on this screen or in a dedicated dialog. It must not
remain in the center of the active database workspace.

## Workspace and database sessions

- **Entities:** active database session, selected profile, selected database, SQL editor tab, query result.
- **Primary intents:** navigate databases, run SQL, inspect query results, inspect table data.
- **Commands:** `OpenDatabaseSession`, `CloseDatabaseSession`, `ExecuteSql`, `RefreshCatalog`.
- **Queries:** `ListDatabases`, `ListSchemas`, `ListTables`, `ListColumns`, `ExecuteSql`.
- **State:** multiple lazily-created sessions keyed by `(profile_id, database_name)`; active session and selected workspace tab are explicit.
- **Permissions:** session access matches the PostgreSQL role; inaccessible databases are visible but disabled in the navigator.
- **Dependencies:** engine adapter and PostgreSQL catalog queries.
- **Failure modes:** CONNECT privilege denied, database removed/unavailable, network/TLS/tunnel failure, query error, session disconnect.

PostgreSQL sessions are bound to one database. Selecting another database opens a
separate session rather than pretending an existing session can switch databases.

## Database Navigator

- **Entities:** database, schema, table, column, and their access/availability metadata.
- **Primary intent:** locate an object to inspect or open.
- **Commands:** `SelectDatabase`, `OpenDatabase`, `SelectSchema`, `SelectTable`, `ViewTableData`.
- **Queries:** database catalog, schema catalog, table catalog, column metadata.
- **State:** tree selection is distinct from the active SQL tab and from an open data tab.
- **Permissions:** list all databases requested by the operator; mark non-connectable entries as disabled rather than hiding them.
- **Failure modes:** restricted catalog access, stale object after DDL, inaccessible database, object removed by another session.

## SQL Workspace

- **Entities:** SQL tab, query text, run state, result sets, query error.
- **Primary intent:** author and execute SQL.
- **Commands:** `RunSql`, `StopSql`, `OpenSqlTab`, `CloseSqlTab`.
- **Queries:** result rows and command status.
- **State:** idle/running/succeeded/failed/cancelled, with per-tab results.
- **Permissions:** active profile's read-only policy is applied to execution; PostgreSQL remains authoritative.
- **Failure modes:** invalid SQL, server cancellation, timeout/disconnect, oversized result.

## Table Data and Schema Operations

- **Entities:** paged table rows, column metadata, primary key, pending row/schema edit.
- **Primary intents:** inspect rows; explicitly open a table's **View Data** tab; create/update/delete rows; inspect or modify table/column schema.
- **Commands:** `LoadTablePage`, `InsertRow`, `UpdateRow`, `DeleteRow`, `CreateTable`, `RenameTable`, `DropTable`, `AddColumn`, `RenameColumn`, `DropColumn`.
- **State:** page offset, loading, edit dialog, destructive-operation confirmation, success/error status.
- **Permissions:** row update/delete requires a primary key; read-only mode disables writes and DDL actions. Destructive DDL remains separately confirmed and uses RESTRICT.
- **Failure modes:** missing primary key, stale row, constraint/type error, permission rejection, concurrent schema change.

## Read-only policy

Read-only is a saved-profile setting copied to every session opened from that
profile. It hides/disables visual row and schema mutations and executes SQL in a
PostgreSQL read-only transaction. This is a client safety mode, not an isolation
boundary against a superuser; a PostgreSQL role without write privileges is needed
for server-enforced read-only access.
