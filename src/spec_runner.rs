/// A parsed spec failure from `crystal spec` output.
#[derive(Debug)]
pub struct SpecFailure {
    pub description: String,
    pub file: String,
    pub line: u32,
    pub expected: String,
    pub actual: String,
    pub raw_message: String,
}

/// Summary of a spec run.
#[derive(Debug)]
pub struct SpecResult {
    pub failures: Vec<SpecFailure>,
    pub total: u32,
    pub failed: u32,
    pub errored: u32,
    pub pending: u32,
    pub duration: String,
}

/// Parses the text output of `crystal spec --no-color`.
///
/// Expected format:
/// ```text
/// Failures:
///
///   1) ClassName#method should do something
///      Failure/Error: ...
///
///        Expected: "foo"
///             Got: "bar"
///
///      # spec/foo_spec.cr:15
/// ```
pub fn parse_spec_output(output: &str) -> SpecResult {
    let mut failures = Vec::new();
    let lines: Vec<&str> = output.lines().collect();

    let failures_start = lines.iter().position(|l| l.trim() == "Failures:");

    if let Some(start) = failures_start {
        let mut i = start + 1;
        while i < lines.len() {
            let trimmed = lines[i].trim();

            // Detect numbered failure: "1) description..."
            if is_failure_header(trimmed) {
                let description = trimmed
                    .splitn(2, ')')
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_string();

                let mut expected = String::new();
                let mut actual = String::new();
                let mut file = String::new();
                let mut line_num: u32 = 0;
                let mut raw_lines: Vec<String> = Vec::new();

                i += 1;
                // Scan until the next numbered failure or end of failures section
                while i < lines.len() {
                    let inner = lines[i].trim();

                    if is_failure_header(inner) || inner.starts_with("Finished in") {
                        break;
                    }

                    if inner.starts_with("Expected:") {
                        expected = inner.trim_start_matches("Expected:").trim().to_string();
                    } else if inner.starts_with("Got:") || inner.starts_with("got:") {
                        actual = inner
                            .trim_start_matches("Got:")
                            .trim_start_matches("got:")
                            .trim()
                            .to_string();
                    } else if inner.starts_with('#') && inner.contains(':') {
                        // # spec/foo_spec.cr:15
                        let location = inner.trim_start_matches('#').trim();
                        if let Some(colon_pos) = location.rfind(':') {
                            file = location[..colon_pos].to_string();
                            line_num = location[colon_pos + 1..]
                                .parse()
                                .unwrap_or(0);
                        }
                    } else if !inner.is_empty()
                        && !inner.starts_with("Failure/Error:")
                    {
                        raw_lines.push(inner.to_string());
                    }

                    i += 1;
                }

                let raw_message = if expected.is_empty() && actual.is_empty() {
                    raw_lines.join("\n")
                } else {
                    format!("Expected: {}\n     Got: {}", expected, actual)
                };

                failures.push(SpecFailure {
                    description,
                    file,
                    line: line_num,
                    expected,
                    actual,
                    raw_message,
                });

                continue; // don't increment i, the while loop already advanced
            }

            i += 1;
        }
    }

    let (total, failed, errored, pending, duration) = parse_summary(&lines);

    SpecResult {
        failures,
        total,
        failed,
        errored,
        pending,
        duration,
    }
}

fn is_failure_header(line: &str) -> bool {
    let Some(first_char) = line.chars().next() else {
        return false;
    };
    if !first_char.is_ascii_digit() {
        return false;
    }
    line.contains(") ")
}

/// Parses the summary line: "5 examples, 2 failures, 1 error, 1 pending"
fn parse_summary(lines: &[&str]) -> (u32, u32, u32, u32, String) {
    let mut total = 0u32;
    let mut failed = 0u32;
    let mut errored = 0u32;
    let mut pending = 0u32;
    let mut duration = String::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with("Finished in") {
            duration = trimmed
                .trim_start_matches("Finished in")
                .trim()
                .to_string();
        }
        if trimmed.contains("example") && trimmed.contains("failure") {
            for part in trimmed.split(',') {
                let part = part.trim();
                if part.contains("example") {
                    total = extract_leading_number(part);
                } else if part.contains("failure") {
                    failed = extract_leading_number(part);
                } else if part.contains("error") {
                    errored = extract_leading_number(part);
                } else if part.contains("pending") {
                    pending = extract_leading_number(part);
                }
            }
        }
    }

    (total, failed, errored, pending, duration)
}

