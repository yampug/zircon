#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

    const TAGS_QUERY: &str = include_str!("../languages/crystal/tags.scm");
    const OUTLINE_QUERY: &str = include_str!("../languages/crystal/outline.scm");

    fn parse(source: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_crystal::LANGUAGE.into())
            .expect("Error loading Crystal grammar");
        parser.parse(source, None).unwrap()
    }

    fn run_query(query_src: &str, source: &str) -> Vec<(String, String)> {
        let lang = tree_sitter_crystal::LANGUAGE.into();
        let query = Query::new(&lang, query_src).expect("Invalid query");
        let tree = parse(source);
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

        let mut results = Vec::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let cap_name = &query.capture_names()[cap.index as usize];
                if *cap_name == "name" {
                    let text = cap
                        .node
                        .utf8_text(source.as_bytes())
                        .unwrap_or("")
                        .to_string();
                    let tag = m
                        .captures
                        .iter()
                        .find_map(|c| {
                            let n = &query.capture_names()[c.index as usize];
                            if n.starts_with("definition.")
                                || n.starts_with("reference.")
                                || *n == "item"
                            {
                                Some(n.to_string())
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default();
                    results.push((tag, text));
                }
            }
        }
        results
    }

    // -- Tags tests using the monorepo fixture content --

    const APP_CR: &str = include_str!("../fixtures/monorepo/backend/src/app.cr");
    const USER_CR: &str = include_str!("../fixtures/monorepo/backend/src/models/user.cr");
    const POST_CR: &str = include_str!("../fixtures/monorepo/backend/src/models/post.cr");

    #[test]
    fn test_tags_class_module_definitions() {
        let results = run_query(TAGS_QUERY, APP_CR);
        let defs: HashSet<(&str, &str)> = results
            .iter()
            .filter(|(tag, _)| tag.starts_with("definition."))
            .map(|(t, n)| (t.as_str(), n.as_str()))
            .collect();

        assert!(defs.contains(&("definition.module", "App")));
        assert!(defs.contains(&("definition.class", "Server")));
        assert!(defs.contains(&("definition.constant", "VERSION")));
    }

    #[test]
    fn test_tags_method_definitions() {
        let results = run_query(TAGS_QUERY, APP_CR);
        let methods: Vec<&str> = results
            .iter()
            .filter(|(tag, _)| tag == "definition.method")
            .map(|(_, n)| n.as_str())
            .collect();

        assert!(methods.contains(&"initialize"));
        assert!(methods.contains(&"start"));
        assert!(methods.contains(&"handle_request")); // abstract def
        assert!(methods.contains(&"instance_count"));
        assert!(methods.contains(&"host")); // property macro
    }

    #[test]
    fn test_tags_instance_and_class_vars() {
        let results = run_query(TAGS_QUERY, APP_CR);
        let fields: Vec<&str> = results
            .iter()
            .filter(|(tag, _)| tag == "definition.field")
            .map(|(_, n)| n.as_str())
            .collect();

        assert!(fields.contains(&"@port"));
        assert!(fields.contains(&"@@instance_count"));
    }

    #[test]
    fn test_tags_ivar_references() {
        let results = run_query(TAGS_QUERY, USER_CR);
        let refs: Vec<&str> = results
            .iter()
            .filter(|(tag, _)| tag == "reference.field")
            .map(|(_, n)| n.as_str())
            .collect();

        assert!(refs.contains(&"@name"));
        assert!(refs.contains(&"@active"));
    }

    #[test]
    fn test_tags_require_paths() {
        let results = run_query(TAGS_QUERY, APP_CR);
        let requires: Vec<&str> = results
            .iter()
            .filter(|(tag, name)| tag == "reference.call" && name.starts_with("./"))
            .map(|(_, n)| n.as_str())
            .collect();

        assert!(requires.contains(&"./models/user"));
        assert!(requires.contains(&"./models/post"));
    }

    #[test]
    fn test_tags_alias_and_constant() {
        let results = run_query(TAGS_QUERY, POST_CR);
        let defs: HashSet<(&str, &str)> = results
            .iter()
            .filter(|(tag, _)| tag.starts_with("definition."))
            .map(|(t, n)| (t.as_str(), n.as_str()))
            .collect();

        assert!(defs.contains(&("definition.constant", "MAX_TITLE_LENGTH")));
        assert!(defs.contains(&("definition.type", "Tags")));
        assert!(defs.contains(&("definition.class", "Post")));
    }

    #[test]
    fn test_tags_getter_macro() {
        let results = run_query(TAGS_QUERY, USER_CR);
        let methods: Vec<&str> = results
            .iter()
            .filter(|(tag, _)| tag == "definition.method")
            .map(|(_, n)| n.as_str())
            .collect();

        assert!(methods.contains(&"name"));   // getter name : String
        assert!(methods.contains(&"active")); // getter? active : Bool
    }

    // -- Outline tests --

    #[test]
    fn test_outline_class_and_methods() {
        let results = run_query(OUTLINE_QUERY, APP_CR);
        let items: Vec<&str> = results
            .iter()
            .filter(|(tag, _)| tag == "item")
            .map(|(_, n)| n.as_str())
            .collect();

        assert!(items.contains(&"App"));
        assert!(items.contains(&"Server"));
        assert!(items.contains(&"initialize"));
        assert!(items.contains(&"start"));
        assert!(items.contains(&"handle_request")); // abstract def
        assert!(items.contains(&"instance_count"));
        assert!(items.contains(&"VERSION"));
    }

    #[test]
    fn test_outline_alias_and_constant() {
        let results = run_query(OUTLINE_QUERY, POST_CR);
        let items: Vec<&str> = results
            .iter()
            .filter(|(tag, _)| tag == "item")
            .map(|(_, n)| n.as_str())
            .collect();

        assert!(items.contains(&"Post"));
        assert!(items.contains(&"MAX_TITLE_LENGTH"));
        assert!(items.contains(&"Tags")); // alias
    }

    #[test]
    fn test_outline_enum_members() {
        let source = "enum Color\n  Red\n  Green\n  Blue\nend\n";
        let results = run_query(OUTLINE_QUERY, source);
        let items: Vec<&str> = results
            .iter()
            .filter(|(tag, _)| tag == "item")
            .map(|(_, n)| n.as_str())
            .collect();

        assert!(items.contains(&"Color"));
        assert!(items.contains(&"Red"));
        assert!(items.contains(&"Green"));
        assert!(items.contains(&"Blue"));
    }

    #[test]
    fn test_outline_annotation_and_fun() {
        let source = "annotation MyAnnotation\nend\n\nlib LibC\n  fun printf(format : UInt8*, ...) : Int32\nend\n";
        let results = run_query(OUTLINE_QUERY, source);
        let items: Vec<&str> = results
            .iter()
            .filter(|(tag, _)| tag == "item")
            .map(|(_, n)| n.as_str())
            .collect();

        assert!(items.contains(&"MyAnnotation"));
        assert!(items.contains(&"LibC"));
        assert!(items.contains(&"printf"));
    }
}
