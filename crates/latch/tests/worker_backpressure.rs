//! Backpressure: the PTY read never blocks on a client.
//!
//! This is the invariant the product rests on, so it is asserted directly rather
//! than assumed from the shape of the code. A phone on a cell network is the
//! ordinary case here, not an edge case; a client that can apply backpressure to
//! the PTY freezes the child process for **everyone** attached to it, including
//! the desk session the user is sitting in front of.
//!
//! The design that makes it impossible rather than unlikely: bounded per-client
//! queues, and an overflow that *drops the queue* and resynchronizes that client
//! with a fresh screen snapshot. Dropping is correct rather than merely
//! expedient — the screen model already holds everything the client needs, so a
//! snapshot reconstructs exactly what a continuously attached client would be
//! showing. Nothing is lost except intermediate frames a stalled client was
//! never going to render.

mod support;

use std::time::{Duration, Instant};

use latch_protocol::control::AttachMode;

use latch::worker::manifest::{TerminalSize, WorkerTuning};
use latch::worker::queue::{Fanout, OutputQueue, QueueConfig};
use latch::worker::registry::AttachmentId;
use support::{last_marker, sh_manifest, size, Harness};

/// A child that floods: large lines, numbered, as fast as a slow link can fail
/// to keep up with.
const FLOODING_CHILD: &str = "PAD=$(printf 'x%.0s' $(seq 1 3900)); i=0; \
     while :; do i=$((i+1)); printf 'TICK:%d %s\\n' \"$i\" \"$PAD\"; sleep 0.01; done";

fn tiny() -> QueueConfig {
    QueueConfig {
        capacity_bytes: 8 * 1024,
        capacity_chunks: 16,
    }
}

#[test]
fn a_queue_past_its_byte_bound_is_dropped_rather_than_grown() {
    let mut queue = OutputQueue::new(tiny());
    let chunk = vec![b'x'; 1024];

    let mut overflowed = false;
    for _ in 0..64 {
        if queue.push(&chunk).overflowed() {
            overflowed = true;
        }
        assert!(
            queue.stats().queued_bytes <= tiny().capacity_bytes,
            "the queue must never hold more than its bound; that bound is the only thing \
             standing between a slow phone and unbounded worker memory"
        );
    }

    assert!(overflowed, "pushing far past the bound must overflow");
    assert!(
        queue.needs_resync(),
        "an overflowed client is owed a snapshot"
    );
}

#[test]
fn a_queue_past_its_chunk_bound_is_dropped_too() {
    let mut queue = OutputQueue::new(tiny());

    // An interactive program emits thousands of tiny writes. A byte cap alone
    // would let a hundred thousand three-byte chunks look healthy.
    let mut overflowed = false;
    for _ in 0..(tiny().capacity_chunks * 4) {
        if queue.push(b"ab").overflowed() {
            overflowed = true;
        }
    }

    assert!(overflowed, "a queue bounded only by bytes is not bounded");
    assert!(queue.stats().queued_chunks <= tiny().capacity_chunks);
}

#[test]
fn an_overflow_is_counted_so_a_bad_link_is_visible() {
    let mut queue = OutputQueue::new(tiny());
    let chunk = vec![b'x'; 4096];
    for _ in 0..16 {
        queue.push(&chunk);
    }
    queue.resynchronized();

    let stats = queue.stats();
    assert!(
        stats.resyncs >= 1,
        "a silently resynchronizing client and a healthy one are otherwise indistinguishable, \
         and telling them apart is most of diagnosing a bad link"
    );
    assert!(
        stats.dropped_bytes > 0,
        "the bytes a client never saw are counted too"
    );
    assert!(
        !queue.needs_resync(),
        "resynchronizing clears the debt; a client owed two snapshots gets one"
    );
}

#[test]
fn a_resync_replaces_the_queue_rather_than_queueing_behind_it() {
    let mut fanout = Fanout::new(tiny());
    let slow = AttachmentId::new("slow");
    fanout.register(slow.clone());

    let chunk = vec![b'x'; 4096];
    for _ in 0..16 {
        fanout.publish(&chunk);
    }
    assert_eq!(fanout.pending_resyncs(), vec![slow.clone()]);

    fanout.resynchronize(&slow, b"SNAPSHOT".to_vec());

    assert_eq!(
        fanout.pop(&slow),
        Some(b"SNAPSHOT".to_vec()),
        "the snapshot goes into an emptied queue: a client resynchronizing on a link that is \
         still bad must not accumulate snapshots behind stale output"
    );
    assert!(fanout.pending_resyncs().is_empty());
}

