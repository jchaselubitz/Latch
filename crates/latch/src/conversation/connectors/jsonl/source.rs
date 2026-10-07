//! Bounded, append-only reads of the two authoritative sources: the hook
//! sidecar and the agent's transcript. Each keeps its own offset; a record
//! is consumed only once its line is complete.
use super::*;

impl JsonlConnector {
    /// Reads complete hook records after `hook_offset`. Hooks carry the
    /// authoritative SessionStart binding and out-of-band permissions.
    pub(super) fn read_hook_sidecar(
        &mut self,
        max_records: usize,
        mutations: &mut Vec<ConnectorMutation>,
        delta: &mut CheckpointDelta,
    ) -> Result<()> {
        let hooks = self.home.session(&self.session).conversation_source_hooks();
        let hook_length = fs::metadata(&hooks).map(|meta| meta.len()).unwrap_or(0);
        if hook_length < self.hook_offset {
            self.hook_offset = 0;
        }
        if hook_length > self.hook_offset {
            let file = File::open(&hooks)
                .with_context(|| format!("open Claude hook sidecar {}", hooks.display()))?;
            let bytes = read_bounded(file, self.hook_offset)?;
            #[cfg(test)]
            {
                self.last_read_bytes += bytes.len();
            }
            let complete = complete_lines_len(&bytes);
            let mut consumed = 0usize;
            for line in bytes[..complete]
                .split_inclusive(|byte| *byte == b'\n')
                .take(max_records)
            {
                consumed += line.len();
                if line.len() > MAX_RECORD_BYTES {
                    self.malformed_records += 1;
                    continue;
                }
                match serde_json::from_slice::<Value>(line) {
                    Ok(value) => {
                        mutations.extend(self.record(value, self.hook_offset + consumed as u64))
                    }
                    Err(_) => self.malformed_records += 1,
                }
            }
            self.hook_offset += consumed as u64;
            delta
                .source_offsets
                .push(crate::conversation::SourceOffset {
                    source: hooks.display().to_string(),
                    offset: self.hook_offset,
                });
        }
        Ok(())
    }

    /// Reads complete transcript records after `offset`, rebuilding first
    /// when the bound file was replaced or truncated.
    pub(super) fn read_transcript(
        &mut self,
        source: PathBuf,
        max_records: usize,
        mutations: &mut Vec<ConnectorMutation>,
        delta: &mut CheckpointDelta,
    ) -> Result<()> {
        let metadata = fs::metadata(&source).ok();
        let identity = metadata.as_ref().map(SourceIdentity::of);
        if self.source_identity.is_some() && identity.is_some() && self.source_identity != identity
        {
            self.offset = 0;
            self.forget_chain();
            self.pending_request = None;
            self.queued_requests.clear();
            self.displaced_requests.clear();
            self.tools.clear();
            self.tool_summaries.clear();
            self.tool_running = false;
            self.turn_open = false;
            self.turn_outcome = None;
            mutations.push(ConnectorMutation::Rebuild {
                reason: "authoritative source file was replaced".to_owned(),
            });
        }
        self.source_identity = identity;
        let length = metadata.map(|meta| meta.len()).unwrap_or(0);
        if length < self.offset {
            self.offset = 0;
            self.forget_chain();
            mutations.push(ConnectorMutation::Rebuild {
                reason: "authoritative source was truncated".to_owned(),
            });
        }
        if length > self.offset {
            let file = File::open(&source)
                .with_context(|| format!("open authoritative {} source", self.id))?;
            let bytes = read_bounded(file, self.offset)?;
            #[cfg(test)]
            {
                self.last_read_bytes += bytes.len();
            }
            let complete = complete_lines_len(&bytes);
            if complete == 0 && bytes.len() == MAX_READ_BYTES {
                // A source line can never consume the worker indefinitely.
                // Skip this bounded malformed fragment and let the next poll
                // continue after it instead of wedging the conversation.
                self.offset += bytes.len() as u64;
                self.malformed_records += 1;
                delta
                    .source_offsets
                    .push(crate::conversation::SourceOffset {
                        source: source.display().to_string(),
                        offset: self.offset,
                    });
            }
            let mut consumed = 0usize;
            for line in bytes[..complete]
                .split_inclusive(|byte| *byte == b'\n')
                .take(max_records)
            {
                consumed += line.len();
                if line.len() > MAX_RECORD_BYTES {
                    self.malformed_records += 1;
                    continue;
                }
                match serde_json::from_slice::<Value>(line) {
                    Ok(value) => {
                        let before = self.active_chain.len();
                        let before_tail = self.active_chain.last().cloned();
                        let record_mutations = self.record(value, self.offset + consumed as u64);
                        for mutation in record_mutations {
                            mutations.push(mutation);
                        }
                        if self.active_chain.len() > before
                            || self.active_chain.last().cloned() != before_tail
                        {
                            // Journal where the record landed in the chain,
                            // not its raw parent: a parallel tool result's
                            // parent is an earlier record it never rewound to.
                            if let Some(source_id) = self.active_chain.last().cloned() {
                                let parent_id = (self.id == "claude")
                                    .then(|| self.active_chain.iter().rev().nth(1).cloned())
                                    .flatten();
                                let item_id = self.chain_items.get(&source_id).cloned();
                                delta
                                    .active_branch_delta
                                    .push(crate::conversation::BranchEntry {
                                        source_id,
                                        parent_id,
                                        item_id,
                                    });
                            }
                        }
                    }
                    Err(_) => self.malformed_records += 1,
                }
            }
            self.offset += consumed as u64;
            delta
                .source_offsets
                .push(crate::conversation::SourceOffset {
                    source: source.display().to_string(),
                    offset: self.offset,
                });
        }
        Ok(())
    }
}

/// At most `MAX_READ_BYTES` of `file` from `from`.
pub(super) fn read_bounded(mut file: File, from: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.take(MAX_READ_BYTES as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The length of the complete lines at the start of `bytes`.
fn complete_lines_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}
