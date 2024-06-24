use chrono::{DateTime, FixedOffset};
use chrono_humanize::{Accuracy, HumanTime, Tense};
use regex::Regex;
use serde_json::{self, Value};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

struct Backend {
    client: Client,
    package_lock_version_regex: Regex,
}

impl Backend {
    fn new(client: Client) -> Result<Self> {
        Ok(Self {
            client,
            package_lock_version_regex: Regex::new(r#""([\w-]+)":\s*"([\w.-]+)""#)
                .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?,
        })
    }

    fn extract_package_name_and_version(
        &self,
        document: &str,
        position: Position,
    ) -> Option<(String, String)> {
        let lines: Vec<&str> = document.lines().collect();
        let line = lines.get(position.line as usize)?;

        if let Some(matches) = self.package_lock_version_regex.captures(line) {
            if matches.len() > 2 {
                let name = matches.get(1)?.as_str().to_string();
                let version = matches.get(2)?.as_str().to_string();
                return Some((name, version));
            }
        }
        None
    }
}
#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Language server initialized.")
            .await;
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;

        if !uri.path().ends_with("package.json") {
            return Ok(None);
        }
        let document = std::fs::read_to_string(uri.to_file_path().unwrap()).unwrap();
        let package_name_pair = self.extract_package_name_and_version(
            &document,
            params.text_document_position_params.position,
        );

        let Some((package_name, _)) = package_name_pair else {
            return Ok(None);
        };
        let meta = fetch_latest_version(&package_name)
            .await
            .ok_or_else(tower_lsp::jsonrpc::Error::internal_error)?;
        let offset = format_time(meta.date);
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!(
                    "**{package_name}**\n\n{}\n\nLatest version: {} (published {offset})\n\n[{2}]({2})",
                    meta.description, meta.version, meta.homepage,
                ),
            }),
            range: None,
        }))
    }
}

fn format_time(time: DateTime<FixedOffset>) -> String {
    let ht = HumanTime::from(time);
    ht.to_text_en(Accuracy::Rough, Tense::Past)
}
struct MetadataFromRegistry {
    version: String,
    description: String,
    homepage: String,
    date: DateTime<FixedOffset>,
}

async fn fetch_latest_version(package_name: &str) -> Option<MetadataFromRegistry> {
    let url = format!("https://registry.npmjs.org/{}", package_name);
    let resp = reqwest::get(url).await.ok()?.json::<Value>().await.ok()?;
    let version = resp["dist-tags"]["latest"].as_str()?;
    let version_info = &resp["versions"][version];
    let version_str = version_info["version"].as_str()?.to_string();
    let description = version_info["description"].as_str()?.to_string();
    let homepage = version_info["homepage"].as_str()?.to_string();
    let date_str = resp["time"][version].as_str()?;
    let date = DateTime::parse_from_rfc3339(date_str).ok()?;
    Some(MetadataFromRegistry {
        version: version_str,
        description,
        homepage,
        date,
    })
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) =
        LspService::new(|client| Backend::new(client).expect("Failed to initialize backend"));
    Server::new(stdin, stdout, socket).serve(service).await;
}
