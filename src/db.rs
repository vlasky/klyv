//! Schema, connection setup, and the key/expiry helpers shared by all commands.

use crate::reply::CmdError;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;

pub(crate) fn open_db(path: &PathBuf) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        PRAGMA busy_timeout=5000;

        CREATE TABLE IF NOT EXISTS strings (
            key TEXT PRIMARY KEY,
            value BLOB NOT NULL
        );

        CREATE TABLE IF NOT EXISTS list_items (
            key TEXT NOT NULL,
            idx REAL NOT NULL,
            value BLOB NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_list_key_idx ON list_items(key, idx);

        CREATE TABLE IF NOT EXISTS set_members (
            key TEXT NOT NULL,
            member BLOB NOT NULL,
            UNIQUE(key, member)
        );

        CREATE TABLE IF NOT EXISTS hash_fields (
            key TEXT NOT NULL,
            field TEXT NOT NULL,
            value BLOB NOT NULL,
            UNIQUE(key, field)
        );

        CREATE TABLE IF NOT EXISTS expiry (
            key TEXT PRIMARY KEY,
            expires_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_expiry_at ON expiry(expires_at);
    ",
    )?;
    Ok(conn)
}

pub(crate) fn is_expired(conn: &Connection, key: &str) -> Result<bool, rusqlite::Error> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM expiry WHERE key = ?1 AND expires_at <= unixepoch()",
            params![key],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub(crate) fn exists_in(conn: &Connection, sql: &str, key: &str) -> Result<bool, rusqlite::Error> {
    Ok(conn
        .query_row(sql, params![key], |_| Ok(()))
        .optional()?
        .is_some())
}

pub(crate) fn key_type(
    conn: &Connection,
    key: &str,
) -> Result<Option<&'static str>, rusqlite::Error> {
    if is_expired(conn, key)? {
        return Ok(None);
    }
    if exists_in(conn, "SELECT 1 FROM strings WHERE key = ?1", key)? {
        return Ok(Some("string"));
    }
    if exists_in(conn, "SELECT 1 FROM list_items WHERE key = ?1 LIMIT 1", key)? {
        return Ok(Some("list"));
    }
    if exists_in(
        conn,
        "SELECT 1 FROM set_members WHERE key = ?1 LIMIT 1",
        key,
    )? {
        return Ok(Some("set"));
    }
    if exists_in(
        conn,
        "SELECT 1 FROM hash_fields WHERE key = ?1 LIMIT 1",
        key,
    )? {
        return Ok(Some("hash"));
    }
    Ok(None)
}

pub(crate) fn ensure_type(conn: &Connection, key: &str, want: &str) -> Result<(), CmdError> {
    match key_type(conn, key)? {
        Some(t) if t != want => Err(CmdError::new(
            "WRONGTYPE Operation against a key holding the wrong kind of value",
        )),
        _ => Ok(()),
    }
}

pub(crate) fn drop_if_expired(conn: &Connection, key: &str) -> Result<(), rusqlite::Error> {
    if is_expired(conn, key)? {
        conn.execute("DELETE FROM strings WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM list_items WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM set_members WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM hash_fields WHERE key = ?1", params![key])?;
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    }
    Ok(())
}

pub(crate) fn key_exists_in_data(conn: &Connection, key: &str) -> Result<bool, rusqlite::Error> {
    Ok(
        exists_in(conn, "SELECT 1 FROM strings WHERE key = ?1", key)?
            || exists_in(conn, "SELECT 1 FROM list_items WHERE key = ?1 LIMIT 1", key)?
            || exists_in(
                conn,
                "SELECT 1 FROM set_members WHERE key = ?1 LIMIT 1",
                key,
            )?
            || exists_in(
                conn,
                "SELECT 1 FROM hash_fields WHERE key = ?1 LIMIT 1",
                key,
            )?,
    )
}

/// Removing the last element of a list/set/hash deletes the key, so any
/// expiry row must go with it — otherwise a later SET on the same key would
/// silently inherit the stale TTL.
pub(crate) fn drop_expiry_if_empty(conn: &Connection, key: &str) -> Result<(), rusqlite::Error> {
    if !key_exists_in_data(conn, key)? {
        conn.execute("DELETE FROM expiry WHERE key = ?1", params![key])?;
    }
    Ok(())
}
