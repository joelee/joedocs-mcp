//! Guards against `config.example.toml` drifting from the schema.

use std::path::Path;

use joedocs::Config;

#[test]
fn example_config_parses_and_matches_defaults() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let parsed: Config = toml::from_str(&text).unwrap();
    assert_eq!(
        parsed,
        Config::default(),
        "config.example.toml must list every key with its default value"
    );

    let defaults = toml::Table::try_from(Config::default()).unwrap();
    let example: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(
        example, defaults,
        "config.example.toml must set every key explicitly"
    );
}
