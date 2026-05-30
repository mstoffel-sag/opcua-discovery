//! Interactive terminal explorer (opt-in via `--tui`).
//!
//! Unlike the streaming scan, the TUI browses **lazily**: it only asks the
//! server for a node's children when the user expands that node. This keeps
//! server load minimal while allowing interactive exploration of very large
//! address spaces.
//!
//! Layout: a navigable tree on the left and a detail pane on the right, with a
//! status/help footer. The terminal is always restored on exit or panic via a
//! RAII guard plus a panic hook.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use opcua::client::Session;
use opcua::types::{NodeClass, NodeId, ReferenceDescription};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::error::{Result, ScanError};
use crate::export::{write_export, NodeRecord};
use crate::filters::{is_subscribable, FilterSet};
use crate::opcua::attributes::{read_node_attributes, NodeAttributes};
use crate::opcua::browse::browse_children;
use crate::opcua::namespaces::NamespaceTable;
use crate::opcua::node_class_name;
use crate::opcua::value::read_values;
use crate::subscription;

/// Options controlling the TUI session.
pub struct TuiOptions {
    /// Root node to anchor the tree at.
    pub root: NodeId,
    /// Browse page size.
    pub page_size: u32,
    /// Default export destination for the `e` action.
    pub export_file: Option<PathBuf>,
}

/// Run the interactive explorer until the user quits.
pub async fn run(
    session: Arc<Session>,
    namespaces: Arc<NamespaceTable>,
    filters: Arc<FilterSet>,
    options: TuiOptions,
) -> Result<()> {
    let mut guard = TerminalGuard::new()?;

    let mut app = App::new(session, namespaces, filters, options);
    app.bootstrap().await;

    let mut events = EventStream::new();
    loop {
        app.ensure_selected_detail().await;
        guard
            .terminal
            .draw(|f| app.render(f))
            .map_err(|e| ScanError::Terminal(e.to_string()))?;

        if app.quit {
            break;
        }

        match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                app.handle_key(key).await?;
            }
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(ScanError::Terminal(e.to_string())),
            None => break,
        }
    }

    Ok(())
}

/// RAII terminal guard: enters raw mode + alternate screen on construction and
/// always restores the terminal on drop. Also installs a panic hook so a panic
/// inside the UI does not leave the terminal in a broken state.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> Result<Self> {
        // Restore the terminal first if a panic unwinds through the UI.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            default_hook(info);
        }));

        enable_raw_mode().map_err(|e| ScanError::Terminal(e.to_string()))?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).map_err(|e| ScanError::Terminal(e.to_string()))?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).map_err(|e| ScanError::Terminal(e.to_string()))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// A single node in the lazily-built tree.
struct Node {
    node_id: NodeId,
    label: String,
    class: NodeClass,
    path: String,
    depth: usize,
    parent: Option<usize>,
    children: Vec<usize>,
    loaded: bool,
    expanded: bool,
    expandable: bool,
    attrs: Option<NodeAttributes>,
    value: Option<String>,
}

/// The interactive application state.
struct App {
    session: Arc<Session>,
    namespaces: Arc<NamespaceTable>,
    filters: Arc<FilterSet>,
    options: TuiOptions,
    arena: Vec<Node>,
    roots: Vec<usize>,
    visible: Vec<usize>,
    selected: usize,
    list_state: ListState,
    status: String,
    filter_query: String,
    filter_mode: bool,
    quit: bool,
}

impl App {
    fn new(
        session: Arc<Session>,
        namespaces: Arc<NamespaceTable>,
        filters: Arc<FilterSet>,
        options: TuiOptions,
    ) -> Self {
        Self {
            session,
            namespaces,
            filters,
            options,
            arena: Vec::new(),
            roots: Vec::new(),
            visible: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            status: "↑/↓ move · →/Enter expand · ← collapse · v value · s validate · e export · / filter · q quit".into(),
            filter_query: String::new(),
            filter_mode: false,
            quit: false,
        }
    }

    /// Read the root node and load its top-level children.
    async fn bootstrap(&mut self) {
        let root_id = self.options.root.clone();
        let label = self
            .read_label(&root_id)
            .await
            .unwrap_or_else(|| "Root".into());
        let root = Node {
            node_id: root_id,
            label,
            class: NodeClass::Object,
            path: String::new(),
            depth: 0,
            parent: None,
            children: Vec::new(),
            loaded: false,
            expanded: true,
            expandable: true,
            attrs: None,
            value: None,
        };
        self.arena.push(root);
        self.roots.push(0);
        self.load_children(0).await;
        self.rebuild_visible();
    }

