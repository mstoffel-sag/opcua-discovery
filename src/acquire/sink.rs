//! Output sink for acquired data.
//!
//! A single sink is shared across all tasks in an acquisition run. It supports
//! both time-series *samples* (poll / subscribe / history) and structural
//! *records* (scan), rendered in one of several formats. Streaming formats
//! (`tedge`, `ndjson`) write each item immediately; batch formats (`json`,
//! `csv`) buffer items and emit them on [`Sink::finish`].

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use serde_json::{Map, Value};

use crate::acquire::config::{OutputConfig, OutputFormat};
use crate::error::{Result, ScanError};
use crate::export::record::NodeRecord;

/// A single time-series sample produced by a poll/subscribe/history task.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Measurement name (alias or browse name).
    pub name: String,
    /// Source node id (string form).
    pub node_id: String,
    /// The value (numeric values stay numeric).
    pub value: Value,
    /// Timestamp (RFC3339), when available.
    pub time: Option<String>,
    /// Status code string, when not Good.
    pub status: Option<String>,
}

/// Shared, thread-safe output sink.
pub struct Sink {
    format: OutputFormat,
    topic: Option<String>,
    state: Mutex<State>,
}

struct State {
    writer: Box<dyn Write + Send>,
    /// Buffered JSON objects for batch formats (json/csv).
    buffer: Vec<Value>,
}

impl Sink {
    /// Build a sink from the resolved output config.
    pub fn new(output: &OutputConfig) -> Result<Self> {
        let writer: Box<dyn Write + Send> = match &output.file {
            Some(path) => open_file(path)?,
            None => Box::new(std::io::stdout()),
        };
        Ok(Sink {
            format: output.format,
            topic: output.topic.clone(),
            state: Mutex::new(State {
                writer,
                buffer: Vec::new(),
            }),
        })
    }

    /// Emit a time-series sample.
    pub fn emit_sample(&self, sample: &Sample) -> Result<()> {
        let mut state = self.lock();
        match self.format {
            OutputFormat::Tedge => {
                let obj = tedge_object(sample);
                self.write_line(&mut state, &obj)
            }
            OutputFormat::Ndjson => {
                let obj = sample_object(sample);
                self.write_line(&mut state, &obj)
            }
            OutputFormat::Json | OutputFormat::Csv => {
                state.buffer.push(sample_object(sample));
                Ok(())
            }
        }
    }

    /// Emit a structural node record (scan task).
    pub fn emit_record(&self, record: &NodeRecord) -> Result<()> {
        let value =
            serde_json::to_value(record).map_err(|e| ScanError::Export(e.to_string()))?;
        let mut state = self.lock();
        match self.format {
            OutputFormat::Tedge | OutputFormat::Ndjson => self.write_line(&mut state, &value),
            OutputFormat::Json | OutputFormat::Csv => {
                state.buffer.push(value);
                Ok(())
            }
        }
    }

    /// Flush any buffered batch output. Must be called once at the end of a run.
    pub fn finish(&self) -> Result<()> {
        let mut state = self.lock();
        match self.format {
            OutputFormat::Json => {
                let text = serde_json::to_string_pretty(&state.buffer)
                    .map_err(|e| ScanError::Export(e.to_string()))?;
                state.writer.write_all(text.as_bytes())?;
                state.writer.write_all(b"\n")?;
            }
            OutputFormat::Csv => {
                let rows = std::mem::take(&mut state.buffer);
                write_csv(&mut state.writer, &rows)?;
            }
            OutputFormat::Tedge | OutputFormat::Ndjson => {}
        }
        state.writer.flush()?;
        Ok(())
    }

    fn write_line(&self, state: &mut State, value: &Value) -> Result<()> {
        let json = serde_json::to_string(value).map_err(|e| ScanError::Export(e.to_string()))?;
        if let Some(topic) = &self.topic {
            writeln!(state.writer, "[{topic}] {json}")?;
        } else {
            writeln!(state.writer, "{json}")?;
        }
        state.writer.flush()?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// thin-edge.io measurement shape: `{ "time": ..., "<name>": <value> }`.
fn tedge_object(sample: &Sample) -> Value {
    let mut map = Map::new();
    if let Some(time) = &sample.time {
        map.insert("time".to_string(), Value::String(time.clone()));
    }
    map.insert(sample.name.clone(), sample.value.clone());
    Value::Object(map)
}

/// Generic sample shape used for json/ndjson/csv.
fn sample_object(sample: &Sample) -> Value {
    let mut map = Map::new();
    if let Some(time) = &sample.time {
        map.insert("time".to_string(), Value::String(time.clone()));
    }
    map.insert("name".to_string(), Value::String(sample.name.clone()));
    map.insert("node_id".to_string(), Value::String(sample.node_id.clone()));
    map.insert("value".to_string(), sample.value.clone());
    if let Some(status) = &sample.status {
        map.insert("status".to_string(), Value::String(status.clone()));
    }
    Value::Object(map)
}

/// Write buffered JSON objects as CSV, using the union of keys (in first-seen
/// order) as the header so heterogeneous rows still align.
fn write_csv(writer: &mut dyn Write, rows: &[Value]) -> Result<()> {
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        if let Value::Object(map) = row {
            for key in map.keys() {
                if !columns.iter().any(|c| c == key) {
                    columns.push(key.clone());
                }
            }
        }
    }

    let mut csv_writer = csv::Writer::from_writer(writer);
    csv_writer
        .write_record(&columns)
        .map_err(|e| ScanError::Export(e.to_string()))?;
    for row in rows {
        let record: Vec<String> = columns
            .iter()
            .map(|col| match row.get(col) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            })
            .collect();
        csv_writer
            .write_record(&record)
            .map_err(|e| ScanError::Export(e.to_string()))?;
    }
    csv_writer
        .flush()
        .map_err(|e| ScanError::Export(e.to_string()))?;
    Ok(())
}

fn open_file(path: &Path) -> Result<Box<dyn Write + Send>> {
    let file = std::fs::File::create(path)?;
    Ok(Box::new(std::io::BufWriter::new(file)))
}
