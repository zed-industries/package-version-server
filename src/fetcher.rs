use std::{
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, FixedOffset};
use itertools::{Either, Itertools};
use reqwest::Client;

use crate::npmrc::NpmConfig;
use semver_rs::Parseable;
use serde_json::Value;
use tokio::sync::Mutex;

type PackageName = String;

static APP_USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " By Zed Industries"
);
pub(super) struct PackageVersionFetcher {
    client: Client,
    npm_config: NpmConfig,
    cache: Arc<Mutex<HashMap<PackageName, MetadataFromRegistry>>>,
}

/// How long do we keep data about a package around before requerying it the second time.
const REFRESH_DURATION: Duration = Duration::from_secs(30);

impl PackageVersionFetcher {
    pub(super) fn new() -> reqwest::Result<Self> {
        let npm_config = NpmConfig::load();
        let client = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .danger_accept_invalid_certs(!npm_config.strict_ssl())
            .build()?;
        Ok(Self {
            client,
            npm_config,
            cache: Default::default(),
        })
    }
    pub(super) async fn get(
        &self,
        package_name: &str,
        fetch_options: FetchOptions,
    ) -> Option<MetadataFromRegistry> {
        {
            let lock = self.cache.lock().await;
            let cached_entry = lock.get(package_name);
            if let Some(cached_entry) = cached_entry {
                if cached_entry.fetch_timestamp + REFRESH_DURATION > std::time::Instant::now() {
                    return Some(cached_entry.clone());
                }
            }
        }
        let latest_version =
            fetch(&self.client, &self.npm_config, package_name, fetch_options).await?;
        {
            match self.cache.lock().await.entry(package_name.into()) {
                Entry::Occupied(mut entry) => {
                    entry.insert(latest_version.clone());
                }
                Entry::Vacant(entry) => {
                    entry.insert(latest_version.clone());
                }
            }
        }
        Some(latest_version)
    }
}

pub(super) struct FetchOptions {
    pub parse_all_versions: bool,
}

#[derive(Clone)]
pub(super) struct MetadataFromRegistry {
    fetch_timestamp: Instant,
    pub latest_version: PackageVersion,
    pub package_versions: Vec<PackageVersion>,
    pub failed_versions: Vec<String>,
}

#[derive(Clone)]
pub(super) struct PackageVersion {
    pub version: semver_rs::Version,
    pub description: String,
    pub homepage: Option<String>,
    pub date: DateTime<FixedOffset>,
}

async fn fetch(
    client: &reqwest::Client,
    npm_config: &NpmConfig,
    package_name: &str,
    fetch_options: FetchOptions,
) -> Option<MetadataFromRegistry> {
    let url = npm_config.package_url(package_name)?;
    let mut request = client.get(url.clone());
    if let Some(auth_token) = npm_config.auth_token_for(&url) {
        request = request.bearer_auth(auth_token);
    }
    let response = request.send().await.ok()?.json::<Value>().await.ok()?;
    let latest_version_str = response["dist-tags"]["latest"].as_str()?;
    let latest_version = parse_version_info(&response, &response["versions"][latest_version_str])?;

    let (package_versions, failed_versions) = if fetch_options.parse_all_versions {
        response["versions"].as_object()?.into_iter().partition_map(
            |(version_name, version_info)| {
                if let Some(parsed_version_info) = parse_version_info(&response, version_info) {
                    Either::Left(parsed_version_info)
                } else {
                    Either::Right(version_name.clone())
                }
            },
        )
    } else {
        (vec![], vec![])
    };

    Some(MetadataFromRegistry {
        fetch_timestamp: Instant::now(),
        latest_version,
        package_versions,
        failed_versions,
    })
}

fn parse_version_info(response: &Value, version_info: &Value) -> Option<PackageVersion> {
    let version_str = version_info["version"].as_str()?;
    let version = semver_rs::Version::parse(
        version_str,
        Some(semver_rs::Options {
            loose: true,
            include_prerelease: true,
        }),
    )
    .ok()?;
    let description = version_info["description"].as_str()?.to_string();
    let homepage = version_info["homepage"].as_str().map(ToString::to_string);
    let date_str = response["time"][version_str].as_str()?;
    let date = DateTime::parse_from_rfc3339(date_str).ok()?;
    Some(PackageVersion {
        version,
        description,
        homepage,
        date,
    })
}