    /// Best-effort display name for a node id.
    async fn read_label(&self, node_id: &NodeId) -> Option<String> {
        let attrs = read_node_attributes(&self.session, std::slice::from_ref(node_id))
            .await
            .ok()?;
        attrs
            .into_iter()
            .next()
            .and_then(|a| a.display_name)
            .filter(|s| !s.is_empty())
    }

    /// Browse and attach the children of `parent_idx`, applying exclude pruning.
    async fn load_children(&mut self, parent_idx: usize) {
        if self.arena[parent_idx].loaded {
            return;
        }
        let parent_id = self.arena[parent_idx].node_id.clone();
        let parent_path = self.arena[parent_idx].path.clone();
        let parent_depth = self.arena[parent_idx].depth;

        let refs = match browse_children(&self.session, &parent_id, self.options.page_size).await {
            Ok(refs) => refs,
            Err(e) => {
                self.status = format!("browse failed: {e}");
                self.arena[parent_idx].loaded = true;
                return;
            }
        };

        let mut child_indices = Vec::new();
        for reference in &refs {
            let node_id = reference.node_id.node_id.clone();
            let segment = segment_of(reference, &node_id);
            let path = if parent_path.is_empty() {
                segment.clone()
            } else {
                format!("{parent_path}/{segment}")
            };

            // Honour exclude patterns: do not show pruned branches.
            if self.filters.path_decision(&path).pruned {
                continue;
            }

            let class = reference.node_class;
            let node = Node {
                node_id,
                label: segment,
                class,
                path,
                depth: parent_depth + 1,
                parent: Some(parent_idx),
                children: Vec::new(),
                loaded: false,
                expanded: false,
                expandable: is_expandable(class),
                attrs: None,
                value: None,
            };
            let idx = self.arena.len();
            self.arena.push(node);
            child_indices.push(idx);
        }

        self.arena[parent_idx].children = child_indices;
        self.arena[parent_idx].loaded = true;
    }

    /// Recompute the flattened list of visible node indices.
    fn rebuild_visible(&mut self) {
        let mut visible = Vec::new();
        for &root in &self.roots {
            collect_visible(&self.arena, root, &mut visible);
        }

        if !self.filter_query.is_empty() {
            let q = self.filter_query.to_lowercase();
            visible.retain(|&idx| self.arena[idx].path.to_lowercase().contains(&q));
        }

        self.visible = visible;
        if self.selected >= self.visible.len() {
            self.selected = self.visible.len().saturating_sub(1);
        }
        self.list_state.select(if self.visible.is_empty() {
            None
        } else {
            Some(self.selected)
        });
    }

    /// Currently selected arena node, if any.
    fn selected_idx(&self) -> Option<usize> {
        self.visible.get(self.selected).copied()
    }

    /// Lazily load attributes for the selected node so the detail pane is filled.
    async fn ensure_selected_detail(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if self.arena[idx].attrs.is_some() {
            return;
        }
        let node_id = self.arena[idx].node_id.clone();
        if let Ok(attrs) = read_node_attributes(&self.session, std::slice::from_ref(&node_id)).await
        {
            self.arena[idx].attrs = attrs.into_iter().next();
        }
    }

