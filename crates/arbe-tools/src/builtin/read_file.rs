use std::path::PathBuf;

use arbe_core::{ContentBlock, ImageSource, RiskLevel, ToolError, ToolInvocation, ToolResult};
use async_trait::async_trait;
use base64::Engine;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::path_guard::{resolve_within_root, verify_no_symlink_escape};
use crate::{ToolContext, ToolDescription, ToolExecutor};

/// Files larger than this are rejected rather than read in full — keeps a
/// single tool call from pulling an entire large binary/log into the
/// model's context by accident.
const MAX_READ_BYTES: u64 = 5 * 1024 * 1024;

/// Largest image returned as an image. Base64 adds a third, which keeps it
/// under the 5 MB-per-image limit the strictest provider (Anthropic) sets.
const MAX_IMAGE_BYTES: u64 = 3_750_000;

/// The image formats every vision-capable provider accepts, by extension.
fn image_media_type(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Args {
    /// Path relative to the project root.
    path: String,
    /// First line to read, 1-indexed and inclusive. Default: the first line.
    start_line: Option<u64>,
    /// Last line to read, 1-indexed and inclusive. Default: the last line.
    end_line: Option<u64>,
}

pub struct ReadFileTool {
    root: PathBuf,
}

impl ReadFileTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl ToolExecutor for ReadFileTool {
    fn read_only(&self) -> bool {
        true
    }

    fn subject_kind(&self) -> crate::SubjectKind {
        crate::SubjectKind::Path
    }

    /// Rule subject: the path (see `ToolExecutor::subject`).
    fn subject(&self, arguments: &serde_json::Value) -> Option<String> {
        crate::path_subject(arguments, None)
    }

    fn description(&self) -> ToolDescription {
        ToolDescription::from_args::<Args>(
            "Read a UTF-8 text file (optionally a 1-indexed inclusive line range) from within the project directory. PNG, JPEG, GIF and WebP files are returned as an image you can look at, if you can see images.",
        )
    }

    fn default_risk(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(
        &self,
        invocation: ToolInvocation,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let args: Args = serde_json::from_value(invocation.arguments)
            .map_err(|e| ToolError::Validation(format!("invalid read_file arguments: {e}")))?;
        let path = resolve_within_root(&self.root, &args.path)?;
        verify_no_symlink_escape(&self.root, &path).await?;

        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| ToolError::RuntimeFailure(format!("{}: {e}", path.display())))?;
        if metadata.len() > MAX_READ_BYTES {
            return Err(ToolError::Validation(format!(
                "{} is {} bytes, over the {MAX_READ_BYTES}-byte read limit",
                path.display(),
                metadata.len()
            )));
        }

        if let Some(media_type) = image_media_type(&path) {
            if metadata.len() > MAX_IMAGE_BYTES {
                return Err(ToolError::Validation(format!(
                    "{} is {} bytes, over the {MAX_IMAGE_BYTES}-byte image limit",
                    path.display(),
                    metadata.len()
                )));
            }
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| ToolError::RuntimeFailure(format!("{}: {e}", path.display())))?;
            return Ok(ToolResult {
                id: invocation.id,
                output: json!({
                    "image": args.path,
                    "media_type": media_type,
                    "bytes": bytes.len(),
                }),
                is_error: false,
                attachments: vec![ContentBlock::Image {
                    source: ImageSource::Base64 {
                        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                    },
                    media_type: media_type.to_string(),
                }],
            });
        }

        let contents = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| ToolError::RuntimeFailure(format!("{}: {e}", path.display())))?;

        let (content, total_lines) = slice_lines(&contents, args.start_line, args.end_line)?;

        Ok(ToolResult {
            id: invocation.id,
            output: json!({ "content": content, "total_lines": total_lines }),
            is_error: false,
            attachments: Vec::new(),
        })
    }
}

