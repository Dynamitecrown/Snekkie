//! The tools that let an agent work with files on a session's server, over
//! SFTP, or SCP on a device without it. They have their own setting, because they reach this computer's
//! files as well as the server's.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::{Inner, find, not_connected, session_id, state_text};
use crate::profiles::Kind;
use crate::transport::sftp::{ExplorerLink, Files};

/// Fits in one request or reply, with room for escaping.
const MAX_TEXT: u64 = 512 * 1024;

const OFF: &str = "File access for agents is off. Ask the user to turn on Preferences → Network → Let agents \
                   transfer and change files.";

/// An open file explorer tab, as agents see it.
#[derive(Clone)]
pub struct ExplorerEntry {
    pub id: u64,
    pub name: String,
    pub description: String,
    /// While it's connected.
    pub link: Option<ExplorerLink>,
}

pub(super) fn handles(tool: &str) -> bool {
    tools().iter().any(|listed| listed["name"] == tool)
}

/// A transfer can far outlast a command, so the bridge waits longer for it.
pub(super) fn long_running(tool: &str) -> bool {
    matches!(tool, "upload_file" | "download_file")
}

pub(super) async fn call(inner: &Inner, agent: &str, tool: &str, arguments: &Value) -> Result<String, String> {
    if !inner.files_allowed.load(Ordering::Relaxed) {
        return Err(OFF.into());
    }
    let files = open(inner, arguments).await?;
    let id = session_id(arguments)?;
    let reads_only = matches!(tool, "list_files" | "read_file");
    let _reading = reads_only.then(|| inner.reading(id, agent));
    if !reads_only {
        inner.wrote(id, agent);
    }
    let result = run(&files, tool, arguments).await;
    files.close(!reads_only).await;
    result
}

async fn open(inner: &Inner, arguments: &Value) -> Result<Files, String> {
    let id = session_id(arguments)?;
    let explorer = inner.listing.lock().explorers.iter().find(|tab| tab.id == id).cloned();
    if let Some(tab) = explorer {
        return tab
            .link
            .and_then(|link| link.files())
            .ok_or_else(|| format!("{} isn't connected. Ask the user to reconnect it.", tab.name));
    }
    let session = find(&inner.listing, arguments)?;
    if session.kind() != Kind::Ssh {
        return Err(format!(
            "{} is a {} session. File tools need an SSH session or a file explorer tab.",
            session.name,
            session.kind().label()
        ));
    }
    let shared = session.share_connection().ok_or_else(|| {
        format!("{} is {}, so no file was touched. Ask the user to reconnect it.", session.name, {
            state_text(&session.state())
        })
    })?;
    let connection = shared.await.map_err(|_| not_connected(&session))?;
    Files::open(connection).await
}

fn text<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, String> {
    arguments[key].as_str().filter(|text| !text.is_empty()).ok_or_else(|| format!("{key} is required."))
}

/// The app's working folder means nothing to an agent, so paths must be whole.
fn local_path(arguments: &Value, key: &str) -> Result<PathBuf, String> {
    let path = Path::new(text(arguments, key)?);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Err(format!("{key} must be a full path on this computer."))
    }
}

async fn run(files: &Files, tool: &str, arguments: &Value) -> Result<String, String> {
    let overwrite = arguments["overwrite"].as_bool().unwrap_or(false);
    match tool {
        "list_files" => {
            let (path, entries) = files.list(arguments["path"].as_str().unwrap_or(".")).await?;
            let entries: Vec<Value> = entries
                .iter()
                .map(|entry| {
                    json!({
                        "name": entry.name,
                        "type": if entry.is_dir { "folder" } else { "file" },
                        "link": entry.is_symlink,
                        "size": entry.size,
                        "modified": entry.modified,
                        "permissions": entry.permissions.map(|bits| format!("{bits:o}")),
                    })
                })
                .collect();
            Ok(serde_json::to_string_pretty(&json!({ "path": path, "entries": entries })).unwrap_or_default())
        }
        "read_file" => {
            let path = text(arguments, "path")?;
            let bytes = files.read(path, MAX_TEXT).await?;
            String::from_utf8(bytes).map_err(|_| format!("{path} isn't text. Use download_file."))
        }
        "write_file" => {
            let path = text(arguments, "path")?;
            // An empty file is a fair thing to write, so this isn't text().
            let content = arguments["content"].as_str().ok_or("content is required.")?;
            files.write(path, content.as_bytes(), overwrite).await?;
            Ok(format!("Wrote {} bytes to {path}.", content.len()))
        }
        "upload_file" => {
            let (local, remote) = (local_path(arguments, "local_path")?, text(arguments, "remote_path")?);
            files.upload(&local, remote, overwrite).await?;
            Ok(format!("Uploaded {} to {remote}.", local.display()))
        }
        "download_file" => {
            let (remote, local) = (text(arguments, "remote_path")?, local_path(arguments, "local_path")?);
            files.download(remote, &local, overwrite).await?;
            Ok(format!("Downloaded {remote} to {}.", local.display()))
        }
        "make_folder" => {
            let path = text(arguments, "path")?;
            files.make_folder(path).await?;
            Ok(format!("Created {path}."))
        }
        "rename_path" => {
            let (from, to) = (text(arguments, "from")?, text(arguments, "to")?);
            files.rename(from, to).await?;
            Ok(format!("Renamed {from} to {to}."))
        }
        "delete_path" => {
            let path = text(arguments, "path")?;
            files.delete(path).await?;
            Ok(format!("Deleted {path}."))
        }
        "set_permissions" => {
            let path = text(arguments, "path")?;
            let mode = text(arguments, "permissions")?;
            let bits = u32::from_str_radix(mode, 8)
                .ok()
                .filter(|bits| *bits <= 0o777)
                .ok_or("permissions must be octal, such as 644.")?;
            files.set_permissions(path, bits).await?;
            Ok(format!("Set {path} to {bits:o}."))
        }
        _ => Err(format!("Unknown tool: {tool}")),
    }
}

