use crate::traits::{DeliveryProcessor, VdpAction};

/// Default maximum ESI recursion depth.
const DEFAULT_MAX_DEPTH: u32 = 5;

/// Represents a parsed fragment from ESI-processed content.
///
/// The body is decomposed into a sequence of fragments. Literal fragments
/// contain raw bytes that pass through unchanged. Include fragments represent
/// `<esi:include>` tags whose `src` attribute must be resolved by the runtime
/// via subrequests. Remove variants mark content that was stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsiFragment {
    /// Raw content that passes through unchanged.
    Literal(Vec<u8>),
    /// An `<esi:include src="..."/>` tag. The runtime must resolve this URL.
    Include { src: String },
    /// Content inside `<esi:remove>...</esi:remove>` that was stripped.
    Remove,
}

/// Parse a body byte slice into a sequence of ESI fragments.
///
/// Recognized tags:
/// - `<esi:include src="..." />` -- produces `EsiFragment::Include`
/// - `<esi:remove>...</esi:remove>` -- produces `EsiFragment::Remove`
/// - `<esi:comment text="..." />` -- stripped silently (no fragment emitted)
///
/// Everything outside of recognized ESI tags becomes `EsiFragment::Literal`.
/// Malformed or unrecognized tags are passed through as literal content.
pub fn parse_esi(body: &[u8]) -> Vec<EsiFragment> {
    let mut fragments: Vec<EsiFragment> = Vec::new();
    let mut pos = 0;
    let len = body.len();

    while pos < len {
        // Look for the next '<esi:' opening
        match find_bytes(body, b"<esi:", pos) {
            None => {
                // No more ESI tags; rest is literal
                push_literal(&mut fragments, &body[pos..]);
                break;
            }
            Some(tag_start) => {
                // Emit any literal content before this tag
                if tag_start > pos {
                    push_literal(&mut fragments, &body[pos..tag_start]);
                }

                // Determine which ESI tag this is
                let after_prefix = tag_start + b"<esi:".len();

                if body[after_prefix..].starts_with(b"include") {
                    match parse_esi_include(body, tag_start) {
                        Some((src, end_pos)) => {
                            fragments.push(EsiFragment::Include { src });
                            pos = end_pos;
                        }
                        None => {
                            // Malformed include tag -- emit as literal
                            push_literal(&mut fragments, &body[tag_start..tag_start + 1]);
                            pos = tag_start + 1;
                        }
                    }
                } else if body[after_prefix..].starts_with(b"remove") {
                    match parse_esi_remove(body, tag_start) {
                        Some(end_pos) => {
                            fragments.push(EsiFragment::Remove);
                            pos = end_pos;
                        }
                        None => {
                            push_literal(&mut fragments, &body[tag_start..tag_start + 1]);
                            pos = tag_start + 1;
                        }
                    }
                } else if body[after_prefix..].starts_with(b"comment") {
                    match parse_esi_comment(body, tag_start) {
                        Some(end_pos) => {
                            // Comments are silently stripped -- no fragment
                            pos = end_pos;
                        }
                        None => {
                            push_literal(&mut fragments, &body[tag_start..tag_start + 1]);
                            pos = tag_start + 1;
                        }
                    }
                } else {
                    // Unrecognized esi: tag -- pass through as literal
                    push_literal(&mut fragments, &body[tag_start..tag_start + 1]);
                    pos = tag_start + 1;
                }
            }
        }
    }

    fragments
}

/// Resolve a list of ESI fragments into a single byte buffer.
///
/// The `resolver` callback is invoked for each `Include` fragment with the
/// `src` URL. If the resolver returns `Some(data)`, the data replaces the
/// include tag. If it returns `None`, the include is silently omitted.
///
/// `Remove` fragments produce no output.
pub fn resolve_fragments(
    fragments: Vec<EsiFragment>,
    resolver: impl Fn(&str) -> Option<Vec<u8>>,
) -> Vec<u8> {
    let mut output = Vec::new();
    for fragment in fragments {
        match fragment {
            EsiFragment::Literal(data) => output.extend_from_slice(&data),
            EsiFragment::Include { src } => {
                if let Some(data) = resolver(&src) {
                    output.extend_from_slice(&data);
                }
            }
            EsiFragment::Remove => {
                // Stripped -- no output
            }
        }
    }
    output
}

