use std::{collections::HashMap, env, fs, path::PathBuf};

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
    pub(super) fn load() -> Self {
        user_npmrc_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|contents| Self::parse(&contents))
            .unwrap_or_default()
    }

    fn parse(contents: &str) -> Self {
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
            let value = expand_environment_variables(unquote(value.trim()));

            if key.eq_ignore_ascii_case("registry") {
                if let Some(registry) = parse_registry_url(&value) {
                    config.default_registry = registry;
                }
            } else if let Some(scope) = key.strip_suffix(":registry") {
                if scope.starts_with('@') {
                    if let Some(registry) = parse_registry_url(&value) {
                        config.scoped_registries.insert(scope.to_owned(), registry);
                    }
                }
            } else if key.eq_ignore_ascii_case("strict-ssl")
                || key.eq_ignore_ascii_case("strictssl")
            {
                if let Ok(strict_ssl) = value.parse::<bool>() {
                    config.strict_ssl = strict_ssl;
                }
            } else if let Some(prefix) = auth_registry_prefix(key) {
                auth_tokens.insert(prefix, value);
            }
        }

        config.auth_tokens = auth_tokens
            .into_iter()
            .map(|(registry_prefix, token)| AuthToken {
                registry_prefix,
                token,
            })
            .collect();
        config
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

fn user_npmrc_path() -> Option<PathBuf> {
    env::var_os("NPM_CONFIG_USERCONFIG")
        .or_else(|| env::var_os("npm_config_userconfig"))
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .or_else(|| env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".npmrc"))
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

fn unquote(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn expand_environment_variables(value: &str) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut remainder = value;

    while let Some(start) = remainder.find("${") {
        expanded.push_str(&remainder[..start]);
        let after_start = &remainder[start + 2..];
        let Some(end) = after_start.find('}') else {
            expanded.push_str(&remainder[start..]);
            return expanded;
        };
        let variable = &after_start[..end];
        match env::var(variable) {
            Ok(value) => expanded.push_str(&value),
            Err(_) => expanded.push_str(&remainder[start..start + end + 3]),
        }
        remainder = &after_start[end + 1..];
    }

    expanded.push_str(remainder);
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
        );

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
        );
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
        );
        let url = config.package_url("@internal/a-package").unwrap();

        assert_eq!(config.auth_token_for(&url), Some("specific-token"));
    }

    #[test]
    fn defaults_to_the_public_npm_registry() {
        let config = NpmConfig::parse("# no registry configuration");

        assert_eq!(
            config.package_url("@scope/package").unwrap().as_str(),
            "https://registry.npmjs.org/@scope%2Fpackage"
        );
        assert!(config.strict_ssl());
    }
}
