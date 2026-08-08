use std::{
    collections::HashMap,
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use reqwest::Url;

const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org/";

/// The subset of npm's user configuration needed to query package metadata.
pub(super) struct NpmConfig {
    default_registry: Url,
    scoped_registries: HashMap<String, Url>,
    auth_tokens: Vec<AuthToken>,
    strict_ssl: bool,
}

struct AuthToken {
    registry_prefix: String,
    token: String,
}

impl Default for NpmConfig {
    fn default() -> Self {
        Self {
            default_registry: Url::parse(DEFAULT_REGISTRY)
                .expect("the default npm registry URL should be valid"),
            scoped_registries: HashMap::new(),
            auth_tokens: Vec::new(),
            strict_ssl: true,
        }
    }
}

impl NpmConfig {
    pub(super) fn load() -> Result<Self> {
        let Some((path, explicit)) = user_npmrc_path() else {
            return Ok(Self::default());
        };
        Self::load_from_path(&path, explicit)
    }

    fn load_from_path(path: &Path, explicit: bool) -> Result<Self> {
        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if !explicit && error.kind() == ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read npm configuration at {path:?}"));
            }
        };
        Self::parse(&contents)
            .with_context(|| format!("failed to parse npm configuration at {path:?}"))
    }

    pub(super) fn parse(contents: &str) -> Result<Self> {
        let mut config = Self::default();
        let mut auth_tokens = HashMap::new();

        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = expand_environment_variables(&parse_ini_value(value));

            if key.eq_ignore_ascii_case("registry") {
                config.default_registry = parse_registry_url(&value)
                    .ok_or_else(|| anyhow!("invalid registry URL for `{key}`"))?;
            } else if let Some(scope) = key.strip_suffix(":registry") {
                if scope.starts_with('@') {
                    let registry = parse_registry_url(&value)
                        .ok_or_else(|| anyhow!("invalid registry URL for `{key}`"))?;
                    config.scoped_registries.insert(scope.to_owned(), registry);
                }
            } else if key.eq_ignore_ascii_case("strict-ssl")
                || key.eq_ignore_ascii_case("strictssl")
            {
                config.strict_ssl = value
                    .parse::<bool>()
                    .map_err(|_| anyhow!("invalid boolean value for `{key}`"))?;
            } else if key.ends_with(":_authToken") {
                let prefix = auth_registry_prefix(key)
                    .ok_or_else(|| anyhow!("invalid authentication registry for `{key}`"))?;
                if !value.is_empty() {
                    auth_tokens.insert(prefix, value);
                }
            }
        }

        config.auth_tokens = auth_tokens
            .into_iter()
            .map(|(registry_prefix, token)| AuthToken {
                registry_prefix,
                token,
            })
            .collect();
        Ok(config)
    }

    pub(super) fn strict_ssl(&self) -> bool {
        self.strict_ssl
    }

    pub(super) fn package_url(&self, package_name: &str) -> Option<Url> {
        let mut url = self.registry_for_package(package_name).clone();
        url.path_segments_mut()
            .ok()?
            .pop_if_empty()
            .push(package_name);
        Some(url)
    }

    pub(super) fn auth_token_for<'a>(&'a self, url: &Url) -> Option<&'a str> {
        let request_key = registry_key(url)?;
        self.auth_tokens
            .iter()
            .filter(|auth| request_key.starts_with(&auth.registry_prefix))
            .max_by_key(|auth| auth.registry_prefix.len())
            .map(|auth| auth.token.as_str())
    }

    fn registry_for_package(&self, package_name: &str) -> &Url {
        let scope = package_name
            .strip_prefix('@')
            .and_then(|name| name.split_once('/'))
            .map(|(scope, _)| format!("@{scope}"));

        scope
            .as_ref()
            .and_then(|scope| self.scoped_registries.get(scope))
            .unwrap_or(&self.default_registry)
    }
}

fn user_npmrc_path() -> Option<(PathBuf, bool)> {
    env::var_os("NPM_CONFIG_USERCONFIG")
        .or_else(|| env::var_os("npm_config_userconfig"))
        .filter(|path| !path.is_empty())
        .map(|path| (PathBuf::from(path), true))
        .or_else(|| {
            env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .map(|home| (PathBuf::from(home).join(".npmrc"), false))
        })
}

fn parse_registry_url(value: &str) -> Option<Url> {
    let mut url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

fn auth_registry_prefix(key: &str) -> Option<String> {
    let registry = key.strip_suffix(":_authToken")?;
    let url = Url::parse(&format!("https:{registry}")).ok()?;
    let mut prefix = registry_key(&url)?;
    if !prefix.ends_with('/') {
        prefix.push('/');
    }
    Some(prefix)
}

fn registry_key(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let mut key = format!("//{host}");
    if let Some(port) = url.port() {
        key.push(':');
        key.push_str(&port.to_string());
    }
    key.push_str(url.path());
    Some(key)
}

fn parse_ini_value(value: &str) -> String {
    let value = value.trim();
    let opening_quote = value
        .chars()
        .next()
        .filter(|character| matches!(character, '"' | '\''));
    let mut parsed = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    let mut active_quote = opening_quote;
    let mut first = true;

    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.peek().copied() {
                Some(next @ ('#' | ';' | '"' | '\'' | '\\')) => {
                    parsed.push(next);
                    chars.next();
                }
                _ => parsed.push(character),
            }
        } else if !first && active_quote == Some(character) {
            active_quote = None;
            parsed.push(character);
        } else if active_quote.is_none() && matches!(character, '#' | ';') {
            break;
        } else {
            parsed.push(character);
        }
        first = false;
    }

    let parsed = parsed.trim_end();
    if let Some(quote) = opening_quote {
        if parsed.len() >= 2 && parsed.ends_with(quote) {
            return parsed[1..parsed.len() - 1].to_owned();
        }
    }
    parsed.to_owned()
}

