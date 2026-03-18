use lazysql::LazyConnection;
use std::sync::{Arc, mpsc};

pub enum DbCommand {
    Query(String),
    Execute(String),
    LoadSchema,
    Shutdown,
}

pub enum DbResponse {
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        elapsed: std::time::Duration,
    },
    RowsAffected(u64, std::time::Duration),
    Schema(Vec<TableSchema>),
    Error(String),
}

pub struct ColumnInfo {
    pub name: String,
    pub typ: String,
    pub pk: bool,
    pub fk_to: Option<String>, // "other_table.col"
}

pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
}

pub fn db_thread(
    conn: Arc<LazyConnection>,
    rx: mpsc::Receiver<DbCommand>,
    tx: mpsc::Sender<DbResponse>,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            DbCommand::Shutdown => break,

            DbCommand::LoadSchema => {
                let tables = match conn.query_dynamic(
                    "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
                ) {
                    Ok(r) => r
                        .filter_map(|row| row.ok())
                        .filter_map(|row| row.into_iter().next().map(|c| c.as_string()))
                        .collect::<Vec<_>>(),
                    Err(e) => {
                        tx.send(DbResponse::Error(e.to_string())).ok();
                        continue;
                    }
                };

                let mut schema = vec![];
                for table in &tables {
                    let fk_sql = format!("PRAGMA foreign_key_list({})", table);
                    let mut fk_map = std::collections::HashMap::new();
                    if let Ok(rows) = conn.query_dynamic(&fk_sql) {
                        for row in rows.filter_map(|r| r.ok()) {
                            let cells: Vec<String> = row.iter().map(|c| c.as_string()).collect();
                            if cells.len() >= 5 {
                                fk_map
                                    .insert(cells[3].clone(), format!("{}.{}", cells[2], cells[4]));
                            }
                        }
                    }

                    let col_sql = format!("PRAGMA table_info({})", table);
                    let mut columns = vec![];
                    if let Ok(rows) = conn.query_dynamic(&col_sql) {
                        for row in rows.filter_map(|r| r.ok()) {
                            let cells: Vec<String> = row.iter().map(|c| c.as_string()).collect();
                            if cells.len() >= 6 {
                                let name = cells[1].clone();
                                let typ = cells[2].clone();
                                let pk = cells[5] != "0";
                                let fk_to = fk_map.get(&name).cloned();
                                columns.push(ColumnInfo {
                                    name,
                                    typ,
                                    pk,
                                    fk_to,
                                });
                            }
                        }
                    }
                    schema.push(TableSchema {
                        name: table.clone(),
                        columns,
                    });
                }
                tx.send(DbResponse::Schema(schema)).ok();
            }

            DbCommand::Query(sql) => {
                let start = std::time::Instant::now();
                let response = match conn.query_dynamic(&sql) {
                    Ok(dynamic_rows) => {
                        let columns = dynamic_rows.column_names.clone();
                        let rows = dynamic_rows
                            .map(|r| match r {
                                Ok(row) => row.iter().map(|c| c.as_string()).collect(),
                                Err(e) => vec![e.to_string()],
                            })
                            .collect();
                        DbResponse::Rows {
                            columns,
                            rows,
                            elapsed: start.elapsed(),
                        }
                    }
                    Err(e) => DbResponse::Error(e.to_string()),
                };
                tx.send(response).ok();
            }

            DbCommand::Execute(sql) => {
                let start = std::time::Instant::now();
                let response = match conn.execute_dynamic(&sql) {
                    Ok(n) => DbResponse::RowsAffected(n, start.elapsed()),
                    Err(e) => DbResponse::Error(e.to_string()),
                };
                tx.send(response).ok();
            }
        }
    }
}

pub fn is_query(sql: &str) -> bool {
    let t = sql.trim().to_uppercase();
    t.starts_with("SELECT")
        || t.starts_with("WITH")
        || t.starts_with("PRAGMA")
        || t.starts_with("EXPLAIN")
}
