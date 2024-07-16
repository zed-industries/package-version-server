use std::sync::Arc;

use tower_lsp::lsp_types::{Position, Range};
use tree_sitter::{Parser, Point, Query, QueryCursor};
use tree_sitter_json::language;

pub fn extract_package_name(text: Arc<str>, position: Position) -> Option<(String, Range)> {
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
                        value: (string (string_content) @version)
                    ) @_dep_specifier
                )+
            (#any-of? @root_name "dependencies" "devDependencies" "peerDependencies" "optionalDependencies" "bundledDependencies" "bundleDependencies")
        )+
    "#;

    let query = Query::new(&language(), query_str).ok()?;
    let mut cursor = QueryCursor::new();

    let root_node = tree.root_node();
    let matches = cursor.matches(&query, root_node, text.as_bytes());
    let capture_names = query.capture_names();
    for m in matches {
        let mut package_name = None;
        let mut match_range = None;
        for capture in m.captures {
            let capture_name = capture_names[capture.index as usize];
            if capture_name == "name" {
                package_name = Some(capture.node.utf8_text(text.as_bytes()).ok()?.to_string());
            } else if capture_name == "root_name" {
                continue;
            }
            let node_range = capture.node.range();
            if node_range.start_point <= point && node_range.end_point >= point {
                if capture_name != "name" && capture_name != "version" {
                    continue;
                }
                match_range = Some(Range {
                    start: Position {
                        line: node_range.start_point.row as u32,
                        character: node_range.start_point.column as u32,
                    },
                    end: Position {
                        line: node_range.end_point.row as u32,
                        character: node_range.end_point.column as u32,
                    },
                });
            }
        }
        if let Some(match_range) = match_range {
            return package_name.map(|name| (name, match_range));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp::lsp_types::Position;

    #[test]
    fn test_parse_package_json() {
        let package = r#"{
  "dependencies": {
    "express": "^4.17.1"
  }
}
"#;
        assert_eq!(
            extract_package_name(
                package.into(),
                Position {
                    line: 2,
                    character: 11,
                },
            ),
            Some((
                "express".into(),
                Range {
                    start: Position {
                        line: 2,
                        character: 5,
                    },
                    end: Position {
                        line: 2,
                        character: 12,
                    },
                }
            ))
        );
    }
}
