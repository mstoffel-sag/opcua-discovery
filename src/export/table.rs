//! Aligned table rendering of discovered nodes.

use crate::export::record::NodeRecord;

/// Render records as a simple aligned text table.
pub fn render(records: &[NodeRecord]) -> String {
    let headers = ["PATH", "CLASS", "DATATYPE", "ACCESS", "SUBSCR", "NODEID"];
    let mut rows: Vec<[String; 6]> = Vec::with_capacity(records.len());
    for r in records {
        rows.push([
            r.browse_path.clone(),
            r.node_class.clone(),
            r.datatype.clone().unwrap_or_default(),
            access_string(r),
            if r.subscribable { "yes" } else { "" }.to_string(),
            r.node_id.clone(),
        ]);
    }

    let mut widths = headers.map(|h| h.len());
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    push_row(&mut out, &headers.map(String::from), &widths);
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    push_row(&mut out, &<[String; 6]>::try_from(sep).unwrap(), &widths);
    for row in &rows {
        push_row(&mut out, row, &widths);
    }
    out
}

fn push_row(out: &mut String, row: &[String; 6], widths: &[usize; 6]) {
    let mut line = String::new();
    for (i, cell) in row.iter().enumerate() {
        if i > 0 {
            line.push_str("  ");
        }
        let pad = widths[i].saturating_sub(cell.chars().count());
        line.push_str(cell);
        line.push_str(&" ".repeat(pad));
    }
    out.push_str(line.trim_end());
    out.push('\n');
}

fn access_string(r: &NodeRecord) -> String {
    let mut s = String::new();
    if r.access_level.map(|a| a & 0x01 != 0).unwrap_or(false) {
        s.push('R');
    }
    if r.access_level.map(|a| a & 0x02 != 0).unwrap_or(false) {
        s.push('W');
    }
    if r.historizing.unwrap_or(false) {
        s.push('H');
    }
    s
}
