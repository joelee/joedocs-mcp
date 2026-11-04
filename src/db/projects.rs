//! Project queries.

use rusqlite::{Connection, OptionalExtension as _, Row, params};

/// A registered project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    /// Row ID.
    pub id: i64,
    /// Slug name, also the vault directory.
    pub name: String,
    /// Immutable document ID prefix, e.g. `JD`.
    pub key: String,
    /// Human-readable title.
    pub title: String,
    /// Registration time.
    pub created_at: String,
    /// Number of live (not archived) documents.
    pub documents: u64,
}

const SELECT: &str = "SELECT p.id, p.name, p.key, p.title, p.created_at,
    (SELECT count(*) FROM documents d WHERE d.project_id = p.id AND d.archived_at IS NULL)
    FROM projects p";

fn from_row(row: &Row<'_>) -> rusqlite::Result<ProjectRow> {
    Ok(ProjectRow {
        id: row.get(0)?,
        name: row.get(1)?,
        key: row.get(2)?,
        title: row.get(3)?,
        created_at: row.get(4)?,
        documents: row.get(5)?,
    })
}

/// Inserts a project and returns its row ID.
///
/// # Errors
///
/// Returns an error when the name or key is taken or the statement fails.
pub fn insert_project(
    conn: &Connection,
    name: &str,
    key: &str,
    title: &str,
    created_at: &str,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO projects (name, key, title, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![name, key, title, created_at],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Every project, by name.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn list_projects(conn: &Connection) -> rusqlite::Result<Vec<ProjectRow>> {
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY p.name"))?;
    stmt.query_map([], from_row)?.collect()
}

/// The project with this name or key.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_project(conn: &Connection, name_or_key: &str) -> rusqlite::Result<Option<ProjectRow>> {
    conn.query_row(
        &format!("{SELECT} WHERE p.name = ?1 OR p.key = ?1"),
        [name_or_key],
        from_row,
    )
    .optional()
}

/// The project with this row ID.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn find_project_by_id(conn: &Connection, id: i64) -> rusqlite::Result<Option<ProjectRow>> {
    conn.query_row(&format!("{SELECT} WHERE p.id = ?1"), [id], from_row)
        .optional()
}

/// Takes the project's next document sequence number.
///
/// # Errors
///
/// Returns an error when the project does not exist or the statement fails.
pub fn allocate_seq(conn: &Connection, project_id: i64) -> rusqlite::Result<u64> {
    conn.query_row(
        "UPDATE projects SET next_seq = next_seq + 1 WHERE id = ?1 RETURNING next_seq - 1",
        [project_id],
        |row| row.get(0),
    )
}

/// Makes sure the project's next sequence number is at least `next`.
///
/// # Errors
///
/// Returns an error when the statement fails.
pub fn raise_next_seq(conn: &Connection, project_id: i64, next: u64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE projects SET next_seq = max(next_seq, ?2) WHERE id = ?1",
        params![project_id, next],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;

    #[test]
    fn projects_are_found_by_name_key_or_id_and_allocate_sequences() {
        let conn = open_in_memory().unwrap();
        let id = insert_project(&conn, "joedocs", "JD", "JoeDocs", "t").unwrap();
        insert_project(&conn, "alpha", "AL", "Alpha", "t").unwrap();
        let by_name = find_project(&conn, "joedocs").unwrap().unwrap();
        assert_eq!(by_name.key, "JD");
        assert_eq!(by_name.documents, 0);
        assert_eq!(find_project(&conn, "JD").unwrap(), Some(by_name.clone()));
        assert_eq!(find_project_by_id(&conn, id).unwrap(), Some(by_name));
        assert_eq!(find_project(&conn, "nope").unwrap(), None);
        let names: Vec<_> = list_projects(&conn)
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, ["alpha", "joedocs"]);

        assert_eq!(allocate_seq(&conn, id).unwrap(), 1);
        assert_eq!(allocate_seq(&conn, id).unwrap(), 2);
        assert!(allocate_seq(&conn, 999).is_err());
        assert!(insert_project(&conn, "other", "JD", "Dup key", "t").is_err());
    }
}
