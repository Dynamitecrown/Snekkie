use std::collections::BTreeMap;

use crate::snippets::{Snippet, SnippetImport, SnippetStore, Template, plan_import, validate_commands};

/// Captured once when the library opens; tab focus cannot change this destination.
pub type SnippetTarget = crate::paste::PasteTarget;

enum Page {
    Library,
    Editor(Snippet),
    Review { snippet: Snippet, template: Template, values: BTreeMap<String, String> },
    Draft(String),
    Transfer { export: bool, rows: Vec<Snippet>, selected: Vec<bool>, notes: Vec<String> },
}

pub enum Action {
    Close,
    Save(Snippet),
    Delete(u64),
    ImportFile,
    Import(Vec<Snippet>),
    Export(Vec<Snippet>),
    Send(String),
}

pub struct SnippetWindow {
    pub target: Option<SnippetTarget>,
    page: Page,
    selected: Option<u64>,
    filter: String,
    reset_scroll: bool,
}

impl SnippetWindow {
    pub fn new(target: Option<SnippetTarget>) -> Self {
        Self { target, page: Page::Library, selected: None, filter: String::new(), reset_scroll: true }
    }

    pub fn saved(&mut self, id: u64) {
        self.selected = Some(id);
        self.page = Page::Library;
        self.reset_scroll = true;
    }

    pub fn imported(&mut self) {
        self.page = Page::Library;
        self.reset_scroll = true;
    }

