//! The application's identity: its name and the environment-variable prefix
//! derived from it.

/// Name and environment prefix used for config paths and variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppId {
    name: String,
    env_prefix: String,
}

impl AppId {
    /// Builds an identity from a kebab-case name such as `my-app`. The
    /// environment prefix is the name upper-cased with `-` replaced by `_`
    /// (`MY_APP`).
    ///
    /// # Examples
    ///
    /// ```
    /// use joedocs::AppId;
    ///
    /// let app = AppId::new("my-app");
    /// assert_eq!(app.env_prefix(), "MY_APP");
    /// assert_eq!(app.config_file_var(), "MY_APP_CONFIG_FILE");
    /// ```
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            env_prefix: name.to_ascii_uppercase().replace('-', "_"),
        }
    }

    /// The identity of this crate, taken from `Cargo.toml`.
    #[must_use]
    pub fn current() -> Self {
        Self::new(env!("CARGO_PKG_NAME"))
    }

    /// The kebab-case application name, used for config directories.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The upper-case environment prefix, e.g. `MY_APP`.
    #[must_use]
    pub fn env_prefix(&self) -> &str {
        &self.env_prefix
    }

    /// The variable that selects a config file: `<PREFIX>_CONFIG_FILE`.
    #[must_use]
    pub fn config_file_var(&self) -> String {
        format!("{}_CONFIG_FILE", self.env_prefix)
    }

    /// The prefix shared by per-key overrides: `<PREFIX>__`.
    #[must_use]
    pub fn override_prefix(&self) -> String {
        format!("{}__", self.env_prefix)
    }

    /// The variable holding a secret: `<PREFIX>_<NAME>`.
    #[must_use]
    pub fn secret_var(&self, name: &str) -> String {
        format!("{}_{name}", self.env_prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_prefix_and_variable_names() {
        let app = AppId::new("my-app");
        assert_eq!(app.name(), "my-app");
        assert_eq!(app.env_prefix(), "MY_APP");
        assert_eq!(app.config_file_var(), "MY_APP_CONFIG_FILE");
        assert_eq!(app.override_prefix(), "MY_APP__");
        assert_eq!(app.secret_var("API_TOKEN"), "MY_APP_API_TOKEN");
    }

    #[test]
    fn current_uses_the_package_name() {
        assert_eq!(AppId::current().name(), env!("CARGO_PKG_NAME"));
    }
}
