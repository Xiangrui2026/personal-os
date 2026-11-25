use crate::filesystem::Layout;
use crate::model::{ChangeError, ChangePlan, ChangeRecord, conflict, hash};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const APPLICATION_ID: i64 = 0x4d414747;
const FORMAT: i64 = 1;

const SCHEMA: &str = "
    CREATE TABLE vault_meta (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        vault_id TEXT NOT NULL
    );
    CREATE TABLE content_blobs (
        hash TEXT PRIMARY KEY,
        bytes BLOB NOT NULL
    );
    CREATE TABLE executions (
        id TEXT PRIMARY KEY,
        path TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('pending', 'completed', 'not_applied')),
        before_hash TEXT NOT NULL REFERENCES content_blobs(hash),
        after_hash TEXT NOT NULL REFERENCES content_blobs(hash),
        plan TEXT NOT NULL
    );
    CREATE INDEX pending_path ON executions(path) WHERE state = 'pending';
    CREATE TABLE changes (
        seq INTEGER PRIMARY KEY,
        id TEXT UNIQUE NOT NULL REFERENCES executions(id),
        path TEXT NOT NULL,
        completed_ms INTEGER NOT NULL,
        reverts TEXT UNIQUE REFERENCES changes(id)
    );
    CREATE INDEX change_path ON changes(path, seq);
    CREATE TABLE change_dependencies (
        change_id TEXT NOT NULL REFERENCES changes(id),
        dependency TEXT NOT NULL REFERENCES changes(id),
        PRIMARY KEY (change_id, dependency)
    );
    CREATE INDEX change_dependent ON change_dependencies(dependency);";

pub(crate) struct Store {
    db: Connection,
    pub vault_id: Uuid,
}

