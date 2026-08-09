//! Per-client bounded queues, and the resync that replaces buffering.
//!
//! **The PTY read never blocks on a client.** This module is where that is true
//! or false. A phone on a cell network is the ordinary case for this product, not
//! an edge case, and a client that can apply backpressure to the PTY freezes the
//! child process for everyone attached to it — the desk session included.
//!
//! So output is published into a bounded queue per attachment. A queue that
//! overflows is not grown and is not drained by waiting: it is *dropped*, and the
//! client is resynchronized with a fresh screen snapshot. Dropping is correct
//! rather than merely expedient, because the screen model already holds
//! everything the client needs — the snapshot reconstructs exactly what a
//! continuously attached client would be showing, so nothing is lost except
//! intermediate frames the client was too slow to have rendered anyway.
//!
//! [`Fanout::publish`] therefore has no failure mode and no wait. It returns the
//! clients that fell behind, and the caller sends each of them a snapshot.

use std::collections::{BTreeMap, VecDeque};

use crate::worker::registry::AttachmentId;

/// Default per-client queue cap in bytes.
///
/// A megabyte is several screens of dense output — enough to ride out a
/// momentary stall on a good link, small enough that a client on a bad one is
/// resynchronized promptly instead of receiving a long stale replay.
pub const DEFAULT_QUEUE_CAPACITY_BYTES: usize = 1024 * 1024;

/// Default per-client queue cap in chunks.
///
/// A byte cap alone is not enough: an interactive program emits thousands of
/// tiny writes, and a queue of a hundred thousand three-byte chunks is a stalled
/// client whichever way its bytes are counted.
pub const DEFAULT_QUEUE_CAPACITY_CHUNKS: usize = 2048;

/// How a per-client queue is bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    /// Maximum bytes queued for one client.
    pub capacity_bytes: usize,
    /// Maximum chunks queued for one client.
    pub capacity_chunks: usize,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            capacity_bytes: DEFAULT_QUEUE_CAPACITY_BYTES,
            capacity_chunks: DEFAULT_QUEUE_CAPACITY_CHUNKS,
        }
    }
}

/// What a push did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Push {
    /// The chunk was queued.
    Queued,
    /// The queue was over its bound and was dropped; the client owes a snapshot.
    Overflowed {
        /// Bytes discarded.
        dropped_bytes: usize,
        /// Chunks discarded.
        dropped_chunks: usize,
    },
}

impl Push {
    /// Whether this push left the client needing a resync.
    pub fn overflowed(self) -> bool {
        matches!(self, Self::Overflowed { .. })
    }
}

/// What one attachment's delivery looks like from outside.
///
/// Surfaced by `latch inspect`. A silently resynchronizing client and a healthy
/// one are otherwise indistinguishable, and telling them apart is most of
/// diagnosing a bad link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttachmentStats {
    /// Bytes waiting to be written to this client.
    pub queued_bytes: usize,
    /// Chunks waiting to be written.
    pub queued_chunks: usize,
    /// Times this client's queue overflowed and it was resynchronized.
    pub resyncs: u64,
    /// Bytes discarded over this attachment's life.
    pub dropped_bytes: u64,
}

/// One attachment's output queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputQueue {
    config: QueueConfig,
    chunks: VecDeque<Vec<u8>>,
    queued_bytes: usize,
    needs_resync: bool,
    resyncs: u64,
    dropped_bytes: u64,
}

impl OutputQueue {
    /// An empty queue bounded by `config`.
    pub fn new(config: QueueConfig) -> Self {
        Self {
            config,
            chunks: VecDeque::new(),
            queued_bytes: 0,
            needs_resync: false,
            resyncs: 0,
            dropped_bytes: 0,
        }
    }

