//! Local command templates. Values entered while using a template are never stored.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config;

pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TEMPLATE_BYTES: usize = 64 * 1024;
const MAX_SNIPPETS: usize = 1000;
const MAX_NAME_BYTES: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    #[serde(default)]
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub group: String,
    pub template: String,
}

impl Snippet {
    pub fn group_label(&self) -> &str {
        if self.group.is_empty() { "General" } else { &self.group }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.name.len() > MAX_NAME_BYTES || self.name.chars().any(char::is_control) {
            return Err("Enter a snippet name of at most 256 bytes without control characters.".into());
        }
        if self.group.len() > 128 || self.group.chars().any(char::is_control) {
            return Err("The group must be at most 128 bytes without control characters.".into());
        }
        validate_commands(&self.template)?;
        Template::parse(&self.template)?;
        Ok(())
    }
}

pub fn validate_commands(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Enter at least one command.".into());
    }
    if text.len() > MAX_TEMPLATE_BYTES {
        return Err("Commands must be at most 64 KiB.".into());
    }
    if text.chars().any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r')) {
        return Err("Commands may contain text, tabs and line breaks, but no terminal control characters.".into());
    }
    Ok(())
}

#[derive(Clone, Debug)]
enum Part {
    Text(String),
    Variable(String),
}

#[derive(Clone, Debug)]
pub struct Template {
    parts: Vec<Part>,
    pub variables: Vec<String>,
}

impl Template {
    /// `{{name}}` is a variable; `{{{{` inserts a literal `{{`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut rest = text;
        let mut parts = Vec::new();
        let mut variables = Vec::new();
        while let Some(start) = rest.find("{{") {
            parts.push(Part::Text(rest[..start].into()));
            rest = &rest[start..];
            if let Some(after) = rest.strip_prefix("{{{{") {
                parts.push(Part::Text("{{".into()));
                rest = after;
                continue;
            }
            let end = rest.find("}}").ok_or("A variable is missing its closing }}.")?;
            let name = rest[2..end].trim();
            let mut chars = name.chars();
            if !chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
                || name.len() > 64
            {
                return Err("Variable names must start with a letter or underscore and contain only letters, numbers or underscores (up to 64 characters).".into());
            }
            if !variables.iter().any(|variable| variable == name) {
                if variables.len() >= 32 {
                    return Err("A template can have at most 32 different variables.".into());
                }
                variables.push(name.into());
            }
            parts.push(Part::Variable(name.into()));
            rest = &rest[end + 2..];
        }
        parts.push(Part::Text(rest.into()));
        Ok(Self { parts, variables })
    }

    pub fn expand(&self, values: &BTreeMap<String, String>) -> Result<String, String> {
        let missing = self
            .variables
            .iter()
            .filter(|name| values.get(*name).is_none_or(|v| v.trim().is_empty()))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!("Enter a value for: {}.", missing.join(", ")));
        }
        let mut text = String::new();
        for part in &self.parts {
            match part {
                Part::Text(literal) => text.push_str(literal),
                Part::Variable(name) => {
                    let value = &values[name];
                    if value.chars().any(char::is_control) {
                        return Err(format!("{name} must be a single line without control characters."));
                    }
                    text.push_str(value);
                }
            }
            if text.len() > MAX_TEMPLATE_BYTES {
                return Err("Expanded commands must be at most 64 KiB.".into());
            }
        }
        validate_commands(&text)?;
        Ok(text)
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    next_id: u64,
    snippets: Vec<Snippet>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

pub struct SnippetStore {
    pub path: PathBuf,
    pub snippets: Vec<Snippet>,
    pub error: Option<String>,
    next_id: u64,
}

