mod fetcher;
mod parser;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, FixedOffset};
use chrono_humanize::{Accuracy, HumanTime, Tense};
use fetcher::{FetchOptions, PackageVersionFetcher};
use itertools::Itertools;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

struct Backend {
    client: Client,
    file_contents: Arc<Mutex<HashMap<Url, Arc<str>>>>,
    fetcher: PackageVersionFetcher,
}

impl Backend {
    fn new(lsp_client: Client) -> Result<Self> {
        Ok(Self {
            client: lsp_client,
            file_contents: Default::default(),
            fetcher: PackageVersionFetcher::new()
                .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?,
        })
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
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![String::from(".")]),
                    ..Default::default()
                }),
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

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().next() {
            self.file_contents
                .lock()
                .unwrap()
                .insert(params.text_document.uri, change.text.into());
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.file_contents
            .lock()
            .unwrap()
            .insert(params.text_document.uri, params.text_document.text.into());
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;

        if !uri.path().ends_with("package.json") {
            return Ok(None);
        }
        let Some(document) = self.file_contents.lock().unwrap().get(&uri).cloned() else {
            return Ok(None);
        };

        let Some((package_name, range, _)) =
            parser::extract_package_name(document, params.text_document_position_params.position)
        else {
            return Ok(None);
        };

        let response = self
            .fetcher
            .get(
                &package_name,
                FetchOptions {
                    parse_all_versions: false,
                },
            )
            .await
            .ok_or_else(tower_lsp::jsonrpc::Error::internal_error)?;
        let offset = format_time(response.latest_version.date);

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: format!(
                    "**{package_name}**\n\n{}\n\nLatest version: {} (published {offset})\n\n[{2}]({2})",
                    response.latest_version.description, response.latest_version.version, response.latest_version.homepage,
                ),
            }),
            range: Some(range),
        }))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;

        if !uri.path().ends_with("package.json") {
            return Ok(None);
        }
        let Some(document) = self.file_contents.lock().unwrap().get(&uri).cloned() else {
            return Ok(None);
        };

        let Some((package_name, _, Some(version))) =
            parser::extract_package_name(document, params.text_document_position.position)
        else {
            return Ok(None);
        };

        let response = self
            .fetcher
            .get(
                &package_name,
                FetchOptions {
                    parse_all_versions: true,
                },
            )
            .await
            .ok_or_else(tower_lsp::jsonrpc::Error::internal_error)?;

        if !response.failed_versions.is_empty() {
            let some_or_all = if response.package_versions.is_empty() {
                "all"
            } else {
                "some"
            };
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!(
                        "Failed to parse {} package versions: {:?}",
                        some_or_all, response.failed_versions
                    ),
                )
                .await;
        }

        let mut completion_items: Vec<_> = response
            .package_versions
            .into_iter()
            .filter_map(|package_version| {
                if package_version.version.starts_with(&version) {
                    Some(CompletionItem {
                        label: package_version.version,
                        ..Default::default()
                    })
                } else {
                    None
                }
            })
            .collect();
        completion_items
            .sort_by(|lhs_version, rhs_version| rhs_version.label.cmp(&lhs_version.label));
        Ok(Some(CompletionResponse::Array(completion_items)))
    }
}

fn format_time(time: DateTime<FixedOffset>) -> String {
    let ht = HumanTime::from(time);
    ht.to_text_en(Accuracy::Rough, Tense::Past)
}

#[tokio::main]
async fn main() {
    if std::env::args()
        .nth(1)
        .filter(|arg| arg == "--version")
        .is_some()
    {
        println!("package-version-server {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) =
        LspService::new(|client| Backend::new(client).expect("Failed to initialize backend"));
    Server::new(stdin, stdout, socket).serve(service).await;
}