    pub fn preview_import(&mut self, batch: SnippetImport, existing: &[Snippet]) {
        let rows = plan_import(existing, &batch.snippets);
        let mut notes = batch.notes;
        for (original, renamed) in batch.snippets.iter().zip(&rows) {
            if original.name != renamed.name {
                notes.push(format!("{} will be added as {}.", original.name, renamed.name));
            }
        }
        self.page = Page::Transfer { export: false, selected: vec![true; rows.len()], rows, notes };
        self.reset_scroll = true;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, store: &SnippetStore, target_valid: bool) -> Option<Action> {
        let heading = ui.heading(match self.page {
            Page::Library => "Command snippets",
            Page::Editor(_) => "Edit command snippet",
            Page::Review { .. } => "Review commands",
            Page::Draft(_) => "Command draft",
            Page::Transfer { export: true, .. } => "Export snippets",
            Page::Transfer { export: false, .. } => "Import snippets",
        });
        let mut header_height = heading.rect.height();
        if let Some(target) = &self.target {
            header_height += ui.label(format!("Destination: {}", target.description)).rect.height();
            if !target_valid {
                header_height += ui
                    .colored_label(
                        ui.visuals().error_fg_color,
                        "Connection changed or closed. Reopen snippets on a connected tab.",
                    )
                    .rect
                    .height();
            }
        } else {
            header_height += ui.label("Open snippets on a connected tab to insert or send commands.").rect.height();
        }
        let writable = store.error.is_none();
        let mut next = None;
        let mut action = None;
        let footer_height = if matches!(self.page, Page::Library) { 70.0 } else { 36.0 };
        let room = (ui.ctx().content_rect().height() - header_height - footer_height - 44.0).clamp(45.0, 420.0);
        let mut expanded = Err(String::new());
        let mut scroll =
            egui::ScrollArea::vertical().id_salt("snippet_body").max_height(room).auto_shrink([false, true]);
        if std::mem::take(&mut self.reset_scroll) {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show(ui, |ui| {
            match &mut self.page {
                Page::Library => {
                    if let Some(error) = &store.error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    let label = ui.label("Filter snippets");
                    ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter names and groups").desired_width(f32::INFINITY))
                        .labelled_by(label.id);
                    let query = self.filter.trim().to_lowercase();
                    let mut groups: BTreeMap<&str, Vec<&Snippet>> = BTreeMap::new();
                    for snippet in &store.snippets {
                        if query.is_empty() || snippet.name.to_lowercase().contains(&query) || snippet.group_label().to_lowercase().contains(&query) {
                            groups.entry(snippet.group_label()).or_default().push(snippet);
                        }
                    }
                    if groups.is_empty() {
                        ui.label(if store.snippets.is_empty() { "No snippets yet. Choose New to save a command template." } else { "No matching snippets." });
                    }
                    for (group, mut snippets) in groups {
                        snippets.sort_by_key(|s| s.name.to_lowercase());
                        egui::CollapsingHeader::new(group).default_open(true).show(ui, |ui| {
                            for snippet in snippets {
                                if ui.selectable_label(self.selected == Some(snippet.id), &snippet.name).clicked() {
                                    self.selected = Some(snippet.id);
                                }
                            }
                        });
                    }
                }
                Page::Editor(snippet) => {
                    ui.label("Save commands as text. Use {{interface}} or {{vlan}} for temporary variables; {{{{ produces literal {{.");
                    let name = ui.label("Snippet name");
                    ui.add(egui::TextEdit::singleline(&mut snippet.name).char_limit(256).desired_width(f32::INFINITY)).labelled_by(name.id);
                    let group = ui.label("Group (optional)");
                    ui.add(egui::TextEdit::singleline(&mut snippet.group).char_limit(128).desired_width(f32::INFINITY)).labelled_by(group.id);
                    let template = ui.label("Command template");
                    ui.add(egui::TextEdit::multiline(&mut snippet.template).code_editor().desired_rows(8).desired_width(f32::INFINITY).char_limit(65536)).labelled_by(template.id);
                    if let Err(error) = snippet.validate() { ui.colored_label(ui.visuals().error_fg_color, error); }
                }
                Page::Review { snippet, template, values } => {
                    ui.label(format!("{} / {}", snippet.group_label(), snippet.name));
                    ui.label("Review every command before sending. Variable values are temporary and are never saved.");
                    for name in &template.variables {
                        let label = ui.label(format!("Variable: {name}"));
                        ui.add(egui::TextEdit::singleline(values.entry(name.clone()).or_default()).desired_width(f32::INFINITY).char_limit(65536)).labelled_by(label.id);
                    }
                    expanded = template.expand(values);
                    match &expanded {
                        Ok(text) => {
                            let label = ui.label("Commands preview");
                            let mut display = text.clone();
                            ui.add(egui::TextEdit::multiline(&mut display).code_editor().interactive(false).desired_rows(text.lines().count().clamp(2, 8)).desired_width(f32::INFINITY)).labelled_by(label.id);
                        }
                        Err(error) => { ui.colored_label(ui.visuals().error_fg_color, error); }
                    }
                }
                Page::Draft(text) => {
                    ui.label("Insert created this editable local draft. Nothing is sent until you choose Send commands.");
                    let label = ui.label("Draft commands");
                    ui.add(egui::TextEdit::multiline(text).code_editor().desired_rows(10).desired_width(f32::INFINITY).char_limit(65536)).labelled_by(label.id);
                    if let Err(error) = validate_commands(text) { ui.colored_label(ui.visuals().error_fg_color, error); }
                }
                Page::Transfer { export, rows, selected, notes } => {
                    ui.label(if *export { "Choose templates to export. Review their contents for secrets before sharing." } else { "Choose templates to add. Existing snippets are preserved; importing sends nothing." });
                    ui.label(format!("{} of {} selected", selected.iter().filter(|s| **s).count(), rows.len()));
                    ui.horizontal(|ui| {
                        if ui.button("Select all").clicked() { selected.fill(true); }
                        if ui.button("Select none").clicked() { selected.fill(false); }
                    });
                    for (row, selected) in rows.iter().zip(selected) {
                        ui.checkbox(selected, format!("{} / {}", row.group_label(), row.name));
                        egui::CollapsingHeader::new("Template").id_salt((&row.group, &row.name)).show(ui, |ui| { ui.monospace(&row.template); });
                    }
                    for note in notes { ui.label(note.as_str()); }
                }
            }
        });
        ui.separator();
        match &self.page {
            Page::Library => {
                let snippet = self.selected.and_then(|id| store.get(id));
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(snippet.is_some() && target_valid, egui::Button::new("Use snippet")).clicked()
                        && let Some(snippet) = snippet
                        && let Ok(template) = Template::parse(&snippet.template)
                    {
                        next = Some(Page::Review { snippet: snippet.clone(), template, values: BTreeMap::new() });
                    }
                    if ui.add_enabled(writable && snippet.is_some(), egui::Button::new("Edit snippet")).clicked() {
                        next = snippet.cloned().map(Page::Editor);
                    }
                    if ui.add_enabled(writable, egui::Button::new("New")).clicked() {
                        next = Some(Page::Editor(Snippet::default()));
                    }
                    if ui.add_enabled(writable && snippet.is_some(), egui::Button::new("Delete snippet")).clicked() {
                        action = snippet.map(|s| Action::Delete(s.id));
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(writable, egui::Button::new("Import…")).clicked() {
                        action = Some(Action::ImportFile);
                    }
                    if ui.add_enabled(!store.snippets.is_empty(), egui::Button::new("Export…")).clicked() {
                        next = Some(Page::Transfer {
                            export: true,
                            rows: store.snippets.clone(),
                            selected: store
                                .snippets
                                .iter()
                                .map(|s| self.selected.is_none_or(|id| id == s.id))
                                .collect(),
                            notes: Vec::new(),
                        });
                    }
                    if ui.button("Close").clicked() {
                        action = Some(Action::Close);
                    }
                });
            }
            Page::Editor(snippet) => {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(writable && snippet.validate().is_ok(), egui::Button::new("Save snippet"))
                        .clicked()
                    {
                        action = Some(Action::Save(snippet.clone()));
                    }
                    if ui.button("Cancel").clicked() {
                        next = Some(Page::Library);
                    }
                });
            }
            Page::Review { .. } => {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(target_valid && expanded.is_ok(), egui::Button::new("Insert into draft"))
                        .clicked()
                    {
                        next = expanded.as_ref().ok().cloned().map(Page::Draft);
                    }
                    if ui.add_enabled(target_valid && expanded.is_ok(), egui::Button::new("Send commands")).clicked() {
                        action = expanded.as_ref().ok().cloned().map(Action::Send);
                    }
                    if ui.button("Back").clicked() {
                        next = Some(Page::Library);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(Action::Close);
                    }
                });
            }
            Page::Draft(text) => {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            target_valid && validate_commands(text).is_ok(),
                            egui::Button::new("Send commands"),
                        )
                        .clicked()
                    {
                        action = Some(Action::Send(text.clone()));
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(Action::Close);
                    }
                });
            }
            Page::Transfer { export, rows, selected, .. } => {
                ui.horizontal_wrapped(|ui| {
                    let label = if *export { "Export selected…" } else { "Import selected" };
                    if ui
                        .add_enabled(selected.iter().any(|s| *s) && (*export || writable), egui::Button::new(label))
                        .clicked()
                    {
                        let rows = rows.iter().zip(selected).filter(|(_, s)| **s).map(|(r, _)| r.clone()).collect();
                        action = Some(if *export { Action::Export(rows) } else { Action::Import(rows) });
                    }
                    if ui.button("Cancel").clicked() {
                        next = Some(Page::Library);
                    }
                });
            }
        }
        if let Some(next) = next {
            self.page = next;
            self.reset_scroll = true;
        }
        action
    }
}