impl SnippetStore {
    pub fn open(path: PathBuf) -> Self {
        let mut store = Self { path, snippets: Vec::new(), error: None, next_id: 1 };
        let result = (|| {
            let file = match fs::File::open(&store.path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 1)),
                Err(error) => return Err(error.to_string()),
            };
            let mut bytes = Vec::new();
            file.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            let file = parse_file(&bytes)?;
            let mut ids = HashSet::new();
            let mut names = HashSet::new();
            for snippet in &file.snippets {
                snippet.validate()?;
                if snippet.id == 0 || !ids.insert(snippet.id) || !names.insert(name_key(snippet)) {
                    return Err("Snippet IDs and names within each group must be unique.".into());
                }
            }
            let minimum = next_id(&file.snippets)?;
            let next = if file.next_id == 0 { minimum } else { file.next_id };
            if next < minimum {
                return Err("Invalid next snippet ID.".into());
            }
            Ok((file.snippets, next))
        })();
        match result {
            Ok((snippets, next)) => {
                store.snippets = snippets;
                store.next_id = next;
            }
            Err(error) => {
                store.error =
                    Some(format!("Could not load {}: {error}. The file has not been changed.", store.path.display()))
            }
        }
        store
    }

    pub fn get(&self, id: u64) -> Option<&Snippet> {
        self.snippets.iter().find(|snippet| snippet.id == id)
    }

    pub fn put(&mut self, mut snippet: Snippet) -> Result<u64, String> {
        snippet.name = snippet.name.trim().into();
        snippet.group = snippet.group.trim().into();
        snippet.validate()?;
        let mut updated = self.snippets.clone();
        let mut next = self.next_id;
        if updated.iter().any(|s| s.id != snippet.id && name_key(s) == name_key(&snippet)) {
            return Err("That snippet name already exists in this group. Choose a different name.".into());
        }
        if snippet.id == 0 {
            snippet.id = next;
            next = next.checked_add(1).ok_or("Snippet IDs are exhausted.")?;
            updated.push(snippet.clone());
        } else {
            let existing = updated.iter_mut().find(|s| s.id == snippet.id).ok_or("This snippet no longer exists.")?;
            *existing = snippet.clone();
        }
        self.persist(updated, next)?;
        Ok(snippet.id)
    }

    pub fn delete(&mut self, id: u64) -> Result<(), String> {
        if self.get(id).is_none() {
            return Err("This snippet no longer exists.".into());
        }
        self.persist(self.snippets.iter().filter(|s| s.id != id).cloned().collect(), self.next_id)
    }

    pub fn import(&mut self, snippets: Vec<Snippet>) -> Result<usize, String> {
        let count = snippets.len();
        let mut updated = self.snippets.clone();
        let mut next = self.next_id;
        for mut snippet in plan_import(&updated, &snippets) {
            snippet.id = next;
            next = next.checked_add(1).ok_or("Snippet IDs are exhausted.")?;
            updated.push(snippet);
        }
        self.persist(updated, next)?;
        Ok(count)
    }

    fn persist(&mut self, snippets: Vec<Snippet>, next_id: u64) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let text = encode_file(&snippets, next_id)?;
        config::write_atomic(&self.path, &text).map_err(|e| format!("Could not save snippets: {e}"))?;
        self.snippets = snippets;
        self.next_id = next_id;
        Ok(())
    }
}

fn next_id(snippets: &[Snippet]) -> Result<u64, String> {
    snippets.iter().map(|s| s.id).max().unwrap_or(0).checked_add(1).ok_or_else(|| "Snippet IDs are exhausted.".into())
}

fn name_key(snippet: &Snippet) -> (String, String) {
    (snippet.group_label().to_lowercase(), snippet.name.to_lowercase())
}

fn parse_file(bytes: &[u8]) -> Result<File, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("The snippets file is larger than 4 MiB.".into());
    }
    let file: File = serde_json::from_slice(bytes).map_err(|e| format!("Invalid snippets JSON: {e}"))?;
    if file.version != 1 {
        return Err(format!("Unsupported snippets file version {}.", file.version));
    }
    if file.snippets.len() > MAX_SNIPPETS {
        return Err("A snippets file can contain at most 1000 entries.".into());
    }
    Ok(file)
}

pub fn export_snippets(snippets: &[Snippet]) -> Result<String, String> {
    encode_file(snippets, 0)
}

fn encode_file(snippets: &[Snippet], next_id: u64) -> Result<String, String> {
    if snippets.len() > MAX_SNIPPETS {
        return Err("At most 1000 snippets can be saved.".into());
    }
    for snippet in snippets {
        snippet.validate()?;
    }
    let text = serde_json::to_string_pretty(&File { version: 1, next_id, snippets: snippets.into() })
        .map_err(|e| e.to_string())?;
    if text.len() > MAX_FILE_BYTES {
        return Err("The snippets file would be larger than 4 MiB.".into());
    }
    Ok(text)
}

pub struct SnippetImport {
    pub snippets: Vec<Snippet>,
    pub notes: Vec<String>,
}

pub fn parse_import(bytes: &[u8]) -> Result<SnippetImport, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("The snippets file is larger than 4 MiB.".into());
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("Invalid snippets JSON: {e}"))?;
    if value.get("version").and_then(|v| v.as_u64()) != Some(1) {
        return Err("Only version-1 Snekkie snippets JSON is supported.".into());
    }
    let rows = value.get("snippets").and_then(|v| v.as_array()).ok_or("The file must contain a snippets array.")?;
    if rows.len() > MAX_SNIPPETS {
        return Err("At most 1000 snippets can be imported.".into());
    }
    let mut batch = SnippetImport { snippets: Vec::new(), notes: Vec::new() };
    for (index, row) in rows.iter().enumerate() {
        let result =
            serde_json::from_value::<Snippet>(row.clone()).map_err(|e| e.to_string()).and_then(|mut snippet| {
                snippet.id = 0;
                snippet.name = snippet.name.trim().into();
                snippet.group = snippet.group.trim().into();
                snippet.validate()?;
                Ok(snippet)
            });
        match result {
            Ok(snippet) => batch.snippets.push(snippet),
            Err(error) => batch.notes.push(format!("Skipped entry {}: {error}", index + 1)),
        }
    }
    Ok(batch)
}