pub(super) fn tools() -> Vec<Value> {
    let session_id = json!({
        "type": "integer",
        "description": "A session_id from list_sessions: an SSH session or a file explorer tab.",
    });
    let path =
        json!({ "type": "string", "description": "A path on the server. Relative paths start at the login's home." });
    let overwrite = json!({ "type": "boolean", "description": "Replace what's there. Without this, an existing file is an error. SCP can't check, so sending to an SCP-only device always needs this." });
    let local = json!({ "type": "string", "description": "A full path to a file on the user's computer." });
    let tool = |name: &str, description: &str, properties: Value, required: &[&str], annotations: Value| {
        let mut required: Vec<&str> = required.to_vec();
        required.insert(0, "session_id");
        let mut properties = properties;
        properties["session_id"] = session_id.clone();
        json!({
            "name": name,
            "description": description,
            "inputSchema": { "type": "object", "properties": properties, "required": required },
            "annotations": annotations,
        })
    };
    let reads = json!({ "readOnlyHint": true });
    let changes = json!({ "destructiveHint": true });
    vec![
        tool(
            "list_files",
            "List a folder on the server: name, type, size, modified time (seconds since 1970) and permissions.",
            json!({ "path": { "type": "string", "description": "A folder on the server. Defaults to the login's home." } }),
            &[],
            reads.clone(),
        ),
        tool(
            "read_file",
            "Read a text file on the server, up to 512 KB. Use download_file for anything bigger or binary.",
            json!({ "path": path }),
            &["path"],
            reads,
        ),
        tool(
            "write_file",
            "Create or replace a text file on the server, up to 512 KB. To edit a file, read_file it and write the \
             whole new text with overwrite. Over SFTP a replaced file keeps its permissions and a failed write leaves it as it \
             was.",
            json!({ "path": path, "content": { "type": "string" }, "overwrite": overwrite }),
            &["path", "content"],
            changes.clone(),
        ),
        tool(
            "upload_file",
            "Copy one file from the user's computer to the server. Waits until it finishes. Works on SCP-only devices such as network gear, where a remote path looks like flash:/name.bin. An SCP transfer that fails can leave a damaged file behind.",
            json!({ "local_path": local, "remote_path": path, "overwrite": overwrite }),
            &["local_path", "remote_path"],
            changes.clone(),
        ),
        tool(
            "download_file",
            "Copy one file from the server to the user's computer. Waits until it finishes. Works on SCP-only devices.",
            json!({ "remote_path": path, "local_path": local, "overwrite": overwrite }),
            &["remote_path", "local_path"],
            changes.clone(),
        ),
        tool("make_folder", "Create a folder on the server.", json!({ "path": path }), &["path"], changes.clone()),
        tool(
            "rename_path",
            "Rename or move a file or folder on the server. Refuses to replace what's already there.",
            json!({ "from": path, "to": path }),
            &["from", "to"],
            changes.clone(),
        ),
        tool(
            "delete_path",
            "Delete a file, link or empty folder on the server. There is no undo.",
            json!({ "path": path }),
            &["path"],
            changes.clone(),
        ),
        tool(
            "set_permissions",
            "Set a file or folder's permissions on the server, in octal such as 644.",
            json!({ "path": path, "permissions": { "type": "string" } }),
            &["path", "permissions"],
            changes,
        ),
    ]
}