/// Process a body buffer, stripping `<esi:remove>` and `<esi:comment>` tags
/// and returning both the cleaned output and any include directives that the
/// runtime must resolve.
///
/// This is the main entry point used by the `DeliveryProcessor` implementation.
pub fn process_body(body: &[u8]) -> Result<Vec<u8>, String> {
    let fragments = parse_esi(body);
    let mut output = Vec::new();
    for fragment in &fragments {
        match fragment {
            EsiFragment::Literal(data) => output.extend_from_slice(data),
            EsiFragment::Include { src } => {
                // Emit a placeholder that the runtime can locate and replace.
                // The placeholder format uses a non-HTML marker that is easy
                // to scan for after the fact.
                let placeholder = format!("<!--esi:include src=\"{src}\"-->");
                output.extend_from_slice(placeholder.as_bytes());
            }
            EsiFragment::Remove => {
                // Stripped
            }
        }
    }
    Ok(output)
}

/// ESI delivery processor that scans response bodies for ESI tags.
///
/// This processor implements the `DeliveryProcessor` trait and can be
/// inserted into a delivery filter chain. It strips `<esi:remove>` and
/// `<esi:comment>` tags, and converts `<esi:include>` tags into HTML
/// comment placeholders that the runtime can resolve via subrequests.
pub struct EsiProcessor {
    /// Current recursion depth (for nested ESI processing).
    pub depth: u32,
    /// Maximum allowed recursion depth.
    pub max_depth: u32,
    /// Accumulated output buffer.
    output: Vec<u8>,
}

impl EsiProcessor {
    pub fn new() -> Self {
        Self {
            depth: 0,
            max_depth: DEFAULT_MAX_DEPTH,
            output: Vec::new(),
        }
    }

    /// Create an ESI processor with a specific maximum recursion depth.
    pub fn with_max_depth(max_depth: u32) -> Self {
        Self {
            depth: 0,
            max_depth,
            output: Vec::new(),
        }
    }

    /// Create an ESI processor at a given recursion depth.
    pub fn at_depth(depth: u32, max_depth: u32) -> Self {
        Self {
            depth,
            max_depth,
            output: Vec::new(),
        }
    }

    /// Extract the include directives found during processing.
    ///
    /// This returns the `src` URLs from all `<esi:include>` tags that were
    /// encountered in the most recently processed body.
    pub fn includes_from(body: &[u8]) -> Vec<String> {
        let fragments = parse_esi(body);
        fragments
            .into_iter()
            .filter_map(|f| match f {
                EsiFragment::Include { src } => Some(src),
                _ => None,
            })
            .collect()
    }
}

impl Default for EsiProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl DeliveryProcessor for EsiProcessor {
    fn name(&self) -> &str {
        "esi"
    }

    fn init(&mut self) -> Result<(), i32> {
        self.output.clear();
        Ok(())
    }

    fn bytes(&mut self, _action: VdpAction, data: &[u8]) -> Result<Vec<u8>, i32> {
        if self.depth >= self.max_depth {
            // At maximum depth, pass through without ESI processing
            return Ok(data.to_vec());
        }

        process_body(data).map_err(|_| -1)
    }

    fn fini(&mut self) {
        self.output.clear();
    }
}

// ---------------------------------------------------------------------------
// Internal parsing helpers
// ---------------------------------------------------------------------------

/// Find a byte subsequence starting at `from`.
fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Push literal bytes, merging with the previous literal fragment if possible.
fn push_literal(fragments: &mut Vec<EsiFragment>, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    if let Some(EsiFragment::Literal(existing)) = fragments.last_mut() {
        existing.extend_from_slice(data);
    } else {
        fragments.push(EsiFragment::Literal(data.to_vec()));
    }
}

/// Parse `<esi:include src="..." />` starting at `start`.
/// Returns `Some((src_url, position_after_tag))` on success.
fn parse_esi_include(body: &[u8], start: usize) -> Option<(String, usize)> {
    // Find the self-closing end of the tag: `/>`
    let close = find_bytes(body, b"/>", start)?;
    let tag_end = close + 2;

    // Extract the tag content between `<esi:include` and `/>`
    let tag_content = &body[start..close];

    // Find src="..." within the tag
    let src_start = find_bytes(tag_content, b"src=\"", 0)?;
    let url_start = src_start + b"src=\"".len();
    let url_end = find_bytes(tag_content, b"\"", url_start)?;

    let src = std::str::from_utf8(&tag_content[url_start..url_end]).ok()?;
    Some((src.to_string(), tag_end))
}

