//! Resolve CLI flags, environment values, and defaults into validated listener, public
//! URL, store, feature, and provider settings. Keep secrets out of diagnostics.
//!
//! Proposed: fn parse() -> Result<Config, ConfigError>
//! Proposed: fn validate(config: Config) -> Result<Config, ConfigError>
