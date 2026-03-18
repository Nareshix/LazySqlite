use lazysql::LazyConnection;
use std::sync::Arc;
use std::sync::mpsc;

pub enum DbCommand {
    Query(String),
    Execute(String),
    GetTables,
    Shutdown,
}

pub enum DbResponse {
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        elapsed: std::time::Duration,
    },
    RowsAffected(u64, std::time::Duration),
    Tables(Vec<String>),
    Error(String),
}

pub fn db_thread(
    conn: Arc<LazyConnection>,
    rx: mpsc::Receiver<DbCommand>,
    tx: mpsc::Sender<DbResponse>,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            DbCommand::Shutdown => break,

            DbCommand::GetTables => {
                let sql = "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name";
                let response = match conn.query_dynamic(sql) {
                    Ok(dynamic_rows) => {
                        let names = dynamic_rows
                            .filter_map(|r| r.ok())
                            .filter_map(|row| row.into_iter().next().map(|c| c.as_string()))
                            .collect();
                        DbResponse::Tables(names)
                    }
                    Err(e) => DbResponse::Error(e.to_string()),
                };
                tx.send(response).ok();
            }

            DbCommand::Query(sql) => {
                let start = std::time::Instant::now();
                let result = conn.query_dynamic(&sql);

                let response = match result {
                    Ok(dynamic_rows) => {
                        let columns = dynamic_rows.column_names.clone();
                        let rows = dynamic_rows
                            .map(|row_result| match row_result {
                                Ok(row) => row.iter().map(|cell| cell.as_string()).collect(),
                                Err(e) => vec![e.to_string()],
                            })
                            .collect::<Vec<Vec<String>>>();
                        let elapsed = start.elapsed();
                        DbResponse::Rows { columns, rows, elapsed }
                    }
                    Err(e) => DbResponse::Error(e.to_string()),
                };
                tx.send(response).ok();
            }

            DbCommand::Execute(sql) => {
                let start = std::time::Instant::now();
                let result = conn.execute_dynamic(&sql);
                let elapsed = start.elapsed();

                let response = match result {
                    Ok(n)  => DbResponse::RowsAffected(n, elapsed),
                    Err(e) => DbResponse::Error(e.to_string()),
                };
                tx.send(response).ok();
            }
        }
    }
}

pub fn is_query(sql: &str) -> bool {
    let trimmed = sql.trim().to_uppercase();
    trimmed.starts_with("SELECT")
        || trimmed.starts_with("WITH")
        || trimmed.starts_with("PRAGMA")
        || trimmed.starts_with("EXPLAIN")
}