/// Parse `<esi:remove>...</esi:remove>` starting at `start`.
/// Returns `Some(position_after_closing_tag)` on success.
fn parse_esi_remove(body: &[u8], start: usize) -> Option<usize> {
    let closing_tag = b"</esi:remove>";
    let close = find_bytes(body, closing_tag, start)?;
    Some(close + closing_tag.len())
}

/// Parse `<esi:comment text="..." />` starting at `start`.
/// Returns `Some(position_after_tag)` on success.
fn parse_esi_comment(body: &[u8], start: usize) -> Option<usize> {
    let close = find_bytes(body, b"/>", start)?;
    Some(close + 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_esi_include() {
        let body = br#"<html><esi:include src="/header" /><p>content</p></html>"#;
        let fragments = parse_esi(body);
        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[0], EsiFragment::Literal(b"<html>".to_vec()),);
        assert_eq!(
            fragments[1],
            EsiFragment::Include {
                src: "/header".to_string(),
            },
        );
        assert_eq!(
            fragments[2],
            EsiFragment::Literal(b"<p>content</p></html>".to_vec()),
        );
    }

    #[test]
    fn test_parse_esi_include_with_full_url() {
        let body = br#"<esi:include src="https://example.com/fragment" />"#;
        let fragments = parse_esi(body);
        assert_eq!(fragments.len(), 1);
        assert_eq!(
            fragments[0],
            EsiFragment::Include {
                src: "https://example.com/fragment".to_string(),
            },
        );
    }

    #[test]
    fn test_parse_esi_remove_strips_content() {
        let body = b"before<esi:remove>this should be removed</esi:remove>after";
        let fragments = parse_esi(body);
        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[0], EsiFragment::Literal(b"before".to_vec()),);
        assert_eq!(fragments[1], EsiFragment::Remove);
        assert_eq!(fragments[2], EsiFragment::Literal(b"after".to_vec()),);
    }

    #[test]
    fn test_parse_esi_comment_strips_comment() {
        let body = br#"before<esi:comment text="a comment" />after"#;
        let fragments = parse_esi(body);
        // Comment produces no fragment, so before and after merge
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0], EsiFragment::Literal(b"beforeafter".to_vec()),);
    }

    #[test]
    fn test_mixed_content_with_literals_and_includes() {
        let body = br#"<html>
<esi:include src="/header" />
<p>Hello</p>
<esi:remove>debug info</esi:remove>
<esi:comment text="version 1.0" />
<esi:include src="/footer" />
</html>"#;
        let fragments = parse_esi(body);

        // Expected: Literal, Include, Literal, Remove, Literal, Include, Literal
        let mut includes = Vec::new();
        let mut literals = Vec::new();
        let mut removes = 0;
        for fragment in &fragments {
            match fragment {
                EsiFragment::Literal(data) => literals.push(data.clone()),
                EsiFragment::Include { src } => includes.push(src.clone()),
                EsiFragment::Remove => removes += 1,
            }
        }
        assert_eq!(includes, vec!["/header", "/footer"]);
        assert_eq!(removes, 1);
        assert!(!literals.is_empty());
    }

    #[test]
    fn test_no_esi_tags() {
        let body = b"<html><body>plain content</body></html>";
        let fragments = parse_esi(body);
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0], EsiFragment::Literal(body.to_vec()),);
    }

    #[test]
    fn test_malformed_include_handled_gracefully() {
        // Missing closing />
        let body = b"before<esi:include src=\"/broken\"after";
        let fragments = parse_esi(body);
        // Should pass through as literal since the tag is malformed
        assert!(!fragments.is_empty());
        // All content should be preserved as literals
        let total_bytes: usize = fragments
            .iter()
            .map(|f| match f {
                EsiFragment::Literal(d) => d.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(total_bytes, body.len());
    }

    #[test]
    fn test_malformed_remove_no_closing_tag() {
        let body = b"before<esi:remove>no closing tag here";
        let fragments = parse_esi(body);
        // Without closing tag, the content passes through as literal
        assert!(!fragments.is_empty());
        let total_bytes: usize = fragments
            .iter()
            .map(|f| match f {
                EsiFragment::Literal(d) => d.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(total_bytes, body.len());
    }

    #[test]
    fn test_nested_include_depth_limiting() {
        let body = br#"<esi:include src="/nested" />"#;

        // Processor at max depth should pass through without processing
        let mut processor = EsiProcessor::at_depth(5, 5);
        processor.init().unwrap();
        let result = processor.bytes(VdpAction::Null, body).unwrap();
        // At max depth, body passes through unchanged
        assert_eq!(&result, body);

        // Processor below max depth should process ESI tags
        let mut processor = EsiProcessor::at_depth(0, 5);
        processor.init().unwrap();
        let result = processor.bytes(VdpAction::Null, body).unwrap();
        // Should produce a placeholder, not the raw tag
        assert_ne!(&result[..], body);
        assert!(
            result
                .windows(b"<!--esi:include".len())
                .any(|w| w == b"<!--esi:include")
        );
    }

    #[test]
    fn test_resolve_fragments() {
        let fragments = vec![
            EsiFragment::Literal(b"<html>".to_vec()),
            EsiFragment::Include {
                src: "/header".to_string(),
            },
            EsiFragment::Remove,
            EsiFragment::Literal(b"<p>body</p>".to_vec()),
            EsiFragment::Include {
                src: "/footer".to_string(),
            },
            EsiFragment::Literal(b"</html>".to_vec()),
        ];

        let output = resolve_fragments(fragments, |src| match src {
            "/header" => Some(b"<h1>Header</h1>".to_vec()),
            "/footer" => Some(b"<footer>Footer</footer>".to_vec()),
            _ => None,
        });

        assert_eq!(
            &output,
            b"<html><h1>Header</h1><p>body</p><footer>Footer</footer></html>",
        );
    }

    #[test]
    fn test_resolve_fragments_missing_include() {
        let fragments = vec![
            EsiFragment::Literal(b"before".to_vec()),
            EsiFragment::Include {
                src: "/missing".to_string(),
            },
            EsiFragment::Literal(b"after".to_vec()),
        ];

        let output = resolve_fragments(fragments, |_| None);
        assert_eq!(&output, b"beforeafter");
    }

    #[test]
    fn test_process_body_entry_point() {
        let body =
            br#"<html><esi:include src="/nav" /><esi:remove>debug</esi:remove>content</html>"#;
        let result = process_body(body).unwrap();
        let result_str = std::str::from_utf8(&result).unwrap();

        // The include should become a placeholder comment
        assert!(result_str.contains(r#"<!--esi:include src="/nav"-->"#));
        // The remove content should be gone
        assert!(!result_str.contains("debug"));
        // Literals should be preserved
        assert!(result_str.contains("<html>"));
        assert!(result_str.contains("content</html>"));
    }

    #[test]
    fn test_esi_processor_delivery_trait() {
        let mut proc = EsiProcessor::new();
        assert_eq!(proc.name(), "esi");
        proc.init().unwrap();

        let body = br#"<esi:include src="/test" />"#;
        let output = proc.bytes(VdpAction::End, body).unwrap();
        let output_str = std::str::from_utf8(&output).unwrap();
        assert!(output_str.contains(r#"<!--esi:include src="/test"-->"#));

        proc.fini();
    }

    #[test]
    fn test_includes_from_helper() {
        let body = br#"<esi:include src="/a" /><p>x</p><esi:include src="/b" />"#;
        let includes = EsiProcessor::includes_from(body);
        assert_eq!(includes, vec!["/a", "/b"]);
    }

    #[test]
    fn test_empty_body() {
        let fragments = parse_esi(b"");
        assert!(fragments.is_empty());

        let result = process_body(b"").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_multiple_consecutive_includes() {
        let body = br#"<esi:include src="/a" /><esi:include src="/b" /><esi:include src="/c" />"#;
        let fragments = parse_esi(body);
        assert_eq!(fragments.len(), 3);
        for fragment in &fragments {
            match fragment {
                EsiFragment::Include { .. } => {}
                _ => panic!("expected Include fragment"),
            }
        }
    }
}
