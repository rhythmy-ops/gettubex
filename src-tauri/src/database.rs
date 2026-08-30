use crate::models::{DownloadTask, Platform};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use tauri::Manager;

/// 重试所需的任务字段（batch 信息随重试保留）
pub struct RetryData {
    pub url: String,
    pub title: String,
    pub format_id: String,
    pub batch_id: Option<String>,
    pub batch_name: Option<String>,
}

#[derive(Clone)]
pub struct Database {
    path: PathBuf,
}

const TASK_COLUMNS: &str =
    "id,url,title,platform,status,progress,speed,eta,output_path,error,batch_id,batch_name";

impl Database {
    pub fn new(app: &tauri::AppHandle) -> Result<Self, String> {
        let path = app
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?
            .join("gettube.sqlite3");
        let db = Self { path };
        db.initialize()?;
        Ok(db)
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Result<Self, String> {
        let db = Self { path };
        db.initialize()?;
        Ok(db)
    }

    fn connect(&self) -> Result<Connection, String> {
        Connection::open(&self.path).map_err(|error| format!("无法打开任务数据库：{error}"))
    }

    fn initialize(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        self.connect()?
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS tasks (
                    id TEXT PRIMARY KEY,
                    url TEXT NOT NULL,
                    title TEXT NOT NULL,
                    platform TEXT NOT NULL,
                    status TEXT NOT NULL,
                    progress REAL NOT NULL DEFAULT 0,
                    speed TEXT,
                    eta TEXT,
                    output_path TEXT,
                    error TEXT,
                    format_id TEXT NOT NULL,
                    batch_id TEXT,
                    batch_name TEXT,
                    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                );
                CREATE INDEX IF NOT EXISTS idx_tasks_created ON tasks(created_at DESC);",
            )
            .map_err(|error| format!("无法初始化任务数据库：{error}"))?;
        self.ensure_column("batch_id", "ALTER TABLE tasks ADD COLUMN batch_id TEXT")?;
        self.ensure_column("batch_name", "ALTER TABLE tasks ADD COLUMN batch_name TEXT")?;
        // v3 早期版本把批次展示名写进 batch_id；保留其文件夹语义供升级后重试使用。
        self.connect()?
            .execute(
                "UPDATE tasks SET batch_name=batch_id WHERE batch_id IS NOT NULL AND batch_name IS NULL",
                [],
            )
            .map_err(|error| format!("无法迁移批次名称：{error}"))?;
        // 上次退出仍在运行的任务标记失败；排队中的任务保留，由调度器在启动后继续执行
        self.connect()?.execute(
            "UPDATE tasks SET status='failed', error='应用上次退出时任务仍在运行，请重试', updated_at=unixepoch()
             WHERE status IN ('downloading','processing')",
            [],
        ).map_err(|error| format!("无法恢复中断任务：{error}"))?;
        Ok(())
    }

    fn ensure_column(&self, name: &str, migration: &str) -> Result<(), String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare("PRAGMA table_info(tasks)")
            .map_err(|error| format!("无法读取任务数据库结构：{error}"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| format!("无法读取任务数据库结构：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法读取任务数据库结构：{error}"))?;
        drop(statement);
        if !columns.iter().any(|column| column == name) {
            connection
                .execute_batch(migration)
                .map_err(|error| format!("无法升级任务数据库：{error}"))?;
        }
        Ok(())
    }