impl Store {
    pub fn initialize(layout: &Layout) -> Result<Self, ChangeError> {
        if layout.has_database()? {
            let db = connect(layout, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
            if db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))? != 0
                || db.pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))? != 0
                || db.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| {
                    row.get::<_, i64>(0)
                })? != 0
            {
                return Self::checked(db);
            }
        }
        let mut db = connect(
            layout,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        let vault_id = Uuid::new_v4();
        let transaction = db.transaction()?;
        transaction.execute_batch(SCHEMA)?;
        transaction.execute(
            "INSERT INTO vault_meta VALUES (1, ?1)",
            [vault_id.to_string()],
        )?;
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        transaction.pragma_update(None, "user_version", FORMAT)?;
        transaction.commit()?;
        Ok(Self { db, vault_id })
    }

    pub fn open(layout: &Layout, write: bool) -> Result<Option<Self>, ChangeError> {
        if !layout.has_database()? {
            return Ok(None);
        }
        let flags = if write {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        Self::checked(connect(layout, flags)?).map(Some)
    }

    fn checked(db: Connection) -> Result<Self, ChangeError> {
        let format = db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
        let application =
            db.pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))?;
        if format != FORMAT || application != APPLICATION_ID {
            return Err(ChangeError::StoreFormat(format));
        }
        let id: String = db.query_row(
            "SELECT vault_id FROM vault_meta WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        let vault_id =
            Uuid::parse_str(&id).map_err(|error| ChangeError::Corrupt(error.to_string()))?;
        Ok(Self { db, vault_id })
    }

    pub fn records(&self) -> Result<Vec<ChangeRecord>, ChangeError> {
        let mut statement = self.db.prepare(
            "SELECT c.seq, c.completed_ms, e.plan FROM changes c JOIN executions e ON e.id = c.id ORDER BY c.seq DESC",
        )?;
        let rows = statement.query_map([], record_row)?;
        rows.map(|row| self.decode_record(row?)).collect()
    }

    pub fn record(&self, id: Uuid) -> Result<Option<ChangeRecord>, ChangeError> {
        let row = self.db.query_row(
            "SELECT c.seq, c.completed_ms, e.plan FROM changes c JOIN executions e ON e.id = c.id WHERE c.id = ?1",
            [id.to_string()],
            record_row,
        ).optional()?;
        row.map(|row| self.decode_record(row)).transpose()
    }

    fn decode_record(
        &self,
        (seq, executed_unix_ms, plan): (i64, i64, String),
    ) -> Result<ChangeRecord, ChangeError> {
        Ok(ChangeRecord {
            seq,
            executed_unix_ms,
            vault_id: self.vault_id,
            plan: serde_json::from_str(&plan)?,
        })
    }

    pub fn execution(&self, id: Uuid) -> Result<Option<(String, ChangePlan)>, ChangeError> {
        let row = self
            .db
            .query_row(
                "SELECT state, plan FROM executions WHERE id = ?1",
                [id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        row.map(|(state, plan)| Ok((state, serde_json::from_str(&plan)?)))
            .transpose()
    }

    pub fn pending(&self) -> Result<Vec<ChangePlan>, ChangeError> {
        let mut statement = self
            .db
            .prepare("SELECT plan FROM executions WHERE state = 'pending' ORDER BY rowid")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn pending_for(&self, path: &str) -> Result<Option<Uuid>, ChangeError> {
        let id = self
            .db
            .query_row(
                "SELECT id FROM executions WHERE state = 'pending' AND path = ?1 LIMIT 1",
                [path],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        id.map(|id| Uuid::parse_str(&id).map_err(|error| ChangeError::Corrupt(error.to_string())))
            .transpose()
    }

    pub fn active_for(&self, path: &str) -> Result<Vec<ChangePlan>, ChangeError> {
        let mut statement = self.db.prepare(
            "SELECT e.plan FROM changes c JOIN executions e ON e.id = c.id
             WHERE c.path = ?1 AND c.reverts IS NULL
               AND NOT EXISTS (SELECT 1 FROM changes r WHERE r.reverts = c.id)
             ORDER BY c.seq",
        )?;
        let rows = statement.query_map([path], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn check_revert(&self, id: Uuid) -> Result<(), ChangeError> {
        let id = id.to_string();
        let reverted: bool = self.db.query_row(
            "SELECT EXISTS (SELECT 1 FROM changes WHERE reverts = ?1)",
            [&id],
            |row| row.get(0),
        )?;
        if reverted {
            return Err(conflict("the change has already been reverted"));
        }
        let dependent = self
            .db
            .query_row(
                "SELECT d.change_id FROM change_dependencies d JOIN changes c ON c.id = d.change_id
             WHERE d.dependency = ?1 AND c.reverts IS NULL
               AND NOT EXISTS (SELECT 1 FROM changes r WHERE r.reverts = d.change_id)
             ORDER BY c.seq LIMIT 1",
                [&id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(dependent) = dependent {
            return Err(conflict(format!(
                "change {dependent} still depends on the original effect"
            )));
        }
        Ok(())
    }

    pub fn prepare(
        &mut self,
        plan: &ChangePlan,
        before: &[u8],
        after: &[u8],
    ) -> Result<(), ChangeError> {
        let transaction = self.db.transaction()?;
        for (digest, bytes) in [(&plan.before_hash, before), (&plan.after_hash, after)] {
            transaction.execute(
                "INSERT OR IGNORE INTO content_blobs VALUES (?1, ?2)",
                params![digest, bytes],
            )?;
            let stored: Vec<u8> = transaction.query_row(
                "SELECT bytes FROM content_blobs WHERE hash = ?1",
                [digest],
                |row| row.get(0),
            )?;
            if stored != bytes || hash(bytes) != *digest {
                return Err(ChangeError::Corrupt(format!("content blob {digest}")));
            }
        }
        transaction.execute(
            "INSERT INTO executions VALUES (?1, ?2, 'pending', ?3, ?4, ?5)",
            params![
                plan.request.change_id.to_string(),
                plan.request.path,
                plan.before_hash,
                plan.after_hash,
                serde_json::to_string(plan)?
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn finish(&mut self, plan: &ChangePlan) -> Result<ChangeRecord, ChangeError> {
        let id = plan.request.change_id.to_string();
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let time = i64::try_from(time).unwrap_or(i64::MAX);
        let transaction = self.db.transaction()?;
        if transaction.execute(
            "UPDATE executions SET state = 'completed' WHERE id = ?1 AND state = 'pending'",
            [&id],
        )? != 1
        {
            return Err(ChangeError::Corrupt(format!(
                "execution {id} was not pending"
            )));
        }
        transaction.execute(
            "INSERT INTO changes (id, path, completed_ms, reverts) VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                plan.request.path,
                time,
                plan.reverts.map(|id| id.to_string())
            ],
        )?;
        let seq = transaction.last_insert_rowid();
        for dependency in &plan.depends_on {
            transaction.execute(
                "INSERT INTO change_dependencies VALUES (?1, ?2)",
                params![id, dependency.to_string()],
            )?;
        }
        transaction.commit()?;
        Ok(ChangeRecord {
            seq,
            vault_id: self.vault_id,
            executed_unix_ms: time,
            plan: plan.clone(),
        })
    }

    pub fn mark_not_applied(&self, id: Uuid) -> Result<(), ChangeError> {
        if self.db.execute(
            "UPDATE executions SET state = 'not_applied' WHERE id = ?1 AND state = 'pending'",
            [id.to_string()],
        )? != 1
        {
            return Err(ChangeError::Corrupt(format!(
                "execution {id} was not pending"
            )));
        }
        Ok(())
    }

    pub fn blob(&self, digest: &str) -> Result<String, ChangeError> {
        let bytes: Vec<u8> = self.db.query_row(
            "SELECT bytes FROM content_blobs WHERE hash = ?1",
            [digest],
            |row| row.get(0),
        )?;
        if hash(&bytes) != digest {
            return Err(ChangeError::Corrupt(format!("content blob {digest}")));
        }
        String::from_utf8(bytes).map_err(|error| ChangeError::Corrupt(error.to_string()))
    }
}

fn connect(layout: &Layout, flags: OpenFlags) -> Result<Connection, ChangeError> {
    let db =
        Connection::open_with_flags(&layout.database, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.pragma_update(None, "foreign_keys", true)?;
    Ok(db)
}

fn record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, i64, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}