fn extract_leading_number(s: &str) -> u32 {
    s.trim()
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Formats spec failures as inline annotations keyed by file:line.
pub fn format_inline_annotations(result: &SpecResult) -> String {
    if result.failures.is_empty() {
        let mut s = format!(
            "All specs passed ({} examples",
            result.total
        );
        if !result.duration.is_empty() {
            s.push_str(&format!(", {}", result.duration));
        }
        s.push(')');
        return s;
    }

    let mut text = String::new();

    for (i, f) in result.failures.iter().enumerate() {
        // Header: file:line | FAIL: description
        if !f.file.is_empty() && f.line > 0 {
            text.push_str(&format!("{}:{} | FAIL: {}\n", f.file, f.line, f.description));
        } else {
            text.push_str(&format!("FAIL: {}\n", f.description));
        }

        // Expected vs actual diff
        if !f.expected.is_empty() || !f.actual.is_empty() {
            text.push_str(&format!("  Expected: {}\n", f.expected));
            text.push_str(&format!("       Got: {}\n", f.actual));
        } else if !f.raw_message.is_empty() {
            for raw_line in f.raw_message.lines() {
                text.push_str(&format!("  {}\n", raw_line));
            }
        }

        if i < result.failures.len() - 1 {
            text.push('\n');
        }
    }

    // Summary
    text.push_str(&format!(
        "\n{} examples, {} failures",
        result.total, result.failed
    ));
    if result.errored > 0 {
        text.push_str(&format!(", {} errors", result.errored));
    }
    if result.pending > 0 {
        text.push_str(&format!(", {} pending", result.pending));
    }
    if !result.duration.is_empty() {
        text.push_str(&format!(" ({})", result.duration));
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_passing() {
        let output = "..\n\nFinished in 0.5 seconds\n2 examples, 0 failures\n";
        let result = parse_spec_output(output);
        assert!(result.failures.is_empty());
        assert_eq!(result.total, 2);
        assert_eq!(result.failed, 0);
    }

    #[test]
    fn test_parse_single_failure() {
        let output = r#"F

Failures:

  1) Foo should return bar
     Failure/Error: foo.should eq("bar")

       Expected: "bar"
            Got: "baz"

     # spec/foo_spec.cr:5

Finished in 0.3 seconds
1 example, 1 failure
"#;
        let result = parse_spec_output(output);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].description, "Foo should return bar");
        assert_eq!(result.failures[0].file, "spec/foo_spec.cr");
        assert_eq!(result.failures[0].line, 5);
        assert_eq!(result.failures[0].expected, "\"bar\"");
        assert_eq!(result.failures[0].actual, "\"baz\"");
        assert_eq!(result.total, 1);
        assert_eq!(result.failed, 1);
    }

    #[test]
    fn test_parse_multiple_failures() {
        let output = r#"FF.

Failures:

  1) A should work
     Failure/Error: a.should eq(1)

       Expected: 1
            Got: 2

     # spec/a_spec.cr:3

  2) B should also work
     Failure/Error: b.should eq("x")

       Expected: "x"
            Got: "y"

     # spec/b_spec.cr:10

Finished in 1.2 seconds
3 examples, 2 failures
"#;
        let result = parse_spec_output(output);
        assert_eq!(result.failures.len(), 2);
        assert_eq!(result.failures[0].file, "spec/a_spec.cr");
        assert_eq!(result.failures[0].line, 3);
        assert_eq!(result.failures[1].file, "spec/b_spec.cr");
        assert_eq!(result.failures[1].line, 10);
        assert_eq!(result.total, 3);
        assert_eq!(result.failed, 2);
    }

    #[test]
    fn test_format_annotations_passing() {
        let result = SpecResult {
            failures: vec![],
            total: 5,
            failed: 0,
            errored: 0,
            pending: 0,
            duration: "0.1 seconds".to_string(),
        };
        let text = format_inline_annotations(&result);
        assert_eq!(text, "All specs passed (5 examples, 0.1 seconds)");
    }

    #[test]
    fn test_format_annotations_with_failure() {
        let result = SpecResult {
            failures: vec![SpecFailure {
                description: "should work".to_string(),
                file: "spec/foo_spec.cr".to_string(),
                line: 5,
                expected: "42".to_string(),
                actual: "0".to_string(),
                raw_message: "Expected: 42\n     Got: 0".to_string(),
            }],
            total: 1,
            failed: 1,
            errored: 0,
            pending: 0,
            duration: "0.5 seconds".to_string(),
        };
        let text = format_inline_annotations(&result);
        assert!(text.contains("spec/foo_spec.cr:5 | FAIL: should work"));
        assert!(text.contains("Expected: 42"));
        assert!(text.contains("Got: 0"));
    }
}
