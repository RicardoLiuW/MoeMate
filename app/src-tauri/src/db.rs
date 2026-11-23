//! SQLite 持久化（`rusqlite` bundled）。
//!
//! DB 路径：`%APPDATA%\Deskpet\deskpet.db`；可用 `DESKPET_DB` 覆盖（测试/便携模式）。
//! 表：
//! - `pet_state`：当前角色、四维、坐标、缩放、置顶、静音、最后结算时间
//! - `settings`：通用 kv（`uiInteractive` 之外的可持久化设置）
//! - `play_history`：`(char_id, clip_id, played_at)`，用于 2h 去重

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{PetPos, PetState, PetStats};

const HISTORY_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct StoredState {
    pub char_id: Option<String>,
    pub stats: PetStats,
    pub pos: PetPos,
    pub scale: f64,
    pub pinned: bool,
    pub muted: bool,
    pub updated_at_ms: i64,
}

impl StoredState {
    pub fn to_pet_state(&self) -> PetState {
        PetState {
            stats: self.stats.clone(),
            pos: self.pos.clone(),
            scale: self.scale,
            pinned: self.pinned,
            muted: self.muted,
        }
    }
}

pub struct Db {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl Db {
    /// `%APPDATA%\Deskpet\deskpet.db`，优先 `DESKPET_DB`。
    pub fn default_path() -> PathBuf {
        if let Ok(custom) = std::env::var("DESKPET_DB") {
            if !custom.trim().is_empty() {
                return PathBuf::from(custom);
            }
        }
        let base = std::env::var("APPDATA")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("USERPROFILE")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .map(|home| PathBuf::from(home).join("AppData").join("Roaming"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("Deskpet").join("deskpet.db")
    }

    pub fn open_default() -> Result<Self, String> {
        let path = Self::default_path();
        Self::open(&path)
    }

    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| format!("创建数据库目录失败 {}: {err}", parent.display()))?;
            }
        }
        let conn = Connection::open(path)
            .map_err(|err| format!("打开数据库失败 {}: {err}", path.display()))?;
        Self::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|err| format!("内存数据库失败: {err}"))?;
        Self::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: PathBuf::from(":memory:"),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn init(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            CREATE TABLE IF NOT EXISTS pet_state (
                id          INTEGER PRIMARY KEY CHECK (id = 1),
                char_id     TEXT,
                satiety     REAL NOT NULL,
                energy      REAL NOT NULL,
                mood        REAL NOT NULL,
                affinity    REAL NOT NULL,
                pos_x       REAL NOT NULL,
                pos_y       REAL NOT NULL,
                scale       REAL NOT NULL,
                pinned      INTEGER NOT NULL,
                muted       INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS play_history (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                char_id   TEXT NOT NULL,
                clip_id   TEXT NOT NULL,
                played_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_play_history_char_clip
                ON play_history(char_id, clip_id, played_at);
            "#,
        )
        .map_err(|err| format!("初始化数据库表失败: {err}"))?;

        let now = crate::stats::now_ms();
        conn.execute(
            r#"INSERT OR IGNORE INTO pet_state
               (id, char_id, satiety, energy, mood, affinity, pos_x, pos_y,
                scale, pinned, muted, updated_at)
               VALUES (1, NULL, 80, 80, 80, 0, 0, 0, 0.5, 1, 0, ?1)"#,
            params![now],
        )
        .map_err(|err| format!("写入默认宠物状态失败: {err}"))?;
        Ok(())
    }

    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.conn
            .lock()
            .map_err(|_| "数据库连接锁已损坏".to_string())
    }

    pub fn load_state(&self) -> Result<StoredState, String> {
        let conn = self.conn()?;
        let mut statement = conn
            .prepare(
                r#"SELECT char_id, satiety, energy, mood, affinity, pos_x, pos_y,
                          scale, pinned, muted, updated_at
                   FROM pet_state WHERE id = 1"#,
            )
            .map_err(|err| format!("准备读取状态失败: {err}"))?;
        let row = statement
            .query_row([], |row| {
                Ok(StoredState {
                    char_id: row.get(0)?,
                    stats: PetStats {
                        satiety: row.get(1)?,
                        energy: row.get(2)?,
                        mood: row.get(3)?,
                        affinity: row.get(4)?,
                    },
                    pos: PetPos {
                        x: row.get(5)?,
                        y: row.get(6)?,
                    },
                    scale: row.get::<_, f64>(7)?.clamp(0.25, 2.0),
                    pinned: row.get::<_, i64>(8)? != 0,
                    muted: row.get::<_, i64>(9)? != 0,
                    updated_at_ms: row.get(10)?,
                })
            })
            .optional()
            .map_err(|err| format!("读取状态失败: {err}"))?;

        row.ok_or_else(|| "pet_state 缺少 id=1 行".to_string())
    }

    pub fn save_pet(
        &self,
        char_id: Option<&str>,
        pet: &PetState,
        updated_at_ms: i64,
    ) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            r#"UPDATE pet_state SET
                 char_id = ?1, satiety = ?2, energy = ?3, mood = ?4, affinity = ?5,
                 pos_x = ?6, pos_y = ?7, scale = ?8, pinned = ?9, muted = ?10,
                 updated_at = ?11
               WHERE id = 1"#,
            params![
                char_id,
                pet.stats.satiety,
                pet.stats.energy,
                pet.stats.mood,
                pet.stats.affinity,
                pet.pos.x,
                pet.pos.y,
                pet.scale,
                if pet.pinned { 1_i64 } else { 0_i64 },
                if pet.muted { 1_i64 } else { 0_i64 },
                updated_at_ms,
            ],
        )
        .map_err(|err| format!("保存宠物状态失败: {err}"))?;
        Ok(())
    }

    pub fn save_stats(&self, stats: &PetStats, updated_at_ms: i64) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE pet_state SET satiety=?1, energy=?2, mood=?3, affinity=?4, updated_at=?5 WHERE id=1",
            params![
                stats.satiety,
                stats.energy,
                stats.mood,
                stats.affinity,
                updated_at_ms
            ],
        )
        .map_err(|err| format!("保存数值失败: {err}"))?;
        Ok(())
    }

    pub fn save_char(&self, char_id: &str) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE pet_state SET char_id=?1 WHERE id=1",
            params![char_id],
        )
        .map_err(|err| format!("保存角色失败: {err}"))?;
        Ok(())
    }

    pub fn save_setting(&self, key: &str, value: &serde_json::Value) -> Result<(), String> {
        let text =
            serde_json::to_string(value).map_err(|err| format!("序列化设置失败 {key}: {err}"))?;
        let conn = self.conn()?;
        conn.execute(
            r#"INSERT INTO settings(key, value) VALUES(?1, ?2)
               ON CONFLICT(key) DO UPDATE SET value = excluded.value"#,
            params![key, text],
        )
        .map_err(|err| format!("保存设置失败 {key}: {err}"))?;
        Ok(())
    }

    pub fn load_settings(&self) -> Result<HashMap<String, serde_json::Value>, String> {
        let conn = self.conn()?;
        let mut statement = conn
            .prepare("SELECT key, value FROM settings")
            .map_err(|err| format!("准备读取设置失败: {err}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|err| format!("读取设置失败: {err}"))?;

        let mut settings = HashMap::new();
        for row in rows.flatten() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&row.1) {
                settings.insert(row.0, value);
            }
        }
        Ok(settings)
    }

    pub fn record_play(
        &self,
        char_id: &str,
        clip_id: &str,
        played_at_ms: i64,
    ) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO play_history(char_id, clip_id, played_at) VALUES(?1, ?2, ?3)",
            params![char_id, clip_id, played_at_ms],
        )
        .map_err(|err| format!("记录播放历史失败: {err}"))?;
        conn.execute(
            "DELETE FROM play_history WHERE played_at < ?1",
            params![played_at_ms - HISTORY_RETENTION_MS],
        )
        .map_err(|err| format!("清理播放历史失败: {err}"))?;
        Ok(())
    }

    /// 每个 clip 最近一次播放时间。
    pub fn load_history(
        &self,
        char_id: &str,
        since_ms: i64,
    ) -> Result<HashMap<String, i64>, String> {
        let conn = self.conn()?;
        let mut statement = conn
            .prepare(
                r#"SELECT clip_id, MAX(played_at)
                   FROM play_history
                   WHERE char_id = ?1 AND played_at >= ?2
                   GROUP BY clip_id"#,
            )
            .map_err(|err| format!("准备读取播放历史失败: {err}"))?;
        let rows = statement
            .query_map(params![char_id, since_ms], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|err| format!("读取播放历史失败: {err}"))?;
        Ok(rows.flatten().collect())
    }

    pub fn prune_history(&self, before_ms: i64) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM play_history WHERE played_at < ?1",
            params![before_ms],
        )
        .map_err(|err| format!("清理播放历史失败: {err}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PetStats;

    #[test]
    fn state_and_settings_roundtrip() {
        let db = Db::open_in_memory().expect("open memory db");
        let stored = db.load_state().expect("default state");
        assert!((stored.stats.satiety - 80.0).abs() < 1e-9);

        let pet = PetState {
            stats: PetStats {
                satiety: 42.0,
                energy: 55.0,
                mood: 66.0,
                affinity: 7.0,
            },
            pos: PetPos { x: 120.0, y: 340.0 },
            scale: 0.75,
            pinned: true,
            muted: true,
        };
        db.save_pet(Some("claude"), &pet, 1234).expect("save");
        let loaded = db.load_state().expect("reload");
        assert_eq!(loaded.char_id.as_deref(), Some("claude"));
        assert!((loaded.stats.affinity - 7.0).abs() < 1e-9);
        assert!((loaded.pos.x - 120.0).abs() < 1e-9);
        assert!(loaded.pinned && loaded.muted);

        db.save_setting("bubbleTtl", &serde_json::json!(4200))
            .expect("setting");
        let settings = db.load_settings().expect("settings");
        assert_eq!(
            settings.get("bubbleTtl").and_then(|v| v.as_i64()),
            Some(4200)
        );
    }

    #[test]
    fn play_history_keeps_latest_per_clip() {
        let db = Db::open_in_memory().expect("open memory db");
        db.record_play("claude", "a", 1_000).expect("play a");
        db.record_play("claude", "a", 2_000).expect("play a again");
        db.record_play("claude", "b", 1_500).expect("play b");
        let history = db.load_history("claude", 0).expect("history");
        assert_eq!(history.get("a"), Some(&2_000));
        assert_eq!(history.get("b"), Some(&1_500));
    }
}
