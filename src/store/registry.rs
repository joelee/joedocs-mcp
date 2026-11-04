//! Projects and API keys. These live only in the database, so they do not
//! create history commits.

use secrecy::SecretString;

use super::{Error, Store};
use crate::auth::{self, Grant, RandomSource};
use crate::db::{self, Connection, KeyRow, ProjectRow};
use crate::document::{check_title, is_project_key, is_slug};

/// A key just created. The token is shown to the user once.
#[derive(Debug)]
pub struct CreatedKey {
    /// The stored key and its grants.
    pub key: KeyRow,
    /// The full `jd_<id>_<secret>` token.
    pub token: SecretString,
}

impl Store {
    /// Registers a project. `title` defaults to the name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a bad name, key or title, and
    /// [`Error::AlreadyExists`] when the name or key is taken.
    pub fn add_project(
        &mut self,
        name: &str,
        key: &str,
        title: Option<&str>,
    ) -> Result<ProjectRow, Error> {
        if !is_slug(name) {
            return Err(Error::invalid(
                "project",
                format!(
                    "\"{name}\" must be lower-case letters, digits and '-', starting with a letter or digit"
                ),
            ));
        }
        if !is_project_key(key) {
            return Err(Error::invalid(
                "key",
                format!(
                    "\"{key}\" must be 1 to 10 upper-case letters and digits, starting with a letter"
                ),
            ));
        }
        let title = check_title(title.unwrap_or(name)).map_err(|m| Error::invalid("title", m))?;
        let now = self.now();
        let tx = self.conn.transaction()?;
        for taken in [name, key] {
            if let Some(existing) = db::find_project(&tx, taken)? {
                let what = if existing.name == taken {
                    format!("project {taken}")
                } else {
                    format!("project key {taken} (used by {})", existing.name)
                };
                return Err(Error::AlreadyExists(what));
            }
        }
        let id = db::insert_project(&tx, name, key, &title, &now)?;
        let project = db::find_project_by_id(&tx, id)?
            .ok_or_else(|| Error::ProjectNotFound(name.to_owned()))?;
        tx.commit()?;
        Ok(project)
    }

    /// Every project, by name.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn projects(&self) -> Result<Vec<ProjectRow>, Error> {
        Ok(db::list_projects(&self.conn)?)
    }

    /// Creates an API key with `grants`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a bad name, [`Error::AlreadyExists`]
    /// when the name is taken, [`Error::ProjectNotFound`] for a grant on an
    /// unknown project, and random or database errors.
    pub fn create_key(
        &mut self,
        name: &str,
        grants: &[Grant],
        random: &dyn RandomSource,
    ) -> Result<CreatedKey, Error> {
        if !is_slug(name) {
            return Err(Error::invalid(
                "name",
                format!(
                    "\"{name}\" must be lower-case letters, digits and '-', starting with a letter or digit"
                ),
            ));
        }
        let new = auth::generate(random)?;
        let now = self.now();
        let tx = self.conn.transaction()?;
        if db::find_key_by_name(&tx, name)?.is_some() {
            return Err(Error::AlreadyExists(format!("API key {name}")));
        }
        db::insert_key(&tx, &new.id, name, &new.secret_sha256, &now)?;
        apply_grants(&tx, &new.id, grants)?;
        let key =
            db::find_key_by_name(&tx, name)?.ok_or_else(|| Error::KeyNotFound(name.to_owned()))?;
        tx.commit()?;
        Ok(CreatedKey {
            key,
            token: new.token,
        })
    }

    /// Every API key with its grants, by name.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn keys(&self) -> Result<Vec<KeyRow>, Error> {
        Ok(db::list_keys(&self.conn)?)
    }

    /// Adds or changes grants on an existing, unrevoked key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyNotFound`], [`Error::Invalid`] for a revoked key
    /// or no grants, and [`Error::ProjectNotFound`].
    pub fn grant(&mut self, name: &str, grants: &[Grant]) -> Result<KeyRow, Error> {
        if grants.is_empty() {
            return Err(Error::invalid(
                "grant",
                "give at least one project:permission",
            ));
        }
        let tx = self.conn.transaction()?;
        let key =
            db::find_key_by_name(&tx, name)?.ok_or_else(|| Error::KeyNotFound(name.to_owned()))?;
        if key.revoked_at.is_some() {
            return Err(Error::invalid("name", format!("API key {name} is revoked")));
        }
        apply_grants(&tx, &key.id, grants)?;
        let key =
            db::find_key_by_name(&tx, name)?.ok_or_else(|| Error::KeyNotFound(name.to_owned()))?;
        tx.commit()?;
        Ok(key)
    }

    /// Revokes a key. Revoking a revoked key changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyNotFound`] and database errors.
    pub fn revoke(&mut self, name: &str) -> Result<KeyRow, Error> {
        let now = self.now();
        let tx = self.conn.transaction()?;
        let key =
            db::find_key_by_name(&tx, name)?.ok_or_else(|| Error::KeyNotFound(name.to_owned()))?;
        db::revoke_key(&tx, &key.id, &now)?;
        let key =
            db::find_key_by_name(&tx, name)?.ok_or_else(|| Error::KeyNotFound(name.to_owned()))?;
        tx.commit()?;
        Ok(key)
    }
}

fn apply_grants(conn: &Connection, key_id: &str, grants: &[Grant]) -> Result<(), Error> {
    for grant in grants {
        let project = db::find_project(conn, &grant.project)?
            .filter(|p| p.name == grant.project)
            .ok_or_else(|| Error::ProjectNotFound(grant.project.clone()))?;
        db::set_grant(conn, key_id, project.id, grant.permission)?;
    }
    Ok(())
}