    /// Queues a chunk, dropping the queue if that would exceed a bound.
    ///
    /// Never blocks, never allocates without bound, and has no error case. The
    /// only thing it can report is that the client fell behind.
    pub fn push(&mut self, chunk: &[u8]) -> Push {
        if self.needs_resync {
            return Push::Queued;
        }

        let exceeds_bytes =
            self.queued_bytes.saturating_add(chunk.len()) > self.config.capacity_bytes;
        let exceeds_chunks = self.chunks.len().saturating_add(1) > self.config.capacity_chunks;
        if exceeds_bytes || exceeds_chunks {
            let dropped_bytes = self.queued_bytes;
            let dropped_chunks = self.chunks.len();
            self.chunks.clear();
            self.queued_bytes = 0;
            self.needs_resync = true;
            self.resyncs = self.resyncs.saturating_add(1);
            self.dropped_bytes = self.dropped_bytes.saturating_add(dropped_bytes as u64);
            return Push::Overflowed {
                dropped_bytes,
                dropped_chunks,
            };
        }

        self.queued_bytes += chunk.len();
        self.chunks.push_back(chunk.to_vec());
        Push::Queued
    }

    /// Takes the next chunk to write to this client.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        let chunk = self.chunks.pop_front()?;
        self.queued_bytes -= chunk.len();
        Some(chunk)
    }

    /// Whether this client is owed a snapshot after an overflow.
    pub fn needs_resync(&self) -> bool {
        self.needs_resync
    }

    /// Records that the owed snapshot has been queued, clearing the flag.
    pub fn resynchronized(&mut self) {
        if self.needs_resync {
            self.needs_resync = false;
        }
    }

    /// This queue's statistics.
    pub fn stats(&self) -> AttachmentStats {
        AttachmentStats {
            queued_bytes: self.queued_bytes,
            queued_chunks: self.chunks.len(),
            resyncs: self.resyncs,
            dropped_bytes: self.dropped_bytes,
        }
    }
}

/// Output distribution to every attachment.
///
/// Owns one [`OutputQueue`] per attachment and nothing else. Keeping the fanout
/// separate from the registry is what allows the central claim to be asserted
/// directly: that [`Fanout::publish`] returns without waiting on anybody, no
/// matter how far behind they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fanout {
    config: QueueConfig,
    queues: BTreeMap<AttachmentId, OutputQueue>,
}

impl Fanout {
    /// An empty fanout whose queues are bounded by `config`.
    pub fn new(config: QueueConfig) -> Self {
        Self {
            config,
            queues: BTreeMap::new(),
        }
    }

    /// Starts queueing output for `id`.
    pub fn register(&mut self, id: AttachmentId) {
        self.queues.insert(id, OutputQueue::new(self.config));
    }

    /// Stops queueing for `id` and discards anything outstanding.
    pub fn unregister(&mut self, id: &AttachmentId) {
        self.queues.remove(id);
    }

    /// Offers a chunk of PTY output to every attachment.
    ///
    /// Returns the attachments whose queues overflowed and which are therefore
    /// owed a snapshot. Never blocks and never fails — this is the call the PTY
    /// read loop makes, and it is the reason a stalled phone cannot freeze the
    /// agent.
    pub fn publish(&mut self, chunk: &[u8]) -> Vec<AttachmentId> {
        self.queues
            .iter_mut()
            .filter_map(|(id, queue)| queue.push(chunk).overflowed().then(|| id.clone()))
            .collect()
    }

    /// Takes the next chunk queued for one attachment.
    pub fn pop(&mut self, id: &AttachmentId) -> Option<Vec<u8>> {
        self.queues.get_mut(id).and_then(OutputQueue::pop)
    }

    /// Queues a snapshot for an attachment that fell behind, clearing its debt.
    ///
    /// The snapshot goes into an emptied queue, so a client resynchronizing on a
    /// link that is still bad cannot accumulate snapshots behind stale output.
    pub fn resynchronize(&mut self, id: &AttachmentId, snapshot: Vec<u8>) {
        let Some(queue) = self.queues.get_mut(id) else {
            return;
        };
        queue.chunks.clear();
        queue.queued_bytes = 0;
        queue.resynchronized();
        let _ = queue.push(&snapshot);
    }

    /// Attachments currently owed a snapshot.
    pub fn pending_resyncs(&self) -> Vec<AttachmentId> {
        self.queues
            .iter()
            .filter(|(_, queue)| queue.needs_resync())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// One attachment's statistics, or `None` if it is not registered.
    pub fn stats(&self, id: &AttachmentId) -> Option<AttachmentStats> {
        self.queues.get(id).map(OutputQueue::stats)
    }
}