#[test]
fn publishing_to_a_client_that_never_drains_never_blocks() {
    let mut fanout = Fanout::new(tiny());
    let stalled = AttachmentId::new("stalled");
    fanout.register(stalled.clone());

    let chunk = vec![b'x'; 4096];
    let started = Instant::now();
    for _ in 0..50_000 {
        fanout.publish(&chunk);
    }
    let took = started.elapsed();

    assert!(
        took < Duration::from_secs(5),
        "publish is the call the PTY read loop makes; it has no wait and no failure mode, \
         and it took {took:?}"
    );
    let stats = fanout
        .stats(&stalled)
        .expect("the stalled client is registered");
    assert!(
        stats.queued_bytes <= tiny().capacity_bytes,
        "200 MB was offered to a client that read none of it, and the worker held {} bytes",
        stats.queued_bytes
    );
    assert!(
        stats.resyncs > 0,
        "the stalled client is owed snapshots, repeatedly"
    );
}

#[test]
fn a_stalled_client_does_not_slow_a_healthy_one() {
    let mut fanout = Fanout::new(tiny());
    let stalled = AttachmentId::new("stalled");
    let healthy = AttachmentId::new("healthy");
    fanout.register(stalled.clone());
    fanout.register(healthy.clone());

    let mut delivered = 0usize;
    for n in 0..500u32 {
        fanout.publish(format!("chunk-{n} ").as_bytes());
        while fanout.pop(&healthy).is_some() {
            delivered += 1;
        }
    }

    assert_eq!(
        delivered, 500,
        "the healthy client received everything while the stalled one received nothing; \
         one client's link must not become another client's problem"
    );
    assert!(fanout.stats(&stalled).expect("registered").resyncs > 0);
}

// -- end to end ------------------------------------------------------------

#[test]
fn a_stalled_client_never_freezes_the_child() {
    let harness = Harness::new();
    let session = harness.spawn(support::tuned(
        sh_manifest(FLOODING_CHILD, TerminalSize::new(200, 50)),
        WorkerTuning {
            queue_capacity_bytes: 16 * 1024,
            queue_capacity_chunks: 32,
            ..WorkerTuning::default()
        },
    ));

    // Attached, and then deliberately never read from. This is a phone whose
    // app was backgrounded mid-stream: the socket is open, the kernel buffer
    // fills, and nothing on the other end is coming back soon.
    let _stalled = session.attach(AttachMode::Watch, false, size(40, 20));

    let mut healthy = session.attach(AttachMode::Watch, false, size(200, 50));
    let early = last_marker(&healthy.output_for(Duration::from_millis(800)), "TICK:")
        .expect("the child should be producing output");
    let late = last_marker(&healthy.output_for(Duration::from_millis(800)), "TICK:")
        .expect("the child should still be producing output");

    assert!(
        late > early + 10,
        "the child must keep running at full speed for the desk session while a phone is \
         stalled; saw TICK:{early} then TICK:{late}"
    );
}

#[test]
fn a_stalled_client_is_resynchronized_rather_than_replayed() {
    let harness = Harness::new();
    let session = harness.spawn(support::tuned(
        sh_manifest(FLOODING_CHILD, TerminalSize::new(200, 50)),
        WorkerTuning {
            queue_capacity_bytes: 16 * 1024,
            queue_capacity_chunks: 32,
            ..WorkerTuning::default()
        },
    ));

    let mut stalled = session.attach(AttachMode::Watch, false, size(200, 50));
    let mut healthy = session.attach(AttachMode::Watch, false, size(200, 50));

    // Let the stalled client fall far behind.
    std::thread::sleep(Duration::from_millis(1500));
    let reference = last_marker(&healthy.output_for(Duration::from_millis(300)), "TICK:")
        .expect("the healthy client is keeping up");

    // Now it starts reading again. What it gets is the current screen, not a
    // backlog: it should reach the present almost immediately.
    let caught_up = last_marker(&stalled.output_for(Duration::from_millis(1500)), "TICK:")
        .expect("the resynchronized client should receive output");

    assert!(
        caught_up >= reference,
        "a client that fell behind is brought to the present with a snapshot, not walked \
         through everything it missed; it reached TICK:{caught_up} against TICK:{reference}"
    );
    assert!(
        session.is_live(),
        "and the session is untouched by any of it"
    );
}
