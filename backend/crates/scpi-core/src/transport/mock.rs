//! In-memory transport for tests, dry runs, and scripted demos.
//!
//! A mock can be built programmatically with [`MockTransport::with_response`]
//! or loaded from a JSON file:
//!
//! ```json
//! {
//!   "fallback": "+0",
//!   "responses": {
//!     "*IDN?": "RIGOL TECHNOLOGIES,DM3058E,SN,1.0",
//!     ":MEASure:VOLTage:DC?": ["+1.0E-01", "+1.1E-01"]
//!   }
//! }
//! ```
//!
//! A response array is consumed one item per query and the last item repeats.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::error::{Error, Result};

use super::Transport;

pub struct MockTransport {
    responses: HashMap<String, VecDeque<String>>,
    fallback: String,
    pending: VecDeque<u8>,
    writes: Arc<Mutex<Vec<String>>>,
    description: String,
}

impl MockTransport {
    /// A silent transport with an empty response fallback.
    pub fn empty() -> Self {
        Self {
            responses: HashMap::new(),
            fallback: String::new(),
            pending: VecDeque::new(),
            writes: Arc::new(Mutex::new(Vec::new())),
            description: "mock".into(),
        }
    }

    /// Load a scripted mock from JSON.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|e| {
            Error::Transport(format!("cannot read mock file {}: {e}", path.display()))
        })?;
        Self::from_json(&text, format!("mock:{}", path.display()))
    }

    /// Parse a scripted mock from JSON text.
    pub fn from_json(text: &str, description: impl Into<String>) -> Result<Self> {
        let spec: MockSpec = serde_json::from_str(text)
            .map_err(|e| Error::Transport(format!("invalid mock spec: {e}")))?;
        let mut transport = Self::empty();
        transport.description = description.into();
        transport.fallback = spec.fallback;
        for (query, response) in spec.responses {
            let queue: VecDeque<String> = match response {
                ResponseSpec::One(value) => VecDeque::from([value]),
                ResponseSpec::Many(values) => values.into(),
            };
            if !queue.is_empty() {
                transport.responses.insert(normalize(&query), queue);
            }
        }
        Ok(transport)
    }

    /// Add a response. Adding the same query more than once makes a sequence:
    /// each query consumes one response and the last one repeats.
    pub fn with_response(mut self, query: &str, response: &str) -> Self {
        self.responses
            .entry(normalize(query))
            .or_default()
            .push_back(response.to_string());
        self
    }

    pub fn with_fallback(mut self, response: &str) -> Self {
        self.fallback = response.to_string();
        self
    }

    /// Shared log of everything written to the instrument.
    pub fn writes(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.writes)
    }
}

impl Transport for MockTransport {
    fn description(&self) -> String {
        self.description.clone()
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let text = String::from_utf8_lossy(bytes)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        self.writes.lock().unwrap().push(text.clone());
        if text
            .split_whitespace()
            .next()
            .is_some_and(|head| head.ends_with('?'))
        {
            let key = normalize(&text);
            let response = self
                .responses
                .get_mut(&key)
                .map(|queue| {
                    if queue.len() > 1 {
                        queue.pop_front().unwrap()
                    } else {
                        queue.front().cloned().unwrap()
                    }
                })
                .unwrap_or_else(|| self.fallback.clone());
            self.pending
                .extend(response.trim_end_matches(['\r', '\n']).bytes());
            self.pending.push_back(b'\n');
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut written = 0;
        while written < buf.len() {
            match self.pending.pop_front() {
                Some(byte) => {
                    buf[written] = byte;
                    written += 1;
                }
                None => break,
            }
        }
        Ok(written)
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.read(buf)
    }

    fn clear(&mut self) -> Result<()> {
        self.pending.clear();
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct MockSpec {
    #[serde(default)]
    fallback: String,
    #[serde(default)]
    responses: HashMap<String, ResponseSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ResponseSpec {
    One(String),
    Many(Vec<String>),
}

fn normalize(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}
