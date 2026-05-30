//! Unicode tree rendering of discovered nodes.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::export::record::NodeRecord;

/// A node in the display tree.
#[derive(Default)]
struct TreeNode {
    children: BTreeMap<String, TreeNode>,
    record: Option<NodeRecord>,
}

/// Render a set of records as a unicode tree to a string.
pub fn render(records: &[NodeRecord]) -> String {
    let mut root = TreeNode::default();
    for record in records {
        insert(&mut root, record);
    }
    let mut out = String::new();
    let child_count = root.children.len();
    for (i, (name, node)) in root.children.iter().enumerate() {
        let last = i + 1 == child_count;
        render_node(&mut out, name, node, "", last);
    }
    out
}

fn insert(root: &mut TreeNode, record: &NodeRecord) {
    let mut cur = root;
    for segment in record.browse_path.split('/') {
        cur = cur.children.entry(segment.to_string()).or_default();
    }
    cur.record = Some(record.clone());
}

fn render_node(out: &mut String, name: &str, node: &TreeNode, prefix: &str, last: bool) {
    let branch = if last { "└─ " } else { "├─ " };
    let _ = write!(out, "{prefix}{branch}{name}");
    if let Some(record) = &node.record {
        let _ = write!(out, "{}", annotation(record));
    }
    out.push('\n');

    let child_prefix = format!("{prefix}{}", if last { "   " } else { "│  " });
    let count = node.children.len();
    for (i, (child_name, child)) in node.children.iter().enumerate() {
        render_node(out, child_name, child, &child_prefix, i + 1 == count);
    }
}

/// Build the bracketed annotation shown after a node name in tree mode.
fn annotation(record: &NodeRecord) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(dt) = &record.datatype {
        parts.push(format!("[{dt}]"));
    }
    let mut access = String::new();
    if record.access_level.map(|a| a & 0x01 != 0).unwrap_or(false) {
        access.push('R');
    }
    if record.access_level.map(|a| a & 0x02 != 0).unwrap_or(false) {
        access.push('W');
    }
    if !access.is_empty() {
        parts.push(format!("[{access}]"));
    }
    if record.subscribable {
        parts.push("[Subscribable]".to_string());
    }
    if let Some(value) = &record.current_value {
        parts.push(format!("= {value}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" {}", parts.join(" "))
    }
}