/// Extracts `[start_line, end_line]` (1-indexed, inclusive) from `text`;
/// a missing bound is the start or end of the file, and with neither the
/// whole text comes back as it is. Pure and separately tested so the
/// line-range edge cases (out-of-range, reversed, single-line files) don't
/// need a real file on disk to verify.
fn slice_lines(
    text: &str,
    start_line: Option<u64>,
    end_line: Option<u64>,
) -> Result<(String, u64), ToolError> {
    let lines: Vec<&str> = text.lines().collect();
    let total_lines = lines.len() as u64;

    if start_line.is_none() && end_line.is_none() {
        return Ok((text.to_string(), total_lines));
    }
    let start = start_line.unwrap_or(1);
    let end = end_line.unwrap_or(total_lines.max(start));

    if start == 0 || start > end {
        return Err(ToolError::Validation(format!(
            "invalid line range: start_line={start}, end_line={end}"
        )));
    }

    let start_idx = (start - 1) as usize;
    if start_idx >= lines.len() {
        return Ok((String::new(), total_lines));
    }
    let end_idx = (end as usize).min(lines.len());

    Ok((lines[start_idx..end_idx].join("\n"), total_lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecuteWithDefaultContext;
    use arbe_core::{RiskLevel, ToolCallId, TurnId};
    use tempfile::tempdir;

    fn invocation(args: serde_json::Value) -> ToolInvocation {
        ToolInvocation {
            id: ToolCallId::new(),
            source_turn: TurnId::new(),
            tool_name: "read_file".to_string(),
            arguments: args,
            risk: RiskLevel::Low,
            rationale: None,
        }
    }

    #[test]
    fn slice_lines_returns_everything_when_no_range_given() {
        let (content, total) = slice_lines("a\nb\nc", None, None).unwrap();
        assert_eq!(content, "a\nb\nc");
        assert_eq!(total, 3);
    }

    #[test]
    fn slice_lines_extracts_an_inclusive_range() {
        let (content, total) = slice_lines("a\nb\nc\nd", Some(2), Some(3)).unwrap();
        assert_eq!(content, "b\nc");
        assert_eq!(total, 4);
    }

    #[test]
    fn a_missing_bound_means_the_start_or_end_of_the_file() {
        assert_eq!(slice_lines("a\nb\nc\nd", Some(3), None).unwrap().0, "c\nd");
        assert_eq!(slice_lines("a\nb\nc\nd", None, Some(2)).unwrap().0, "a\nb");
        // Starting past the end is empty, not an error.
        assert_eq!(slice_lines("a\nb", Some(5), None).unwrap().0, "");
    }

    #[test]
    fn slice_lines_clamps_an_end_past_the_file_length() {
        let (content, _) = slice_lines("a\nb", Some(1), Some(100)).unwrap();
        assert_eq!(content, "a\nb");
    }

    #[test]
    fn slice_lines_rejects_a_zero_start_line() {
        assert!(slice_lines("a\nb", Some(0), Some(1)).is_err());
    }

    #[test]
    fn slice_lines_rejects_a_reversed_range() {
        assert!(slice_lines("a\nb\nc", Some(3), Some(1)).is_err());
    }

    #[test]
    fn slice_lines_returns_empty_for_a_start_past_the_file_length() {
        let (content, total) = slice_lines("a\nb", Some(10), Some(20)).unwrap();
        assert_eq!(content, "");
        assert_eq!(total, 2);
    }

    #[tokio::test]
    async fn reads_a_whole_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello world").unwrap();

        let tool = ReadFileTool::new(dir.path().to_path_buf());
        let result = tool
            .execute_default(invocation(json!({ "path": "hello.txt" })))
            .await
            .unwrap();

        assert_eq!(result.output["content"], "hello world");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn reads_a_line_range() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\nthree\n").unwrap();

        let tool = ReadFileTool::new(dir.path().to_path_buf());
        let result = tool
            .execute_default(invocation(
                json!({ "path": "f.txt", "start_line": 2, "end_line": 2 }),
            ))
            .await
            .unwrap();

        assert_eq!(result.output["content"], "two");
    }

    #[tokio::test]
    async fn rejects_a_path_escaping_the_root() {
        let dir = tempdir().unwrap();
        let tool = ReadFileTool::new(dir.path().to_path_buf());
        let err = tool
            .execute_default(invocation(json!({ "path": "../outside.txt" })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn missing_file_is_a_runtime_failure_not_a_panic() {
        let dir = tempdir().unwrap();
        let tool = ReadFileTool::new(dir.path().to_path_buf());
        let err = tool
            .execute_default(invocation(json!({ "path": "nope.txt" })))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::RuntimeFailure(_)));
    }

    #[tokio::test]
    async fn invalid_arguments_are_a_validation_error() {
        let dir = tempdir().unwrap();
        let tool = ReadFileTool::new(dir.path().to_path_buf());
        let err = tool
            .execute_default(invocation(json!({})))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Validation(_)));
    }

    #[tokio::test]
    async fn image_files_come_back_as_images() {
        let dir = tempfile::tempdir().unwrap();
        // A 1x1 PNG.
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48, 0x44, 0x52,
        ];
        std::fs::write(dir.path().join("shot.PNG"), png).unwrap();
        let tool = ReadFileTool::new(dir.path().to_path_buf());
        let result = tool
            .execute_default(invocation(json!({"path": "shot.PNG"})))
            .await
            .unwrap();
        assert_eq!(result.output["media_type"], "image/png");
        assert_eq!(result.output["bytes"], png.len());
        match &result.attachments[..] {
            [
                ContentBlock::Image {
                    source: ImageSource::Base64 { data },
                    media_type,
                },
            ] => {
                assert_eq!(media_type, "image/png");
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap();
                assert_eq!(decoded, png);
            }
            other => panic!("{other:?}"),
        }
    }
}
