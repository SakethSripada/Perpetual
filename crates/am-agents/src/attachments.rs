//! Attachment bytes travel with messages, including queued turns and retries.
use crate::AgentError;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
const START: &str = "<perpetual-attachments>";
const END: &str = "</perpetual-attachments>";
const MAX_BYTES: usize = 20 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub data: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
pub fn unpack(text: &str) -> Result<(String, Vec<Attachment>), AgentError> {
    let Some(start) = text.find(START) else {
        return Ok((text.into(), vec![]));
    };
    let begin = start + START.len();
    let end = text[begin..]
        .rfind(END)
        .map(|i| i + begin)
        .ok_or_else(|| AgentError::Other("Invalid attachment message".into()))?;
    let files: Vec<Attachment> = serde_json::from_str(&text[begin..end])
        .map_err(|_| AgentError::Other("Invalid attachment data".into()))?;
    if files.len() > 10 {
        return Err(AgentError::Other("Attach up to 10 files".into()));
    }
    let mut total = 0;
    for file in &files {
        if file.data.len() > (MAX_BYTES * 4 / 3 + 4) {
            return Err(AgentError::Other("Attachments exceed 20 MB".into()));
        }
        let bytes = STANDARD
            .decode(&file.data)
            .map_err(|_| AgentError::Other("Invalid attachment encoding".into()))?;
        if native_image(&file.mime) && bytes.len() > 5 * 1024 * 1024 {
            return Err(AgentError::Other("Images must be 5 MB or less".into()));
        }
        total += bytes.len();
        if total > MAX_BYTES {
            return Err(AgentError::Other("Attachments exceed 20 MB".into()));
        }
        if file.name.len() > 255 || file.mime.len() > 100 {
            return Err(AgentError::Other("Invalid attachment metadata".into()));
        }
    }
    Ok((
        format!("{}{}", &text[..start], &text[end + END.len()..]),
        files,
    ))
}
pub fn display_text(text: &str) -> String {
    match unpack(text) {
        Ok((mut text, files)) => {
            for file in files {
                text.push_str(&format!(
                    "\nAttached file {}: ./{}",
                    file.name,
                    attachment_path(&file)
                ));
            }
            text
        }
        Err(_) => text.into(),
    }
}
/// Materialize in the task workspace, so host, Docker and remote workers can
/// read PDFs and other files without depending on the client's filesystem.
pub fn prepare(text: &str, workspace: &Path) -> Result<String, AgentError> {
    let (mut text, mut files) = unpack(text)?;
    if files.is_empty() {
        return Ok(text);
    }
    for file in files.iter_mut() {
        let relative = attachment_path(file);
        let full = workspace.join(&relative);
        let dir = full.parent().expect("attachment parent");
        if !dir.exists() {
            std::fs::create_dir(dir)
                .map_err(|e| AgentError::Other(format!("Could not save attachments: {e}")))?;
        }
        if std::fs::symlink_metadata(dir)
            .map_err(|e| AgentError::Other(e.to_string()))?
            .file_type()
            .is_symlink()
        {
            return Err(AgentError::Other(
                "Attachment directory must not be a symbolic link".into(),
            ));
        }
        let bytes = STANDARD
            .decode(&file.data)
            .map_err(|e| AgentError::Other(e.to_string()))?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&full)
        {
            Ok(mut out) => {
                use std::io::Write;
                out.write_all(&bytes)
                    .map_err(|e| AgentError::Other(e.to_string()))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(&full)
                    .map_err(|e| AgentError::Other(e.to_string()))?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || std::fs::read(&full).map_err(|e| AgentError::Other(e.to_string()))? != bytes
                {
                    return Err(AgentError::Other(
                        "Saved attachment has changed; attach the file again.".into(),
                    ));
                }
            }
            Err(e) => {
                return Err(AgentError::Other(format!(
                    "Could not save {}: {e}",
                    file.name
                )))
            }
        }
        file.path = Some(relative.clone());
        text.push_str(&format!("\nAttached file {} is available at ./{relative}. Read it when answering this request.\n", file.name));
    }
    text.push_str(START);
    text.push_str(&serde_json::to_string(&files).map_err(|e| AgentError::Other(e.to_string()))?);
    text.push_str(END);
    Ok(text)
}
fn attachment_path(file: &Attachment) -> String {
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{}:{}", file.name, file.data).as_bytes())
    );
    let name: String = file
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "perpetual-upload-{digest}/{}",
        if name.is_empty() || name == "." || name == ".." {
            "file"
        } else {
            &name
        }
    )
}
pub fn codex_input(prompt: &str) -> Vec<Value> {
    let (text, files) = unpack(prompt).unwrap_or_else(|_| (prompt.to_string(), vec![]));
    let mut input = vec![json!({"type":"text","text":text})];
    for file in files {
        if native_image(&file.mime) {
            input.push(
                json!({"type":"image","url":format!("data:{};base64,{}",file.mime,file.data)}),
            );
        }
    }
    input
}
pub fn native_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}
pub fn claude_content(prompt: &str) -> Vec<Value> {
    let (text, files) = unpack(prompt).unwrap_or_else(|_| (prompt.to_string(), vec![]));
    let mut input = vec![json!({"type":"text","text":text})];
    for file in files {
        let kind = if native_image(&file.mime) {
            "image"
        } else if file.mime == "application/pdf" {
            "document"
        } else {
            continue;
        };
        input.push(
            json!({"type":kind,"source":{"type":"base64","media_type":file.mime,"data":file.data}}),
        );
    }
    input
}
#[cfg(test)]
mod tests {
    use super::*;
    fn message() -> String {
        format!("Inspect this{START}[{{\"name\":\"image.png\",\"mime\":\"image/png\",\"data\":\"aGVsbG8=\"}}]{END}\nInstructions")
    }
    #[test]
    fn native_inputs_preserve_text_and_bytes() {
        let prompt = message();
        assert_eq!(
            codex_input(&prompt)[1]["url"],
            "data:image/png;base64,aGVsbG8="
        );
        assert_eq!(claude_content(&prompt)[1]["source"]["data"], "aGVsbG8=");
        assert_eq!(unpack(&prompt).unwrap().0, "Inspect this\nInstructions");
        assert!(!display_text(&prompt).contains("aGVsbG8="));
    }
    #[test]
    fn rejects_corrupt_or_excess_attachments() {
        assert!(unpack(&message().replace("aGVsbG8=", "not base64!")).is_err());
        assert!(unpack(&format!("{START}[]{END}")).is_ok());
        assert!(unpack(&format!("{START}[]")).is_err());
    }
    #[test]
    fn attachment_only_message_survives_trim_and_retry_files_are_stable() {
        let message = format!("\n\n{START}[{{\"name\":\"../../image.png\",\"mime\":\"image/png\",\"data\":\"aGVsbG8=\"}}]{END}");
        assert_eq!(codex_input(message.trim()).len(), 2);
        let root = std::env::temp_dir().join(am_proto::new_id());
        std::fs::create_dir(&root).unwrap();
        let one = prepare(message.trim(), &root).unwrap();
        let two = prepare(message.trim(), &root).unwrap();
        assert_eq!(one, two);
        let (_, files) = unpack(&one).unwrap();
        let path = root.join(files[0].path.as_ref().unwrap());
        assert!(path
            .canonicalize()
            .unwrap()
            .starts_with(root.canonicalize().unwrap()));
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        std::fs::write(&path, "changed").unwrap();
        assert!(prepare(&message, &root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pdf_uses_document_input() {
        let pdf = message().replace("image/png", "application/pdf");
        assert_eq!(claude_content(&pdf)[1]["type"], "document");
        assert_eq!(codex_input(&pdf).len(), 1);
    }
}