    pub fn insert(&self, task: &DownloadTask, format_id: &str) -> Result<(), String> {
        self.connect()?
            .execute(
                "INSERT INTO tasks (id,url,title,platform,status,progress,speed,eta,output_path,error,format_id,batch_id,batch_name)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![task.id, task.url, task.title, platform_str(task.platform), task.status,
                    task.progress, task.speed, task.eta, task.output_path, task.error, format_id, task.batch_id,
                    task.batch_name],
            )
            .map(|_| ())
            .map_err(|error| format!("无法保存任务：{error}"))
    }

    pub fn insert_many(&self, tasks: &[(DownloadTask, String)]) -> Result<(), String> {
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始批量任务事务：{error}"))?;
        for (task, format_id) in tasks {
            transaction
                .execute(
                    "INSERT INTO tasks (id,url,title,platform,status,progress,speed,eta,output_path,error,format_id,batch_id,batch_name)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                    params![task.id, task.url, task.title, platform_str(task.platform), task.status,
                        task.progress, task.speed, task.eta, task.output_path, task.error, format_id,
                        task.batch_id, task.batch_name],
                )
                .map_err(|error| format!("无法保存批量任务：{error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交批量任务：{error}"))
    }

    pub fn update_progress(
        &self,
        id: &str,
        status: &str,
        progress: f64,
        speed: Option<&str>,
        eta: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.connect()?.execute(
            "UPDATE tasks SET status=?2,progress=?3,speed=?4,eta=?5,error=?6,updated_at=unixepoch() WHERE id=?1",
            params![id, status, progress, speed, eta, error],
        ).map(|_| ()).map_err(|error| format!("无法更新任务：{error}"))
    }

    pub fn mark_cancelled(&self, id: &str) -> Result<(), String> {
        self.connect()?.execute(
            "UPDATE tasks SET status='cancelled', speed=NULL, eta=NULL, updated_at=unixepoch() WHERE id=?1",
            params![id],
        ).map(|_| ()).map_err(|error| format!("无法取消任务：{error}"))
    }

    pub fn set_output_path(&self, id: &str, path: &str) -> Result<(), String> {
        self.connect()?
            .execute(
                "UPDATE tasks SET output_path=?2, updated_at=unixepoch() WHERE id=?1",
                params![id, path],
            )
            .map(|_| ())
            .map_err(|error| format!("无法更新任务：{error}"))
    }

    pub fn list(&self) -> Result<Vec<DownloadTask>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks ORDER BY created_at DESC"
            ))
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], row_to_task)
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    /// 最早的排队任务在前，调度器按此顺序拉起
    pub fn list_queued(&self, limit: u32) -> Result<Vec<(DownloadTask, String)>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(&format!(
                "SELECT {TASK_COLUMNS},format_id FROM tasks WHERE status='queued' ORDER BY created_at ASC LIMIT ?1"
            ))
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![limit], |row| {
                Ok((row_to_task(row)?, row.get::<_, String>(12)?))
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    pub fn task_full(&self, id: &str) -> Result<(DownloadTask, String), String> {
        self.connect()?
            .query_row(
                &format!("SELECT {TASK_COLUMNS},format_id FROM tasks WHERE id=?1"),
                params![id],
                |row| Ok((row_to_task(row)?, row.get::<_, String>(12)?)),
            )
            .map_err(|_| "找不到任务".into())
    }

    pub fn list_by_batch(&self, batch_id: &str) -> Result<Vec<DownloadTask>, String> {
        let connection = self.connect()?;
        let mut statement = connection
            .prepare(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks WHERE batch_id=?1 ORDER BY created_at ASC"
            ))
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![batch_id], row_to_task)
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())
    }

    pub fn retry_data(&self, id: &str) -> Result<RetryData, String> {
        self.connect()?
            .query_row(
                "SELECT url,title,format_id,batch_id,batch_name FROM tasks WHERE id=?1",
                [id],
                |row| {
                    Ok(RetryData {
                        url: row.get(0)?,
                        title: row.get(1)?,
                        format_id: row.get(2)?,
                        batch_id: row.get(3)?,
                        batch_name: row.get(4)?,
                    })
                },
            )
            .map_err(|_| "找不到要重试的任务".into())
    }

    /// 按状态批量清除任务记录，返回删除条数；调用方保证只传终态
    pub fn clear_tasks(&self, statuses: &[&str]) -> Result<usize, String> {
        if statuses.is_empty() {
            return Ok(0);
        }
        let placeholders = statuses.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM tasks WHERE status IN ({placeholders})");
        self.connect()?
            .execute(sql.as_str(), rusqlite::params_from_iter(statuses.iter()))
            .map_err(|error| format!("无法清除任务：{error}"))
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<DownloadTask> {
    Ok(DownloadTask {
        id: row.get(0)?,
        url: row.get(1)?,
        title: row.get(2)?,
        platform: parse_platform(row.get::<_, String>(3)?),
        status: row.get(4)?,
        progress: row.get(5)?,
        speed: row.get(6)?,
        eta: row.get(7)?,
        output_path: row.get(8)?,
        error: row.get(9)?,
        batch_id: row.get(10)?,
        batch_name: row.get(11)?,
    })
}

fn platform_str(platform: Platform) -> &'static str {
    match platform {
        Platform::Youtube => "youtube",
        Platform::Bilibili => "bilibili",
        Platform::Douyin => "douyin",
        Platform::Twitter => "twitter",
    }
}

