// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! Synthetic foreign files for the wizard's tests and screenshot scenes (example data only; the
//! passwords are fake and the importers never read them).

/// A Beekeeper Studio export: three supported engines (one of them SQL Server with an SSH tunnel)
/// and two skipped ones.
pub const BEEKEEPER: &str = r#"{
  "connection_folders": [
    { "id": 1, "name": "Shop" },
    { "id": 2, "name": "Staging", "parentId": 1 }
  ],
  "connections": [
    {
      "id": 1,
      "connectionType": "postgresql",
      "name": "Orders primary",
      "host": "db.example.test",
      "port": 5433,
      "defaultDatabase": "orders",
      "username": "alice",
      "password": "fake-pw-123",
      "ssl": true,
      "connectionFolderId": 2,
      "labelColor": "blue"
    },
    {
      "id": 2,
      "connectionType": "mariadb",
      "name": "Reports",
      "host": "maria.example.test",
      "port": "3307",
      "defaultDatabase": "reports",
      "username": "bob",
      "password": "fake-pw-123",
      "connectionFolderId": 1
    },
    {
      "id": 3,
      "connectionType": "sqlserver",
      "name": "Ledger",
      "host": "mssql.example.test",
      "defaultDatabase": "ledger",
      "username": "sa_alice",
      "password": "fake-pw-123",
      "sshEnabled": true,
      "sshHost": "bastion.example.test",
      "sshUsername": "tunnel",
      "sshMode": "userpass",
      "sshPassword": "fake-ssh-pw-456"
    },
    {
      "id": 4,
      "connectionType": "duckdb",
      "name": "Analytics file",
      "defaultDatabase": "analytics.duckdb"
    },
    { "id": 5, "connectionType": "mongodb", "name": "Docs", "host": "mongo.example.test" }
  ]
}
"#;

/// An Azure Data Studio `settings.json` with two SQL Server logins and a `PostgreSQL` one.
pub const AZURE_DATA_STUDIO: &str = r#"// Fictional Azure Data Studio user settings (example data only).
{
  "datasource.connectionGroups": [
    { "name": "ROOT", "id": "g-root" },
    { "name": "Shop", "id": "g-shop", "parentId": "g-root" }
  ],
  "datasource.connections": [
    {
      "options": {
        "connectionName": "Orders primary",
        "server": "db.example.test,14330",
        "database": "orders",
        "authenticationType": "SqlLogin",
        "user": "alice",
        "encrypt": "Mandatory",
        "groupId": "g-shop"
      },
      "groupId": "g-shop",
      "providerName": "MSSQL",
      "id": "c-1"
    },
    {
      "options": {
        "connectionName": "Reporting (PostgreSQL)",
        "server": "pg.example.test",
        "port": "5433",
        "database": "reports",
        "authenticationType": "SqlLogin",
        "user": "carol",
        "groupId": "g-root"
      },
      "groupId": "g-root",
      "providerName": "PGSQL",
      "id": "c-2"
    }
  ]
}
"#;
