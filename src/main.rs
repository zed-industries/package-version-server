mod fetcher;
mod parser;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, FixedOffset};
use chrono_humanize::{Accuracy, HumanTime, Tense};
use fetcher::{FetchOptions, PackageVersionFetcher};
use parser::ParseResult;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use tree_sitter::Parser;
use tree_sitter_json::language;

struct Backend {
    client: Client,
    file_contents: Arc<Mutex<HashMap<Url, (Arc<str>, tree_sitter::Tree)>>>,
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
    fn get_parser() -> Parser {
        let mut parser = Parser::new();
        parser.set_language(&language()).unwrap();

        parser
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
            let mut parser = Self::get_parser();
            let text: Arc<str> = change.text.into();
            self.file_contents
                .lock()
                .unwrap()
                .entry(params.text_document.uri)
                .and_modify(|(contents, parse_tree)| {
                    let new_parse_tree = parser
                        .parse(text.as_bytes(), None)
                        .expect("We should always get a new parse tree.");
                    *contents = text.clone();
                    *parse_tree = new_parse_tree;
                })
                .or_insert_with(|| {
                    let parse_tree = parser
                        .parse(text.as_bytes(), None)
                        .expect("We should always get a new parse tree.");
                    (text, parse_tree)
                });
        }
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let mut parser = Self::get_parser();
        let text: Arc<str> = params.text_document.text.into();
        self.file_contents
            .lock()
            .unwrap()
            .entry(params.text_document.uri)
            .and_modify(|(contents, parse_tree)| {
                let new_parse_tree = parser
                    .parse(text.as_bytes(), None)
                    .expect("We should always get a new parse tree.");
                *contents = text.clone();
                *parse_tree = new_parse_tree;
            })
            .or_insert_with(|| {
                let parse_tree = parser
                    .parse(text.as_bytes(), None)
                    .expect("We should always get a new parse tree.");
                (text, parse_tree)
            });
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;

        if !uri.path().ends_with("package.json") {
            return Ok(None);
        }
        let Some((contents, parse_tree)) = self.file_contents.lock().unwrap().get(&uri).cloned()
        else {
            return Ok(None);
        };

        let Some(ParseResult {
            package_name,
            match_range,
            ..
        }) = parser::extract_package_name(
            contents,
            parse_tree,
            params.text_document_position_params.position,
        )
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
        let mut description = format!(
            "**{package_name}**\n\n{}\n\nLatest version: {} (published {offset})\n\n",
            response.latest_version.description, response.latest_version.version
        );
        if let Some(homepage) = response.latest_version.homepage {
            use std::fmt::Write;
            write!(&mut description, "[{0}]({0})", homepage).ok();
        }
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: description,
            }),
            range: Some(match_range),
        }))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;

        if !uri.path().ends_with("package.json") {
            return Ok(None);
        }
        let Some((contents, parse_tree)) = self.file_contents.lock().unwrap().get(&uri).cloned()
        else {
            return Ok(None);
        };

        let Some(ParseResult {
            package_name,
            version,
            ..
        }) = parser::extract_package_name(
            contents,
            parse_tree,
            params.text_document_position.position,
        )
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
                        label: package_version.version.clone(),
                        detail: Some(package_version.date.format("%d/%m/%Y %H:%M").to_string()),
                        insert_text: Some(package_version.version.clone()),
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
