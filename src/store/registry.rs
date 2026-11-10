//! Projects and API keys. These live only in the database, so they do not
//! create history commits.

use secrecy::SecretString;

use super::{Actor, Error, Store, index_file, lock};
use crate::auth::{self, Grant, RandomSource};
use crate::db::{self, AuditEntry, Connection, KeyRow, ProjectRow};
use crate::document::{DocumentId, check_title, is_project_key, is_slug};

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
        match lock(&self.paths) {
            Ok(_lock) => index_file::refresh(&self.conn, &self.paths.home, &self.settings, name),
            Err(error) => {
                tracing::warn!(project = %name, %error, "could not write {name}/INDEX.md");
            }
        }
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

    /// Removes a key's grants on `projects`, all or nothing. The key stays
    /// active, even with no grants left; its next request sees the change.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyNotFound`], [`Error::ProjectNotFound`], and
    /// [`Error::Invalid`] for no projects, a revoked key, or a project the
    /// key has no grant on.
    pub fn ungrant(&mut self, name: &str, projects: &[String]) -> Result<KeyRow, Error> {
        if projects.is_empty() {
            return Err(Error::invalid("project", "give at least one project"));
        }
        let tx = self.conn.transaction()?;
        let key =
            db::find_key_by_name(&tx, name)?.ok_or_else(|| Error::KeyNotFound(name.to_owned()))?;
        if key.revoked_at.is_some() {
            return Err(Error::invalid("name", format!("API key {name} is revoked")));
        }
        for project in projects {
            let row = db::find_project(&tx, project)?
                .filter(|p| &p.name == project)
                .ok_or_else(|| Error::ProjectNotFound(project.clone()))?;
            if db::remove_grant(&tx, &key.id, row.id)? == 0 {
                return Err(Error::invalid(
                    "project",
                    format!("API key {name} has no grant on {project}"),
                ));
            }
        }
        let key =
            db::find_key_by_name(&tx, name)?.ok_or_else(|| Error::KeyNotFound(name.to_owned()))?;
        tx.commit()?;
        Ok(key)
    }

    /// Issues a new token for a key and invalidates the old one at once.
    /// The key keeps its ID, name and grants, so the audit log and the
    /// history still attribute its changes to it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyNotFound`], [`Error::Invalid`] for a revoked key
    /// (revocation is permanent; create a new key), and database or random
    /// source errors.
    pub fn rotate_key(
        &mut self,
        name: &str,
        random: &dyn RandomSource,
    ) -> Result<CreatedKey, Error> {
        let tx = self.conn.transaction()?;
        let key =
            db::find_key_by_name(&tx, name)?.ok_or_else(|| Error::KeyNotFound(name.to_owned()))?;
        if key.revoked_at.is_some() {
            return Err(Error::invalid(
                "name",
                format!("API key {name} is revoked; create a new key instead"),
            ));
        }
        let new = auth::new_secret(&key.id, random)?;
        db::set_key_secret(&tx, &key.id, &new.secret_sha256)?;
        tx.commit()?;
        Ok(CreatedKey {
            key,
            token: new.token,
        })
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

impl Store {
    /// The key a bearer token belongs to, if the token is well formed,
    /// matches, and the key is not revoked. Records the time of use.
    ///
    /// # Errors
    ///
    /// Returns database errors only; every kind of bad token is `None`.
    pub fn authenticate(&self, token: &SecretString) -> Result<Option<KeyRow>, Error> {
        let Some((id, secret)) = auth::parse_token(token) else {
            return Ok(None);
        };
        let Some(hash) = db::key_secret(&self.conn, &id)? else {
            return Ok(None);
        };
        if !auth::verify_secret(&secret, &hash) {
            return Ok(None);
        }
        let Some(key) = db::find_key_by_id(&self.conn, &id)? else {
            return Ok(None);
        };
        if key.revoked_at.is_some() {
            return Ok(None);
        }
        db::touch_key(&self.conn, &id, &self.now())?;
        Ok(Some(key))
    }

    /// The project with this name.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn project(&self, name: &str) -> Result<Option<ProjectRow>, Error> {
        Ok(db::find_project(&self.conn, name)?.filter(|p| p.name == name))
    }

    /// The project a document ID belongs to (by its key prefix), whether or
    /// not the document exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] for a malformed ID, and database errors.
    pub fn project_of_document(&self, id: &str) -> Result<Option<ProjectRow>, Error> {
        let id: DocumentId = id.parse().map_err(|e: crate::document::InvalidId| {
            Error::invalid("document_id", e.to_string())
        })?;
        Ok(db::find_project(&self.conn, id.key())?.filter(|p| p.key == id.key()))
    }

    /// Records a tool call in the audit log.
    ///
    /// # Errors
    ///
    /// Returns database errors.
    pub fn audit(
        &self,
        key_id: &str,
        actor: &Actor,
        tool: &str,
        document_id: Option<&str>,
        outcome: &str,
    ) -> Result<(), Error> {
        let at = self.now();
        db::insert_audit(
            &self.conn,
            &AuditEntry {
                at: &at,
                api_key_id: key_id,
                key_name: actor.name(),
                client: actor.client(),
                tool,
                document_id,
                outcome,
            },
        )?;
        Ok(())
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