fn parse_platform(value: String) -> Platform {
    match value.as_str() {
        "youtube" => Platform::Youtube,
        "bilibili" => Platform::Bilibili,
        "douyin" => Platform::Douyin,
        _ => Platform::Twitter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_task(id: &str, status: &str) -> DownloadTask {
        DownloadTask {
            id: id.into(),
            url: "https://youtu.be/test".into(),
            title: "Test".into(),
            platform: Platform::Youtube,
            status: status.into(),
            progress: 0.0,
            speed: None,
            eta: None,
            output_path: None,
            error: None,
            batch_id: Some("批次".into()),
            batch_name: Some("播放列表".into()),
        }
    }

    fn temp_db() -> (Database, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("gettube-{}.sqlite3", uuid::Uuid::new_v4()));
        let db = Database::at(path.clone()).unwrap();
        (db, path)
    }

    #[test]
    fn persists_tasks_with_batch() {
        let (db, path) = temp_db();
        db.insert(&sample_task("task-1", "queued"), "best").unwrap();
        let listed = db.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].batch_id.as_deref(), Some("批次"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn queued_survives_reopen_but_running_fails() {
        let (db, path) = temp_db();
        db.insert(&sample_task("q1", "queued"), "best").unwrap();
        db.insert(&sample_task("r1", "downloading"), "best")
            .unwrap();
        drop(db);
        let db = Database::at(path.clone()).unwrap();
        let statuses: Vec<(String, String)> = db
            .list()
            .unwrap()
            .into_iter()
            .map(|task| (task.id, task.status))
            .collect();
        assert!(statuses.contains(&("q1".into(), "queued".into())));
        assert!(statuses.contains(&("r1".into(), "failed".into())));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn batch_insert_is_atomic() {
        let (db, path) = temp_db();
        let tasks = vec![
            (sample_task("duplicate", "queued"), "fmt-a".to_string()),
            (sample_task("duplicate", "queued"), "fmt-b".to_string()),
        ];
        assert!(db.insert_many(&tasks).is_err());
        assert!(db.list().unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn list_queued_returns_oldest_first_with_format() {
        let (db, path) = temp_db();
        let first = sample_task("older", "queued");
        db.insert(&first, "fmt-a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let second = sample_task("newer", "queued");
        db.insert(&second, "fmt-b").unwrap();
        db.insert(&sample_task("done", "completed"), "x").unwrap();
        let queued = db.list_queued(10).unwrap();
        assert_eq!(
            queued
                .iter()
                .map(|(task, _)| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["older", "newer"]
        );
        assert_eq!(queued[1].1, "fmt-b");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn clear_tasks_only_removes_given_statuses() {
        let (db, path) = temp_db();
        for (id, status) in [
            ("a", "completed"),
            ("b", "failed"),
            ("c", "cancelled"),
            ("d", "queued"),
            ("e", "downloading"),
        ] {
            db.insert(&sample_task(id, status), "best").unwrap();
        }
        let removed = db
            .clear_tasks(&["completed", "failed", "cancelled"])
            .unwrap();
        assert_eq!(removed, 3);
        let left: Vec<String> = db.list().unwrap().into_iter().map(|task| task.id).collect();
        assert_eq!(left.len(), 2);
        assert!(left.contains(&"d".into()) && left.contains(&"e".into()));
        let _ = std::fs::remove_file(path);
    }
}