fn expand_environment_variables(value: &str) -> String {
    expand_environment_variables_with(value, |variable| env::var(variable).ok())
}

fn expand_environment_variables_with(
    value: &str,
    mut resolve: impl FnMut(&str) -> Option<String>,
) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut remainder = value;

    while !remainder.is_empty() {
        if let Some(after_start) = remainder.strip_prefix("\\${") {
            let Some(end) = after_start.find('}') else {
                expanded.push_str(&remainder[1..]);
                break;
            };
            expanded.push_str(&remainder[1..end + 4]);
            remainder = &after_start[end + 1..];
        } else if let Some(after_start) = remainder.strip_prefix("${") {
            let Some(end) = after_start.find('}') else {
                expanded.push_str(remainder);
                break;
            };
            let expression = &after_start[..end];
            let (variable, optional) = expression
                .strip_suffix('?')
                .map_or((expression, false), |variable| (variable, true));
            if let Some(value) = resolve(variable) {
                expanded.push_str(&value);
            } else if !optional {
                expanded.push_str(&remainder[..end + 3]);
            }
            remainder = &after_start[end + 1..];
        } else {
            let character = remainder
                .chars()
                .next()
                .expect("remainder should not be empty");
            expanded.push(character);
            remainder = &remainder[character.len_utf8()..];
        }
    }

    expanded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_scoped_registries_and_matching_auth_tokens() {
        let config = NpmConfig::parse(
            r#"
@internal:registry=https://artifactory.example.com/artifactory/api/npm/npm_prod_vir/
@internal-platform:registry=https://artifactory.example.com/api/npm/npm_prod_vir/
//artifactory.example.com/artifactory/api/npm/npm_prod_vir/:_authToken=abc123
strictssl=false
"#,
        )
        .unwrap();

        let internal_url = config.package_url("@internal/a-package").unwrap();
        assert_eq!(
            internal_url.as_str(),
            "https://artifactory.example.com/artifactory/api/npm/npm_prod_vir/@internal%2Fa-package"
        );
        assert_eq!(config.auth_token_for(&internal_url), Some("abc123"));

        let platform_url = config.package_url("@internal-platform/a-package").unwrap();
        assert_eq!(
            platform_url.as_str(),
            "https://artifactory.example.com/api/npm/npm_prod_vir/@internal-platform%2Fa-package"
        );
        assert_eq!(config.auth_token_for(&platform_url), None);
        assert!(!config.strict_ssl());
    }

    #[test]
    fn uses_configured_default_registry_for_unscoped_packages() {
        let config = NpmConfig::parse(
            r#"
registry=https://registry.example.com/npm
//registry.example.com/npm/:_authToken=default-token
strict-ssl=true
"#,
        )
        .unwrap();
        let url = config.package_url("example-package").unwrap();

        assert_eq!(
            url.as_str(),
            "https://registry.example.com/npm/example-package"
        );
        assert_eq!(config.auth_token_for(&url), Some("default-token"));
        assert!(config.strict_ssl());
    }

    #[test]
    fn uses_the_most_specific_auth_token() {
        let config = NpmConfig::parse(
            r#"
@internal:registry=https://registry.example.com/npm/internal/
//registry.example.com/npm/:_authToken=broad-token
//registry.example.com/npm/internal/:_authToken=specific-token
"#,
        )
        .unwrap();
        let url = config.package_url("@internal/a-package").unwrap();

        assert_eq!(config.auth_token_for(&url), Some("specific-token"));
    }

    #[test]
    fn defaults_to_the_public_npm_registry() {
        let config = NpmConfig::parse("# no registry configuration").unwrap();

        assert_eq!(
            config.package_url("@scope/package").unwrap().as_str(),
            "https://registry.npmjs.org/@scope%2Fpackage"
        );
        assert!(config.strict_ssl());
    }

    #[test]
    fn rejects_invalid_registry_urls_instead_of_falling_back() {
        let error = NpmConfig::parse("@internal:registry=not-a-url")
            .err()
            .unwrap();

        assert!(error.to_string().contains("invalid registry URL"));
    }

    #[test]
    fn requires_explicit_configuration_files() {
        let path = std::env::temp_dir().join(format!(
            "missing-package-version-server-npmrc-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);

        assert!(NpmConfig::load_from_path(&path, true).is_err());
        assert!(NpmConfig::load_from_path(&path, false).is_ok());
    }

    #[test]
    fn parses_inline_comments_quotes_and_escapes() {
        assert_eq!(
            parse_ini_value(" https://registry.example.com/npm/ ; mirror"),
            "https://registry.example.com/npm/"
        );
        assert_eq!(parse_ini_value(r#""token#part" # comment"#), "token#part");
        assert_eq!(parse_ini_value(r"token\;part; comment"), "token;part");
        assert_eq!(parse_ini_value("token'part; comment"), "token'part");
    }

    #[test]
    fn expands_optional_and_escaped_environment_variables() {
        let value = expand_environment_variables_with(
            r"${SET}:${MISSING?}:${MISSING}:\${SET}",
            |variable| (variable == "SET").then(|| "value".to_owned()),
        );

        assert_eq!(value, "value::${MISSING}:${SET}");
    }
}
