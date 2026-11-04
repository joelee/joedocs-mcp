//! Secrets loaded from the environment.
//!
//! Secrets are never part of `config.toml` and cannot be set through config
//! overrides. They are held in [`SecretString`] so `Debug` output and logs
//! show `[REDACTED]` instead of the value.

use secrecy::SecretString;

use crate::app_id::AppId;
use crate::env::Env;

/// Secret values the application may use. Add fields here and document each
/// variable in `.env.example` and `docs/configuration.md`.
#[derive(Debug, Default)]
pub struct Secrets {
    /// Example secret read from `<PREFIX>_API_TOKEN`.
    pub api_token: Option<SecretString>,
}

impl Secrets {
    /// Reads secrets from `env`. Empty values count as unset.
    #[must_use]
    pub fn from_env(app: &AppId, env: &dyn Env) -> Self {
        Self {
            api_token: env
                .non_empty(&app.secret_var("API_TOKEN"))
                .map(SecretString::from),
        }
    }

    /// Returns `(variable name, is set)` for every supported secret, for
    /// diagnostics that must not reveal values.
    #[must_use]
    pub fn status(&self, app: &AppId) -> Vec<(String, bool)> {
        vec![(app.secret_var("API_TOKEN"), self.api_token.is_some())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;
    use secrecy::ExposeSecret as _;

    #[test]
    fn reads_secret_and_reports_status_without_value() {
        let app = AppId::new("my-app");
        let env = MapEnv::new().with("MY_APP_API_TOKEN", "s3cr3t-value");
        let secrets = Secrets::from_env(&app, &env);

        assert_eq!(
            secrets
                .api_token
                .as_ref()
                .map(|s| s.expose_secret().to_owned()),
            Some("s3cr3t-value".to_owned())
        );
        assert_eq!(
            secrets.status(&app),
            vec![("MY_APP_API_TOKEN".into(), true)]
        );
    }

    #[test]
    fn debug_output_redacts_secret() {
        let app = AppId::new("my-app");
        let env = MapEnv::new().with("MY_APP_API_TOKEN", "s3cr3t-value");
        let debug = format!("{:?}", Secrets::from_env(&app, &env));
        assert!(!debug.contains("s3cr3t-value"));
    }

    #[test]
    fn empty_secret_counts_as_unset() {
        let app = AppId::new("my-app");
        let env = MapEnv::new().with("MY_APP_API_TOKEN", "");
        let secrets = Secrets::from_env(&app, &env);
        assert!(secrets.api_token.is_none());
        assert_eq!(
            secrets.status(&app),
            vec![("MY_APP_API_TOKEN".into(), false)]
        );
    }
}