    /// Handle a key press.
    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.filter_mode {
            self.handle_filter_key(key);
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('g') => {
                self.selected = 0;
                self.rebuild_visible();
            }
            KeyCode::Char('G') => {
                self.selected = self.visible.len().saturating_sub(1);
                self.rebuild_visible();
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => self.expand_selected().await,
            KeyCode::Left | KeyCode::Char('h') => self.collapse_selected(),
            KeyCode::Char('v') => self.preview_value().await,
            KeyCode::Char('s') => self.validate_selected().await,
            KeyCode::Char('e') => self.export(),
            KeyCode::Char('/') => {
                self.filter_mode = true;
                self.filter_query.clear();
                self.status = "filter: (type to filter, Enter apply, Esc clear)".into();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter_mode = false;
                self.filter_query.clear();
                self.rebuild_visible();
                self.status = "filter cleared".into();
            }
            KeyCode::Enter => {
                self.filter_mode = false;
                self.status = format!("filter: '{}'", self.filter_query);
                self.rebuild_visible();
            }
            KeyCode::Backspace => {
                self.filter_query.pop();
                self.rebuild_visible();
            }
            KeyCode::Char(c) => {
                self.filter_query.push(c);
                self.rebuild_visible();
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: i64) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as i64;
        let next = (self.selected as i64 + delta).clamp(0, len - 1);
        self.selected = next as usize;
        self.list_state.select(Some(self.selected));
    }

    async fn expand_selected(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if !self.arena[idx].expandable {
            self.status = format!("{} is a leaf node", self.arena[idx].label);
            return;
        }
        if !self.arena[idx].loaded {
            self.status = format!("browsing {}…", self.arena[idx].label);
            self.load_children(idx).await;
        }
        self.arena[idx].expanded = true;
        self.rebuild_visible();
        self.status = format!(
            "{} ({} children)",
            self.arena[idx].label,
            self.arena[idx].children.len()
        );
    }

    fn collapse_selected(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if self.arena[idx].expanded && !self.arena[idx].children.is_empty() {
            self.arena[idx].expanded = false;
        } else if let Some(parent) = self.arena[idx].parent {
            // Move selection to the parent and collapse it.
            self.arena[parent].expanded = false;
            self.rebuild_visible();
            if let Some(pos) = self.visible.iter().position(|&v| v == parent) {
                self.selected = pos;
            }
        }
        self.rebuild_visible();
    }

    async fn preview_value(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if self.arena[idx].class != NodeClass::Variable {
            self.status = "value preview only applies to Variables".into();
            return;
        }
        let node_id = self.arena[idx].node_id.clone();
        match read_values(&self.session, std::slice::from_ref(&node_id)).await {
            Ok(values) => {
                let value = values.into_iter().next().and_then(|v| v.value);
                self.arena[idx].value = value.clone();
                self.status = match value {
                    Some(v) => format!("value = {v}"),
                    None => "value = (none)".into(),
                };
            }
            Err(e) => self.status = format!("read failed: {e}"),
        }
    }

    async fn validate_selected(&mut self) {
        let Some(idx) = self.selected_idx() else {
            return;
        };
        if self.arena[idx].class != NodeClass::Variable {
            self.status = "validate only applies to Variables".into();
            return;
        }
        let node_id = self.arena[idx].node_id.clone();
        self.status = "validating subscription…".into();
        match subscription::validate_one(&self.session, &node_id).await {
            Ok(()) => self.status = "subscription OK".into(),
            Err(e) => self.status = format!("not subscribable: {e}"),
        }
    }

    fn export(&mut self) {
        let records: Vec<NodeRecord> = self
            .arena
            .iter()
            .filter(|n| n.parent.is_some())
            .map(|n| self.node_to_record(n))
            .collect();
        if records.is_empty() {
            self.status = "nothing loaded to export yet".into();
            return;
        }

        let path = self
            .options
            .export_file
            .clone()
            .unwrap_or_else(|| PathBuf::from("opcua-scan-export.json"));
        let encoding = encoding_for(&path);
        let mut records = records;
        match write_export(&mut records, encoding, Some(&path)) {
            Ok(()) => {
                self.status = format!("exported {} nodes to {}", records.len(), path.display())
            }
            Err(e) => self.status = format!("export failed: {e}"),
        }
    }

    fn node_to_record(&self, node: &Node) -> NodeRecord {
        let attrs = node.attrs.clone().unwrap_or_default();
        let datatype = attrs.data_type.as_ref().map(|dt| {
            crate::filters::predicate::builtin_datatype_name(dt)
                .map(|s| s.to_string())
                .unwrap_or_else(|| dt.to_string())
        });
        NodeRecord {
            browse_path: node.path.clone(),
            display_name: attrs
                .display_name
                .clone()
                .unwrap_or_else(|| node.label.clone()),
            browse_name: node.label.clone(),
            node_id: node.node_id.to_string(),
            namespace_uri: self.namespaces.display(node.node_id.namespace),
            node_class: node_class_name(node.class).to_string(),
            datatype,
            value_rank: attrs.value_rank,
            access_level: attrs.access_level,
            user_access_level: attrs.user_access_level,
            historizing: attrs.historizing,
            subscribable: is_subscribable(&attrs),
            validation_status: None,
            current_value: node.value.clone(),
            engineering_units: None,
            description: attrs.description.clone(),
        }
    }

    /// Render the whole UI.
    fn render(&mut self, f: &mut Frame) {
        let outer = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        let panes = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(outer[0]);

        self.render_tree(f, panes[0]);
        self.render_detail(f, panes[1]);
        self.render_footer(f, outer[1]);
    }

    fn render_tree(&mut self, f: &mut Frame, area: ratatui::layout::Rect) {
        let items: Vec<ListItem> = self
            .visible
            .iter()
            .map(|&idx| {
                let node = &self.arena[idx];
                let indent = "  ".repeat(node.depth.saturating_sub(1));
                let marker = if node.expandable {
                    if node.expanded {
                        "▼ "
                    } else {
                        "▶ "
                    }
                } else {
                    "  "
                };
                let class = short_class(node.class);
                let line = Line::from(vec![
                    Span::raw(format!("{indent}{marker}")),
                    Span::styled(node.label.clone(), Style::default().fg(Color::White)),
                    Span::styled(format!("  {class}"), Style::default().fg(Color::DarkGray)),
                ]);
                ListItem::new(line)
            })
            .collect();

        let title = if self.filter_query.is_empty() {
            "Address space".to_string()
        } else {
            format!("Address space  [filter: {}]", self.filter_query)
        };
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");
        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_detail(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let mut lines: Vec<Line> = Vec::new();
        if let Some(idx) = self.selected_idx() {
            let node = &self.arena[idx];
            lines.push(detail_line("Browse name", &node.label));
            lines.push(detail_line("Browse path", &node.path));
            lines.push(detail_line("Node id", &node.node_id.to_string()));
            lines.push(detail_line(
                "Namespace",
                &self.namespaces.display(node.node_id.namespace),
            ));
            lines.push(detail_line("Node class", node_class_name(node.class)));

            if let Some(attrs) = &node.attrs {
                if let Some(d) = &attrs.display_name {
                    lines.push(detail_line("Display name", d));
                }
                if let Some(dt) = &attrs.data_type {
                    let name = crate::filters::predicate::builtin_datatype_name(dt)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| dt.to_string());
                    lines.push(detail_line("Data type", &name));
                }
                if let Some(vr) = attrs.value_rank {
                    lines.push(detail_line("Value rank", &vr.to_string()));
                }
                lines.push(detail_line(
                    "Access",
                    &format!(
                        "{}{}{}",
                        if attrs.is_readable() { "read " } else { "" },
                        if attrs.is_writable() { "write " } else { "" },
                        if attrs.is_historizing() {
                            "historizing"
                        } else {
                            ""
                        },
                    ),
                ));
                lines.push(detail_line(
                    "Subscribable",
                    if is_subscribable(attrs) { "yes" } else { "no" },
                ));
                if let Some(desc) = &attrs.description {
                    if !desc.is_empty() {
                        lines.push(detail_line("Description", desc));
                    }
                }
            } else {
                lines.push(Line::from(Span::styled(
                    "(loading attributes…)",
                    Style::default().fg(Color::DarkGray),
                )));
            }

            if let Some(value) = &node.value {
                lines.push(detail_line("Value", value));
            }
        } else {
            lines.push(Line::from("No node selected"));
        }

        let detail = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Details"))
            .wrap(Wrap { trim: true });
        f.render_widget(detail, area);
    }

    fn render_footer(&self, f: &mut Frame, area: ratatui::layout::Rect) {
        let text = if self.filter_mode {
            format!("/{}", self.filter_query)
        } else {
            self.status.clone()
        };
        let footer = Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(Color::Black).bg(Color::Gray),
        )));
        f.render_widget(footer, area);
    }
}