pub fn plan_import(existing: &[Snippet], incoming: &[Snippet]) -> Vec<Snippet> {
    let mut used: HashSet<_> = existing.iter().map(name_key).collect();
    incoming
        .iter()
        .map(|snippet| {
            let mut copy = snippet.clone();
            copy.id = 0;
            let mut suffix_number = 1;
            while used.contains(&name_key(&copy)) {
                let suffix = format!(" (Imported {suffix_number})");
                let mut base = snippet.name.clone();
                while base.len() + suffix.len() > MAX_NAME_BYTES {
                    base.pop();
                }
                copy.name = format!("{base}{suffix}");
                suffix_number += 1;
            }
            used.insert(name_key(&copy));
            copy
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(name: &str, template: &str) -> Snippet {
        Snippet { name: name.into(), template: template.into(), ..Default::default() }
    }

    #[test]
    fn variables_are_ordered_unique_literal_and_not_recursive() {
        let template = Template::parse("show {{interface}} {{vlan}} {{interface}} {{{{literal}}").unwrap();
        assert_eq!(template.variables, ["interface", "vlan"]);
        let values = BTreeMap::from([("interface".into(), "Gi0/1".into()), ("vlan".into(), "{{other}}".into())]);
        assert_eq!(template.expand(&values).unwrap(), "show Gi0/1 {{other}} Gi0/1 {{literal}}");
        assert!(Template::parse("set {{unfinished").is_err());
        assert!(Template::parse("set {{bad-name}}").is_err());
        assert!(template.expand(&BTreeMap::new()).unwrap_err().contains("interface, vlan"));
    }

    #[test]
    fn values_cannot_hide_line_breaks_or_control_sequences() {
        let template = Template::parse("show {{value}}").unwrap();
        for value in ["a\nb", "a\rb", "\u{1b}[2J", "\t", "\u{7f}"] {
            assert!(template.expand(&BTreeMap::from([("value".into(), value.into())])).is_err());
        }
        assert!(snippet("Control", "show\u{1b}").validate().is_err());
        assert!(snippet("Empty", " ").validate().is_err());
    }

    #[test]
    fn save_rename_move_and_delete_preserve_other_snippets_and_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snippets.json");
        let mut store = SnippetStore::open(path.clone());
        let first = store.put(snippet("Version", "show version")).unwrap();
        let second = store.put(snippet("Interface", "show interface {{interface}}")).unwrap();
        let unrelated = store.get(second).unwrap().clone();
        let mut changed = store.get(first).unwrap().clone();
        changed.name = "Platform".into();
        changed.group = "Cisco IOS".into();
        store.put(changed.clone()).unwrap();
        let mut restarted = SnippetStore::open(path);
        assert_eq!(restarted.get(first), Some(&changed));
        assert_eq!(restarted.get(second), Some(&unrelated));
        restarted.delete(first).unwrap();
        assert_eq!(restarted.snippets, [unrelated]);
    }

    #[test]
    fn importing_is_additive_and_export_contains_templates_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SnippetStore::open(dir.path().join("snippets.json"));
        store.put(snippet("Version", "show version")).unwrap();
        let original = store.snippets[0].clone();
        let mut exported = snippet("VERSION", "show {{command}}");
        exported.id = original.id;
        let text = export_snippets(&[exported]).unwrap();
        let batch = parse_import(text.as_bytes()).unwrap();
        store.import(batch.snippets).unwrap();
        assert_eq!(store.snippets[0], original);
        assert_eq!(store.snippets[1].name, "VERSION (Imported 1)");
        assert_ne!(store.snippets[1].id, original.id);
        assert_eq!(store.snippets[1].template, "show {{command}}");
        let batch = parse_import(br#"{"version":1,"snippets":[{"name":"Bad"},{"name":"Good","template":"show ver"}]}"#)
            .unwrap();
        assert_eq!(batch.snippets.len(), 1);
        assert_eq!(batch.notes.len(), 1);
    }

    #[test]
    fn damaged_future_and_unwritable_stores_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snippets.json");
        for text in ["broken", r#"{"version":2,"snippets":[]}"#] {
            fs::write(&path, text).unwrap();
            let mut store = SnippetStore::open(path.clone());
            assert!(store.error.is_some());
            assert!(store.put(snippet("New", "show ver")).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
        }
        let mut store = SnippetStore::open(dir.path().join("blocked/snippets.json"));
        fs::write(dir.path().join("blocked"), "not a directory").unwrap();
        assert!(store.put(snippet("New", "show ver")).is_err());
        assert!(store.snippets.is_empty());
    }

    #[test]
    fn deleted_ids_are_not_reused_after_restart_and_failed_writes_do_not_consume_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snippets.json");
        let mut store = SnippetStore::open(path.clone());
        let id = store.put(snippet("First", "show version")).unwrap();
        store.delete(id).unwrap();
        let mut restarted = SnippetStore::open(path);
        let original_path = restarted.path.clone();
        restarted.path = dir.path().join("directory");
        fs::create_dir(&restarted.path).unwrap();
        assert!(restarted.put(snippet("Failed", "show version")).is_err());
        restarted.path = original_path;
        assert_eq!(restarted.put(snippet("Second", "show version")).unwrap(), id + 1);
        assert!(restarted.put(Snippet { id, ..snippet("Stale editor", "show version") }).is_err());
    }
}
