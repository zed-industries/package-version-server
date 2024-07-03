use std::{
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, FixedOffset};
use reqwest::Client;
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
    cache: Arc<Mutex<HashMap<PackageName, MetadataFromRegistry>>>,
}

/// How long do we keep data about a package around before requerying it the second time.
const REFRESH_DURATION: Duration = Duration::from_secs(30);

impl PackageVersionFetcher {
    pub(super) fn new() -> reqwest::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .build()?;
        Ok(Self {
            client,
            cache: Default::default(),
        })
    }
    pub(super) async fn get(&self, package_name: &str) -> Option<MetadataFromRegistry> {
        {
            let lock = self.cache.lock().await;
            let cached_entry = lock.get(package_name);
            if let Some(cached_entry) = cached_entry {
                if cached_entry.fetch_timestamp + REFRESH_DURATION > std::time::Instant::now() {
                    return Some(cached_entry.clone());
                }
            }
        }
        let latest_version = fetch_latest_version(&self.client, package_name).await?;
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

#[derive(Clone)]
pub(super) struct MetadataFromRegistry {
    fetch_timestamp: Instant,
    pub version: String,
    pub description: String,
    pub homepage: String,
    pub date: DateTime<FixedOffset>,
}

async fn fetch_latest_version(
    client: &reqwest::Client,
    package_name: &str,
) -> Option<MetadataFromRegistry> {
    let package_name = urlencoding::encode(package_name);
    let url = format!("https://registry.npmjs.org/{}", package_name);
    let resp = client
        .get(url)
        .send()
        .await
        .ok()?
        .json::<Value>()
        .await
        .ok()?;
    let version = resp["dist-tags"]["latest"].as_str()?;
    let version_info = &resp["versions"][version];
    let version_str = version_info["version"].as_str()?.to_string();
    let description = version_info["description"].as_str()?.to_string();
    let homepage = version_info["homepage"].as_str()?.to_string();
    let date_str = resp["time"][version].as_str()?;
    let date = DateTime::parse_from_rfc3339(date_str).ok()?;
    Some(MetadataFromRegistry {
        fetch_timestamp: Instant::now(),
        version: version_str,
        description,
        homepage,
        date,
    })
}
