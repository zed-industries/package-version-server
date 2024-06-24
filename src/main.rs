use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, FixedOffset};
use chrono_humanize::{Accuracy, HumanTime, Tense};
use regex::Regex;
use serde_json::{self, Value};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use tree_sitter::{Parser, Point, Query, QueryCursor};
use tree_sitter_json::language;

struct Backend {
    client: Client,
    file_contents: Arc<Mutex<HashMap<Url, Arc<str>>>>,
}

impl Backend {
    fn new(client: Client) -> Result<Self> {
        Ok(Self {
            client,
            file_contents: Default::default(),
        })
    }
    fn extract_package_name(text: Arc<str>, position: Position) -> Option<String> {
        let mut parser = Parser::new();
        parser.set_language(&language()).ok()?;

        let tree = parser.parse(text.as_bytes(), None)?;
        let point = Point {
            row: position.line as usize,
            column: position.character as usize,
        };

        let query_str = r#"
            (pair
                key: (string (string_content) @root_name)
                value:
                    (object
                        (pair
                            key: (string (string_content) @name)
                            value: (string)
                        )
                    )
                (#any-of? @root_name "dependencies" "devDependencies" "peerDependencies" "optionalDependencies" "bundledDependencies" "bundleDependencies")
            )+
        "#;

        let query = Query::new(&language(), query_str).ok()?;
        let mut cursor = QueryCursor::new();

        let root_node = tree.root_node();
        let matches = cursor.matches(&query, root_node, text.as_bytes());
        let mut package_name = None;
        let capture_names = query.capture_names();
        for m in matches {
            package_name.take();
            let mut matched = false;
            for capture in m.captures {
                let capture_name = capture_names[capture.index as usize];
                if capture_name == "name" {
                    package_name = Some(capture.node.utf8_text(text.as_bytes()).ok()?.to_string());
                }
                let node_range = capture.node.range();
                if node_range.start_point <= point && node_range.end_point >= point {
                    matched = true;
                }
            }
            if matched {
                return package_name;
            }
        }

        package_name
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

        let package_name_pair =
            Self::extract_package_name(document, params.text_document_position_params.position);

        let Some(package_name) = package_name_pair else {
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
    let package_name = urlencoding::encode(package_name);
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

#[cfg(test)]
mod tests {
    use tower_lsp::lsp_types::Position;

    use crate::Backend;

    #[test]
    fn test_parse_package_json() {
        let package = r#"{
  "dependencies": {
    "express": "^4.17.1"
  }
}
"#;
        assert_eq!(
            Backend::extract_package_name(
                package.into(),
                Position {
                    line: 2,
                    character: 11,
                },
            ),
            Some("express".into())
        );
    }
}
