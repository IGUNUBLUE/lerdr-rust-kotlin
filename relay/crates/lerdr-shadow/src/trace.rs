//! Trace records — one JSON value per line, self-describing so `lerdr-shadow
//! diff` can compare two arbitrary runs without the scenario file.
//!
//! ```text
//! {"kind":"meta","side":"rust","url":"ws://…","scenario":"core",…}
//! {"kind":"step","index":3,"label":"worktree","op":"send","request_id":"req-3",…}
//! {"kind":"tx","t_ms":812,"step":3,"frame":{…}}
//! {"kind":"rx","t_ms":815,"step":3,"frame":{…}}
//! {"kind":"fence","t_ms":900,"label":"end"}
//! {"kind":"note","t_ms":901,"text":"until type=\"x\" not matched"}
//! ```
//!
//! `rx.step` is the index of the step the executor was inside when the frame
//! arrived — the differ's primary bucket attribution. `None` means the frame
//! arrived outside any step window (startup, drain tail).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use crate::scenario::CompareConfig;
use crate::{Result, ShadowError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    /// First line of every trace.
    Meta {
        side: String,
        url: String,
        scenario: String,
        started_ms: i64,
        compare: CompareConfig,
    },
    /// Executor entered a step — `request_id`/`capture` mirror the scenario
    /// so the differ needs no scenario file.
    Step {
        index: usize,
        label: String,
        op: String,
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        capture: Vec<String>,
    },
    /// Plaintext sent to the relay (post-handshake actions only — the
    /// handshake hellos/finish are recorded as `rx`/`note` only when
    /// received, since the client hello bytes are pure key material).
    Tx {
        t_ms: u64,
        step: Option<usize>,
        frame: Value,
    },
    /// Decoded plaintext received from the relay.
    Rx {
        t_ms: u64,
        step: Option<usize>,
        frame: Value,
    },
    Fence {
        t_ms: u64,
        label: String,
    },
    /// Non-fatal observation — a missed `until`, a truncated read, etc.
    Note {
        t_ms: u64,
        text: String,
    },
}

impl Record {
    /// Millisecond timestamp where present (meta/step have none).
    pub fn t_ms(&self) -> Option<u64> {
        match self {
            Record::Tx { t_ms, .. }
            | Record::Rx { t_ms, .. }
            | Record::Fence { t_ms, .. }
            | Record::Note { t_ms, .. } => Some(*t_ms),
            _ => None,
        }
    }
}

/// Append-only JSONL writer; each record flushes so a killed run still
/// leaves a usable trace.
pub struct TraceWriter {
    inner: BufWriter<File>,
}

impl TraceWriter {
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(Self {
            inner: BufWriter::new(File::create(path)?),
        })
    }

    pub fn write(&mut self, record: &Record) -> Result<()> {
        serde_json::to_writer(&mut self.inner, record)?;
        self.inner.write_all(b"\n")?;
        self.inner.flush()?;
        Ok(())
    }
}

/// A parsed trace: meta plus ordered records.
pub struct TraceFile {
    pub meta: Option<Record>,
    pub records: Vec<Record>,
}

impl TraceFile {
    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let mut meta = None;
        let mut records = Vec::new();
        for (line_no, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let record: Record = serde_json::from_str(&line).map_err(|e| {
                ShadowError::msg(format!("{}:{}: {e}", path.display(), line_no + 1))
            })?;
            if let Record::Meta { .. } = record {
                meta = Some(record);
            } else {
                records.push(record);
            }
        }
        Ok(Self { meta, records })
    }

    /// The compare config embedded in `meta`, if any.
    pub fn compare_config(&self) -> Option<&CompareConfig> {
        match &self.meta {
            Some(Record::Meta { compare, .. }) => Some(compare),
            _ => None,
        }
    }

    pub fn side(&self) -> &str {
        match &self.meta {
            Some(Record::Meta { side, .. }) => side,
            _ => "?",
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut w = TraceWriter::create(&path).unwrap();
        w.write(&Record::Meta {
            side: "a".into(),
            url: "ws://x".into(),
            scenario: "s".into(),
            started_ms: 1,
            compare: CompareConfig::default(),
        })
        .unwrap();
        w.write(&Record::Step {
            index: 0,
            label: "l".into(),
            op: "send".into(),
            request_id: Some("r".into()),
            capture: vec!["t".into()],
        })
        .unwrap();
        w.write(&Record::Rx {
            t_ms: 5,
            step: Some(0),
            frame: json!({"type": "x"}),
        })
        .unwrap();
        drop(w);
        let file = TraceFile::load(&path).unwrap();
        assert_eq!(file.side(), "a");
        assert_eq!(file.records.len(), 2);
        assert!(file.compare_config().is_some());
    }
}
