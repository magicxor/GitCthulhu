mod run {
    use std::time::{Duration, Instant};

    use but_db::{M, SchemaVersion, migration};

    use crate::migration::util::{dump_data, dump_schema};
    const ZERO: SchemaVersion = SchemaVersion::Zero;

    fn run_with_backoff<'a>(
        conn: &mut rusqlite::Connection,
        migrations: &[M<'a>],
    ) -> Result<usize, migration::Error> {
        let mut count = 0;
        let policy = backoff::ExponentialBackoffBuilder::new()
            .with_initial_interval(Duration::from_millis(10))
            .with_randomization_factor(0.0)
            .with_multiplier(1.0)
            .with_max_interval(Duration::from_millis(10))
            .with_max_elapsed_time(Some(Duration::from_secs(1)))
            .build();
        backoff::retry(policy, || {
            count = migration::run(conn, migrations.iter().copied())?;
            Ok::<_, migration::Error>(())
        })?;
        Ok(count)
    }

    #[test]
    fn all_or_nothing() -> anyhow::Result<()> {
        let mut db = rusqlite::Connection::open_in_memory()?;

        let (good, bad) = (
            M::up(0, ZERO, "CREATE TABLE T1 ( first TEXT PRIMARY KEY );"),
            M::up(1, ZERO, "bad"),
        );
        let err = migration::run(&mut db, [good, bad]).unwrap_err();
        assert!(matches!(err, backoff::Error::Permanent(_)));

        snapbox::assert_data_eq!(dump_schema(&db)?, snapbox::str![""]);
        snapbox::assert_data_eq!(dump_data(&db)?, snapbox::str![""]);
        Ok(())
    }

    #[test]
    fn read_only_until_it_needs_a_lock() -> anyhow::Result<()> {
        use rusqlite::TransactionBehavior;
        let tmp = tempfile::TempDir::new()?;
        let db_path = tmp.path().join("db");
        let mut db1 = rusqlite::Connection::open(&db_path)?;
        let mut db2 = rusqlite::Connection::open(&db_path)?;

        let migrations = [
            M::up(0, ZERO, "CREATE TABLE T1 ( first TEXT PRIMARY KEY );"),
            M::up(1, ZERO, "CREATE TABLE T2 ( first TEXT PRIMARY KEY );"),
        ];

        {
            let _blocking_trans = db1.transaction_with_behavior(TransactionBehavior::Immediate)?;

            let start = Instant::now();
            let busy_timeout = Duration::from_millis(100);
            db2.busy_timeout(busy_timeout)?;
            let err = migration::run(&mut db2, migrations).unwrap_err();
            let elapsed = start.elapsed();
            assert!(
                matches!(
                    err,
                    backoff::Error::Transient {
                        ref err,
                        ..
                    } if err.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
                ),
                "it wants to write, but can't, but knows it's a locking issue"
            );
            assert!(
                elapsed < busy_timeout / 2,
                "initial migration attempt should fail before busy_timeout when the deferred read snapshot cannot be upgraded: {elapsed:?}"
            );
        }

        let count = migration::run(&mut db2, migrations)?;
        assert_eq!(count, 2, "DB is unlocked and migrations are performed");

        {
            let _blocking_trans = db1.transaction_with_behavior(TransactionBehavior::Immediate)?;

            let count = migration::run(&mut db2, migrations)?;
            assert_eq!(count, 0, "It reads first which doesn't run into the lock");

            let count = migration::run(&mut db2, [migrations[0]])?;
            assert_eq!(
                count, 0,
                "older compatible migration lists also stay read-only despite extra migration rows in the DB"
            );
        }

        snapbox::assert_data_eq!(
            dump_schema(&db1)?,
            snapbox::str![[r#"
-- table T1
CREATE TABLE T1 ( first TEXT PRIMARY KEY );

-- table T2
CREATE TABLE T2 ( first TEXT PRIMARY KEY );

-- table __diesel_schema_migrations
CREATE TABLE __diesel_schema_migrations (
       version VARCHAR(50) PRIMARY KEY NOT NULL,
       run_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);


"#]]
        );
        snapbox::assert_data_eq!(
            dump_data(&db1)?,
            snapbox::str![[r#"
Table: __diesel_schema_migrations
version
Text("0")
Text("1")

Table: T1
first

Table: T2
first


"#]]
        );
        Ok(())
    }

    #[test]
    fn initialization_retries_lock_failures_with_backoff_in_threads() -> anyhow::Result<()> {
        use rusqlite::TransactionBehavior;

        let tmp = tempfile::TempDir::new()?;
        let db_path = tmp.path().join("db");
        let mut db = rusqlite::Connection::open(&db_path)?;
        let blocking_trans = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let hold_lock = Duration::from_millis(250);
        let busy_timeout = Duration::from_secs(1);

        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let thread = std::thread::spawn({
            move || -> anyhow::Result<Duration> {
                let mut db = rusqlite::Connection::open(&db_path)?;
                db.busy_timeout(busy_timeout)?;

                started_tx.send(())?;
                let start = Instant::now();
                let migrations = [M::up(
                    0,
                    ZERO,
                    "CREATE TABLE T1 ( first TEXT PRIMARY KEY );",
                )];
                let count = run_with_backoff(&mut db, &migrations)?;
                assert_eq!(count, 1, "migration succeeds once the lock is released");
                Ok(start.elapsed())
            }
        });

        started_rx
            .recv()
            .expect("worker starts before we release the lock");
        std::thread::sleep(hold_lock);
        // Release the DB lock.
        drop(blocking_trans);

        let elapsed = thread.join().expect("worker did not panic")?;
        let safety_margin = Duration::from_millis(75);
        assert!(
            elapsed >= hold_lock - safety_margin,
            "busy timeout should wait for lock release, elapsed={elapsed:?}"
        );
        Ok(())
    }

    #[test]
    fn journey() -> anyhow::Result<()> {
        let mut db = rusqlite::Connection::open_in_memory()?;

        let (old, new) = (
            M::up(0, ZERO, "CREATE TABLE T1 ( first VARCHAR(50) PRIMARY KEY )"),
            M::up(1, ZERO, "ALTER TABLE `T1` ADD COLUMN `two` TEXT"),
        );
        let incorrectly_ordered = [new, old];
        let count = migration::run(&mut db, incorrectly_ordered)?;
        assert_eq!(count, 2, "both migrations ran the first time");

        let count = migration::run(&mut db, incorrectly_ordered)?;
        assert_eq!(count, 0, "everything is up-to-date already");

        snapbox::assert_data_eq!(
            dump_schema(&db)?,
            snapbox::str![[r#"
-- table T1
CREATE TABLE T1 ( first VARCHAR(50) PRIMARY KEY , `two` TEXT);

-- table __diesel_schema_migrations
CREATE TABLE __diesel_schema_migrations (
       version VARCHAR(50) PRIMARY KEY NOT NULL,
       run_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);


"#]]
        );
        snapbox::assert_data_eq!(
            dump_data(&db)?,
            snapbox::str![[r#"
Table: __diesel_schema_migrations
version
Text("0")
Text("1")

Table: T1
first | two


"#]]
        );

        let count = migration::run(&mut db, [old])
            .expect("older migration list is accepted if DB is compatible");
        assert_eq!(
            count, 0,
            "extra migrations in the DB are tolerated when forward-compatibility allows it"
        );

        let newer_new = M::up(2, ZERO, "ALTER TABLE `T1` ADD COLUMN `two` TEXT");
        let err = migration::run(&mut db, [old, /* 'new' missing */ newer_new])
            .expect_err("cannot skip a migration");
        assert!(matches!(err, backoff::Error::Permanent(_)));
        Ok(())
    }

    #[test]
    fn rejects_forward_incompatible_database() -> anyhow::Result<()> {
        let mut db = rusqlite::Connection::open_in_memory()?;

        db.execute_batch(&format!(
            "PRAGMA user_version = {};",
            SchemaVersion::One as u32
        ))?;
        let err = migration::run(
            &mut db,
            [M::up(
                0,
                ZERO,
                "CREATE TABLE T1 ( first TEXT PRIMARY KEY );",
            )],
        )
        .expect_err("future forward-compatibility version must be rejected");
        assert!(matches!(err, backoff::Error::Permanent(_)));
        Ok(())
    }

    #[test]
    fn schema_version_is_derived_from_passed_migrations() -> anyhow::Result<()> {
        let mut db = rusqlite::Connection::open_in_memory()?;
        let forward_incompatible = [M::up(
            0,
            SchemaVersion::One,
            "CREATE TABLE T1 ( first TEXT PRIMARY KEY );",
        )];

        migration::run(&mut db, forward_incompatible)?;
        let version: u32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(version, SchemaVersion::One as u32);

        let err = migration::run(
            &mut db,
            [M::up(
                0,
                ZERO,
                "CREATE TABLE T1 ( first TEXT PRIMARY KEY );",
            )],
        )
        .expect_err("older binaries must reject newer schema versions");
        assert!(matches!(err, backoff::Error::Permanent(_)));
        Ok(())
    }

    #[test]
    fn schema_version_bumps_even_if_all_migrations_are_already_applied() -> anyhow::Result<()> {
        let mut db = rusqlite::Connection::open_in_memory()?;
        let compatible = [M::up(
            0,
            ZERO,
            "CREATE TABLE T1 ( first TEXT PRIMARY KEY );",
        )];
        migration::run(&mut db, compatible)?;

        let initial_version: u32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(initial_version, ZERO as u32);

        let forward_incompatible = [M::up(
            0,
            SchemaVersion::One,
            "CREATE TABLE T1 ( first TEXT PRIMARY KEY );",
        )];
        let count = migration::run(&mut db, forward_incompatible)?;
        assert_eq!(
            count, 0,
            "no SQL migration reruns when only the schema version changes"
        );

        let bumped_version: u32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(
            bumped_version,
            SchemaVersion::One as u32,
            "schema version is still bumped when the migration list is otherwise up-to-date"
        );
        Ok(())
    }

    #[test]
    fn schema_version_bumps_even_if_database_has_extra_migrations() -> anyhow::Result<()> {
        let mut db = rusqlite::Connection::open_in_memory()?;
        let first_sql = "CREATE TABLE T1 ( first TEXT PRIMARY KEY );";
        let compatible = [
            M::up(0, ZERO, first_sql),
            M::up(1, ZERO, "CREATE TABLE T2 ( first TEXT PRIMARY KEY );"),
        ];
        migration::run(&mut db, compatible)?;

        let initial_version: u32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(initial_version, ZERO as u32);

        let forward_incompatible = [M::up(0, SchemaVersion::One, first_sql)];
        let count = migration::run(&mut db, forward_incompatible)?;
        assert_eq!(
            count, 0,
            "no SQL migration reruns when only the schema version changes and the DB already has extra compatible migrations"
        );

        let bumped_version: u32 = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        assert_eq!(
            bumped_version,
            SchemaVersion::One as u32,
            "schema version is still bumped when the DB contains additional compatible migrations"
        );
        Ok(())
    }

    #[test]
    fn run_ours() -> anyhow::Result<()> {
        // See all of our schema combined in the latest version, along with
        // all migration versions we ran to get there.
        let mut db = rusqlite::Connection::open_in_memory()?;
        migration::run(&mut db, but_db::migration::ours())?;

        snapbox::assert_data_eq!(
            dump_schema(&db)?,
            snapbox::str![[r#"
-- table __diesel_schema_migrations
CREATE TABLE __diesel_schema_migrations (
       version VARCHAR(50) PRIMARY KEY NOT NULL,
       run_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- table ci_checks
CREATE TABLE `ci_checks`(
	`id` BIGINT NOT NULL PRIMARY KEY,
	`name` TEXT NOT NULL,
	`output_summary` TEXT NOT NULL,
	`output_text` TEXT NOT NULL,
	`output_title` TEXT NOT NULL,
	`started_at` TIMESTAMP,
	`status_type` TEXT NOT NULL,
	`status_conclusion` TEXT,
	`status_completed_at` TIMESTAMP,
	`head_sha` TEXT NOT NULL,
	`url` TEXT NOT NULL,
	`html_url` TEXT NOT NULL,
	`details_url` TEXT NOT NULL,
	`pull_requests` TEXT NOT NULL,
	`reference` TEXT NOT NULL,
	`last_sync_at` TIMESTAMP NOT NULL,
	`struct_version` INTEGER NOT NULL
);

-- table fetch_status
CREATE TABLE `fetch_status`(
	`singleton` INTEGER NOT NULL PRIMARY KEY CHECK (`singleton` = 1),
	`last_attempted_ms` INTEGER NOT NULL,
	`last_successful_ms` INTEGER,
	`last_error` TEXT
);

-- table forge_reviews
CREATE TABLE `forge_reviews`(
	`html_url` TEXT NOT NULL,
	`number` BIGINT NOT NULL PRIMARY KEY,
	`title` TEXT NOT NULL,
	`body` TEXT,
	`author` TEXT,
	`labels` TEXT NOT NULL,
	`draft` BOOL NOT NULL,
	`source_branch` TEXT NOT NULL,
	`target_branch` TEXT NOT NULL,
	`sha` TEXT NOT NULL,
	`created_at` TIMESTAMP,
	`modified_at` TIMESTAMP,
	`merged_at` TIMESTAMP,
	`closed_at` TIMESTAMP,
	`repository_ssh_url` TEXT,
	`repository_https_url` TEXT,
	`repo_owner` TEXT,
	`reviewers` TEXT NOT NULL,
	`unit_symbol` TEXT NOT NULL,
	`last_sync_at` TIMESTAMP NOT NULL,
	`struct_version` INTEGER NOT NULL,
	`head_repo_is_fork` BOOL NOT NULL DEFAULT FALSE,
	`integration_commit_shas` TEXT NOT NULL DEFAULT '[]',
	`auto_merge_enabled` BOOL NOT NULL DEFAULT FALSE
);

-- index idx_ci_checks_reference
CREATE INDEX `idx_ci_checks_reference` ON `ci_checks`(`reference`);


"#]]
        );

        snapbox::assert_data_eq!(
            dump_data(&db)?,
            snapbox::str![[r#"
Table: __diesel_schema_migrations
version
Text("20261112000001")
Text("20261112000002")
Text("20261112000003")

Table: forge_reviews
html_url | number | title | body | author | labels | draft | source_branch | target_branch | sha | created_at | modified_at | merged_at | closed_at | repository_ssh_url | repository_https_url | repo_owner | reviewers | unit_symbol | last_sync_at | struct_version | head_repo_is_fork | integration_commit_shas | auto_merge_enabled

Table: ci_checks
id | name | output_summary | output_text | output_title | started_at | status_type | status_conclusion | status_completed_at | head_sha | url | html_url | details_url | pull_requests | reference | last_sync_at | struct_version

Table: fetch_status
singleton | last_attempted_ms | last_successful_ms | last_error


"#]]
        );
        let count = migration::run(&mut db, but_db::migration::ours())?;
        assert_eq!(count, 0, "nothing has to run as everything is uptodate");

        Ok(())
    }
}

mod util {
    use std::fmt::Write;
    pub fn dump_data(conn: &rusqlite::Connection) -> anyhow::Result<String> {
        // Get all table names
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        )?;

        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;

        let mut out = String::new();
        for table in tables {
            dump_table(conn, &table, &mut out)?;
            writeln!(out)?;
        }
        Ok(out)
    }

    pub fn dump_schema(conn: &rusqlite::Connection) -> anyhow::Result<String> {
        let mut stmt = conn.prepare(
            "SELECT type, name, sql FROM sqlite_master
         WHERE sql NOT NULL
         ORDER BY CASE type
             WHEN 'table' THEN 1
             WHEN 'index' THEN 2
             WHEN 'trigger' THEN 3
             WHEN 'view' THEN 4
         END, name",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut out = String::new();
        for row in rows {
            let (type_, name, sql) = row?;
            writeln!(out, "-- {type_} {name}")?;
            writeln!(out, "{sql};")?;
            writeln!(out)?;
        }
        Ok(out)
    }

    fn dump_table(
        conn: &rusqlite::Connection,
        table_name: &str,
        out: &mut String,
    ) -> anyhow::Result<()> {
        let query = if table_name == "__diesel_schema_migrations" {
            format!("SELECT version FROM {table_name}")
        } else {
            format!("SELECT * FROM {table_name}")
        };
        let mut stmt = conn.prepare(&query)?;

        let column_count = stmt.column_count();
        let column_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();

        writeln!(out, "Table: {table_name}")?;
        writeln!(out, "{}", column_names.join(" | "))?;

        let rows = stmt.query_map([], |row| {
            let mut values = Vec::new();
            for i in 0..column_count {
                let val: String = row
                    .get::<_, rusqlite::types::Value>(i)
                    .map(|v| format!("{v:?}"))
                    .unwrap_or_else(|_| "NULL".to_string());
                values.push(val);
            }
            Ok(values.join(" | "))
        })?;

        for row in rows {
            writeln!(out, "{}", row?)?;
        }
        Ok(())
    }
}
