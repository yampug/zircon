#[cfg(test)]
mod tests {
    use crate::analyzer::Analyzer;
    use tree_sitter::Parser;
    use tree_sitter_crystal;

    fn analyze(source: &str) -> Analyzer {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_crystal::LANGUAGE.into()).expect("Error loading Crystal grammar");
        let tree = parser.parse(source, None).unwrap();
        let mut analyzer = Analyzer::new();
        analyzer.analyze(&tree, source.as_bytes());
        analyzer
    }

    #[test]
    fn test_literal_inference() {
        let source = r#"
            a = 1
            b = "hello"
            c = true
            d = :symbol
            e = [1, 2, 3]
            f = {"key" => 1.5}
            g = {1, "a"}
        "#;
        let analyzer = analyze(source);
        let completions = analyzer.completions();
        
        let mut map = std::collections::HashMap::new();
        for (name, t) in completions {
            map.insert(name, t);
        }

        assert_eq!(map.get("a").unwrap(), "Int32");
        assert_eq!(map.get("b").unwrap(), "String");
        assert_eq!(map.get("c").unwrap(), "Bool");
        assert_eq!(map.get("d").unwrap(), "Symbol");
        assert_eq!(map.get("e").unwrap(), "Array(Int32)");
        assert_eq!(map.get("f").unwrap(), "Hash(String, Float64)");
        assert_eq!(map.get("g").unwrap(), "{Int32, String}");
    }

    #[test]
    fn test_variable_reassignment() {
        let source = r#"
            a = 1
            a = "string"
            b = a
        "#;
        let analyzer = analyze(source);
        let completions = analyzer.completions();
        
        let mut map = std::collections::HashMap::new();
        for (name, t) in completions {
            map.insert(name, t);
        }

        assert_eq!(map.get("a").unwrap(), "String");
        assert_eq!(map.get("b").unwrap(), "String");
    }

    #[test]
    fn test_scope_inference() {
        let source = r#"
            a = 1
            def method
              a = "inner"
            end
            c = a
        "#;
        let analyzer = analyze(source);
        
        let completions = analyzer.completions();
        let mut map = std::collections::HashMap::new();
        for (name, t) in completions {
            map.insert(name, t);
        }
        
        // At the end of analysis, only root scope variables are in analyzer.scopes
        // 'a' was 1 in root scope.
        // 'c' was assigned 'a' which was 1.
        assert_eq!(map.get("a").unwrap(), "Int32");
        assert_eq!(map.get("c").unwrap(), "Int32");
    }
}