/// Walk the tree depth-first collecting visible (expanded-chain) node indices.
fn collect_visible(arena: &[Node], idx: usize, out: &mut Vec<usize>) {
    out.push(idx);
    if arena[idx].expanded {
        for &child in &arena[idx].children {
            collect_visible(arena, child, out);
        }
    }
}

/// A node class likely to have hierarchical children worth expanding.
fn is_expandable(class: NodeClass) -> bool {
    matches!(
        class,
        NodeClass::Object
            | NodeClass::Variable
            | NodeClass::View
            | NodeClass::ObjectType
            | NodeClass::VariableType
    )
}

/// Short class tag for the tree list.
fn short_class(class: NodeClass) -> &'static str {
    match class {
        NodeClass::Object => "[obj]",
        NodeClass::Variable => "[var]",
        NodeClass::Method => "[mth]",
        NodeClass::ObjectType => "[objT]",
        NodeClass::VariableType => "[varT]",
        NodeClass::ReferenceType => "[refT]",
        NodeClass::DataType => "[dt]",
        NodeClass::View => "[view]",
        NodeClass::Unspecified => "",
    }
}

/// Browse-path segment for a reference (browse name preferred).
fn segment_of(reference: &ReferenceDescription, node_id: &NodeId) -> String {
    let browse = reference.browse_name.name.to_string();
    if !browse.is_empty() {
        return browse;
    }
    let display = reference.display_name.text.to_string();
    if !display.is_empty() {
        return display;
    }
    node_id.to_string()
}

/// A `label: value` detail line.
fn detail_line<'a>(label: &'a str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:>13}: "), Style::default().fg(Color::Cyan)),
        Span::raw(value.to_string()),
    ])
}

/// Pick an export encoding from a file extension.
fn encoding_for(path: &std::path::Path) -> crate::cli::Encoding {
    match path.extension().and_then(|e| e.to_str()) {
        Some("csv") => crate::cli::Encoding::Csv,
        Some("ndjson") => crate::cli::Encoding::Ndjson,
        _ => crate::cli::Encoding::Json,
    }
}
