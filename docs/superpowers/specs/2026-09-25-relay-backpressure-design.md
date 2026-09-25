# Relay backpressure — fix specification

- **Date:** 2026-09-25
- **Status:** Specified; implementation and runtime validation pending
- **Finding:** [F01: unbounded relay queues](../../review/index.html#F01)
- **Source baseline:** `c15e0beb052d4373430d1486e4065f69e49edc63`
- **Scope:** Multiplayer relay outbound buffering, related connection limits, and slow-client isolation

## 1. Outcome

A client that stops reading must not cause relay memory to grow indefinitely or
stall delivery to other players. The relay keeps a bounded amount of pending
data, replaces obsolete poses, and disconnects a client whose output cannot make
progress. Existing viewers continue using the same JSON messages and reconnect
behavior.

This is a design deliverable. F01 remains open until the implementation and the
acceptance checks below pass.

## 2. Existing failure path

In [relay/src/main.rs](../../../relay/src/main.rs):

- Lines 22 and 41 create an unbounded `mpsc::channel<String>` per peer.
- Lines 29–34 clone every broadcast into each recipient's queue.
- Lines 48–51 call blocking `ws.send`; a failure exits only the inner drain loop.
- Welcome and JSON ping writes also discard errors (lines 44 and 60).
- The 20 ms read timeout is installed after the handshake. There is no write
  deadline, admission limit, or application message-rate limit.
- The `< 4096` text check happens after `ws.read()` has assembled the message.

The [service unit](../../../viewer/deploy/vr-fire-relay.service) sets
`MemoryMax=64M` and `Restart=always`. A stalled recipient can therefore contribute
to a restart that disconnects everyone.

The checked-in Tungstenite dependency is 0.30.0. Its locally installed source
confirms that `send` performs `write` followed by `flush`, failed writes may leave
frames buffered, and default message/frame limits are much larger than the
application's 4 KiB check. The design must bound both application and transport
buffers.

## 3. Design decisions

Keep one worker thread owning each WebSocket and the existing synchronous Rust
stack. Replace the outbound channel with a bounded, mutex-protected `Outbox`.
Broadcasting performs bounded in-memory enqueue work; it never waits for queue
capacity or performs socket I/O.

Use a single `VecDeque<Outbound>` with at most 64 entries. For a pose (`t: "s"`),
replace any pending pose from the same sender in place. Other events remain FIFO.
An O(64) scan is acceptable here and avoids introducing another independently
growing index or queue. Serialize each validated broadcast once, then share the
immutable bytes across recipients with `Arc<str>` or an equivalent shared type.

This coalescing matters because the viewer sends poses at 20 Hz and renders the
latest reported pose. A slow consumer benefits from the current state instead of
receiving a backlog of obsolete positions. Control events must not be silently
dropped to make room.

The following defaults are proposed implementation choices, not measurements of
the currently deployed service. Keep them together in a `Limits` configuration
with test overrides. Do not add independently tunable production environment
variables in this fix.

| Limit | Proposed value | Enforcement point |
|---|---:|---|
| Admitted sockets | 32 total, including handshakes and closing workers | Before spawning a worker |
| Outbox capacity | 64 entries per recipient | Every enqueue |
| Text message size | 4,096 bytes maximum | Incoming assembled message and outgoing serialization |
| WebSocket frame payload | 4,096 bytes maximum | Tungstenite frame parser |
| WebSocket read buffer | 4 KiB initial capacity | `WebSocketConfig` |
| WebSocket write buffer | Eager writes; 16 KiB maximum | `write_buffer_size(0)` and `max_write_buffer_size(16 * 1024)` |
| Write deadline | 500 ms for a complete send/flush operation | Underlying I/O wrapper, using monotonic time |
| Read poll interval | 20 ms | Socket/read-loop polling |
| Handshake deadline | 5 seconds from admission | Absolute deadline across all handshake progress |
| Inbound application rate | 40 messages/s; burst capacity 40 | Per-connection token bucket before JSON parsing/fan-out |
| Outbound drain slice | At most 32 messages or 2 ms, checked between messages | Each worker iteration |
| Existing idle timeout | 60 seconds | Monotonic timer checked every worker iteration |

An individual send can take up to the write deadline, even if it starts near the
end of a drain slice. The 2 ms slice is a fairness check, not a hard I/O deadline.
The new 32-socket admission cap is observable behavior: excess connections are
closed before WebSocket admission, and existing viewers can retry normally.

The maximum retained **outbox payload** is conservatively
`32 × 64 × 4,096 = 8 MiB`, without relying on sharing to reduce it. There is at most
one additional dequeued message per worker, plus bounded transport buffers and
short-lived producer serializations. This does not bound the whole process to
8 MiB: stacks, allocator overhead, stats/history, and kernel/proxy buffers are
separate. Verify process memory under the deployment's 64 MiB service limit.

## 4. Message and overflow semantics

| Message | Pending behavior | Recipients |
|---|---|---|
| `welcome` | Send before publishing the peer to the room; failure aborts admission | New peer only |
| `s` | Replace a queued pose from the same sender; otherwise append | All other active peers |
| `over` | Append once, preserving order among control events | All active peers, including sender |
| `leave` | Remove that sender's queued pose, then append once | Remaining active peers |
| JSON `ping` | Echo using the same bounded write path | Sender only |
| WebSocket Ping/Pong/Close | Let Tungstenite handle the protocol; bound implicit writes too | Connection-local |

Only replace a pose that has not been handed to Tungstenite. Never retry a
partially written application message by calling `send` with it again.

Coalescing does not promise replay of every pose or total causal ordering between
poses and events. Control events retain FIFO order relative to one another.
After a sender's `leave` is queued, no later pose from that closed session may be
accepted. This prevents a stale update from recreating a departed truck.

When an enqueue would exceed capacity:

1. If it replaces an existing sender's pending pose, replace without growing the
   queue, even when the queue is full.
2. Otherwise atomically mark **the recipient** closing with reason
   `outbox_full`, clear its pending queue, and reject subsequent enqueues.
3. After releasing queue and registry locks, shut down its socket to interrupt a
   blocked worker. Continue broadcasting to other recipients.

No producer waits for space, spawns an overflow task, or creates a secondary
retry queue. A client exceeding the ingress token bucket is disconnected with
`rate_limit`; the offending message is not forwarded or recorded as gameplay.
Count every incoming text/binary application message, including malformed or
unsupported messages, before parsing. This is an application-message rate limit;
it does not claim protection against arbitrary control-frame or connection floods.

## 5. Ownership, locking and cleanup

Proposed types:

```text
Peers: Mutex<HashMap<PlayerId, Arc<Peer>>>
Peer: Outbox + closing flag/reason + shutdown handle
Outbox: Mutex<VecDeque<Outbound>>, fixed capacity
Outbound: Pose { sender_id, text } | Event { text }
SessionGuard: admission permit + registration/join flags + peer identity
```

The worker exclusively owns the WebSocket. The shutdown handle is a cloned TCP
handle used only to interrupt I/O, not a second reader/writer.

Broadcast snapshots at most 32 peer handles under the registry lock, releases
that lock, and enqueues into one outbox at a time. Outbox critical sections contain
only bounded queue operations. No registry/outbox lock is held across a socket
operation, stats update, disk write, or another outbox lock. Queue helpers release
their mutex before requesting shutdown.

Other workers may request a peer's cancellation, but only the peer's owning
worker performs final removal and emits `leave`. That worker must finish or abort
its in-progress fan-out before emitting its own `leave`. This gives each sender
an ordering boundary without introducing recursive cleanup inside broadcasts.
New enqueues check the recipient's closing state while holding its outbox lock;
stale registry snapshots cannot reopen it.

All exit paths converge on one idempotent cleanup routine:

1. Mark closing and clear pending output; stop processing further input.
2. Shut down the socket outside locks; remove the peer from `Peers` exactly once.
3. If a join was recorded, call `Stats::leave` exactly once and publish one
   `leave` event to remaining peers through the same bounded fan-out path.
4. Release the admission permit when the worker exits. Closing workers continue
   counting toward the connection cap until their resources are released.

Queue overflow during delivery of a `leave` requests cancellation of another
slow recipient; it does not recursively remove peers or rebroadcast inside the
current stack. The cancelled recipient's owner later performs its own cleanup.
Handshake failures and capacity rejections must not produce fictitious player
join/leave events. A failed thread spawn must also release its reserved permit.

## 6. Transport and scheduling

Use `accept_with_config` with explicit frame, assembled-message and buffer limits.
Keep client masking requirements enabled. Check the serialized outbound length
after inserting the server-assigned ID; a small incoming message is not sufficient
proof that the rewritten output fits. Reject the originating input before
fan-out if its serialized result exceeds the outbound limit.

Enforce a **total** write deadline across partial writes and flushes. Setting
`TcpStream::set_write_timeout` once only bounds an individual blocking operation;
it does not establish a deadline across a succession of partial writes. A small
`Read + Write` wrapper around `TcpStream` can carry an operation deadline, check
it on each write, and cap the OS timeout to the remaining duration. The wrapper
must also expose cancellation and identify write failures encountered during
`ws.read()` when Tungstenite flushes an automatic Pong or Close response.

Welcome, pose/event delivery, ping echoes, explicit flushes and automatic protocol
replies all use this policy. Write timeout, write-side `WouldBlock`,
`WriteBufferFull`, cancellation, or another write error terminates the connection.
Do not treat a failed write as an ordinary read-poll timeout or continue adding
messages to Tungstenite's buffered output. In this synchronous design, a write
error is terminal even if the library supports retrying it.

A read-side `WouldBlock`/timeout remains a normal poll result. Check cancellation
and idle expiry regardless of which message was last received. Limit each drain
slice and pop/send one message at a time; do not collect the outbox into an
unbounded temporary batch. Between slices, give incoming messages, heartbeat
handling and cleanup an opportunity to run.

Install handshake bounds before reading any handshake bytes. Its 5-second limit
is absolute, so a slow trickle cannot keep resetting it. Preserve/resume the
handshake state if using nonblocking polling; do not restart parsing each time.
Failure to install required socket limits aborts admission with a diagnostic.
Avoid a dedicated watchdog thread per connection.

## 7. Compatibility and operational evidence

Keep `welcome`, `s`, `over`, `leave`, JSON `ping`, server-assigned IDs, the relay URL,
and stats endpoints compatible with [viewer/src/net.rs](../../../viewer/src/net.rs).
The viewer already reconnects after five seconds when a socket closes. The viewer
needs no code change for this fix. The old text guard accepted fewer than 4,096
bytes; the new explicit maximum accepts exactly 4,096 and rejects anything larger,
including fragmented messages whose assembled size exceeds the limit.

Expose these process-local counters under an additive `relay` object in
`/stats.json`, preserving existing keys: current admitted/active connections,
current pending entries, queue high-water mark, coalesced poses, and disconnect
counts by reason. Define the high-water mark as the maximum entry count observed
in any one outbox (always at most 64); do not conflate it with the room total.
Use fixed counters or aggregate snapshots, not unbounded per-message samples.
Do not count coalesced deliveries as fewer received gameplay updates in the
existing player statistics.

Emit one diagnostic per disconnection with peer ID and reason. Do not log raw
messages, names, or IPs for this diagnostic. Do not increase `MemoryMax` to make
the acceptance test pass. The existing stats name history and JSONL growth are
separate concerns; this fix makes no lifetime memory/disk bound for those stores.

## 8. Acceptance checks

These are implementation requirements, not tests already run for this spec.

| Check | Required observation |
|---|---|
| Exact capacity | A paused consumer never retains more than 64 entries; the 65th nonreplaceable entry cancels only that recipient and clears pending output. |
| Pose replacement | Thousands of queued poses for one sender retain one entry containing the latest pose. Replacement still works at full capacity. |
| Control ordering | `over` and `leave` remain FIFO; `leave` purges a pending pose; no pose from a closed session follows its `leave`. |
| Concurrent close | Concurrent fan-out, queue overflow and socket failure produce one registry removal, one stats leave, one leave broadcast and one permit release. |
| All write paths | Inject failures in welcome, normal send, ping echo, explicit flush and automatic Pong/Close writes. Every failure terminates the whole worker and leaves no retained peer. |
| Absolute deadlines | A fake writer making tiny partial progress still reaches the 500 ms operation deadline. A trickled handshake still expires at 5 seconds. |
| Frame and message bounds | Oversized single frames and oversized fragmented messages are rejected at the configured transport boundary. A rewritten outbound message cannot exceed 4,096 bytes. |
| Rate/fairness | Fake-clock tests prove token-bucket refill/burst behavior. Continuous outbound work cannot starve input checks. Normal 20 Hz poses plus a 1 Hz ping are admitted. |
| Admission recovery | Handshakes, active peers and closing workers share a 32-slot cap. Capacity returns after failed handshake, write error, normal close and failed worker spawn. |

Add a deterministic slow-reader integration test with three clients: producer,
healthy receiver and stalled receiver. After handshake, stop reads on the stalled
connection while the producer sends valid poses at 20 Hz and regular pings.
Require the healthy receiver to keep observing advancing poses and responding to
pings, receive one leave for the stalled peer, and remain connected. Normal
close/`over` propagation must still match the existing protocol.

Do not assert that a stopped reader disconnects immediately: TCP and nginx can
buffer successfully written bytes. Use a deterministic blocked-writer fixture
for precise deadline checks and a loopback test with constrained socket buffers
or an explicitly bounded byte/time budget. Begin the disconnect deadline when a
write stops progressing, not when the client first stops reading. Queue-count
assertions are mandatory; RSS alone is not evidence that the queue is bounded.

For release acceptance, run the release relay with 32 admitted sockets, fixed
player names, a fresh temporary event log and the existing 64 MiB service limit.
Exercise a healthy 20 Hz room, one stalled reader, reconnect cycles, and a sender
that violates the ingress rate. Sample memory and queue gauges during a
two-minute warmup and ten-minute sustained run. Require:

- No service restart/OOM, total retained outbox entries at most `32 × 64`, and
  per-outbox high-water marks at most 64.
- No healthy-peer disconnections in the ordinary 20 Hz workload; fail the run
  if scheduling or chosen limits disconnect healthy peers under the target load.
- After warmup, a memory plateau with no workload-driven upward trend; report
  peak memory and first/last steady-state windows rather than claiming zero
  allocator fluctuation.
- The stalled peer is removed once output blocks and its deadline expires;
  rate violations disconnect the producer, and healthy peers continue advancing.

Exercise the same scenario through local nginx with the checked-in WebSocket
proxy settings before deployment. Separate measurements of relay memory and
nginx buffering. This spec authorizes no production load test or deployment.

## 9. Implementation breakdown

| File | Intended responsibility |
|---|---|
| `relay/src/outbox.rs` (new) | Bounded queue, pose replacement, leave purging, cancellation outcome; deterministic concurrency tests |
| `relay/src/transport.rs` (new) | Explicit Tungstenite configuration, deadline-aware I/O, write-error classification, handshake bounds |
| `relay/src/main.rs` | Admission permits, bounded fan-out, fair worker loop and a single cleanup path |
| `relay/src/stats.rs` | Additive aggregate relay gauges/counters without changing existing gameplay totals |
| `relay/tests/backpressure.rs` or a crate-local test module | Local multi-client isolation and protocol compatibility checks |
| `viewer/README.md` | Document connection cap and overloaded-peer reconnect behavior |

Implement and verify in that order. Prefer clock and transport injection for
deadline tests over long sleeps. Run `cargo test --offline -p relay` and a release
build before the controlled soak; no real terrain requests are needed.

An async-runtime migration, player authentication, authoritative physics, other
review findings and a redesign of stats persistence are outside this fix. A plain
bounded FIFO is a possible smaller patch, but this design includes pose replacement
so temporarily slow clients retain useful current state. Increasing buffer sizes
or the service memory cap alone does not meet the acceptance criteria.

## 10. Completion evidence

Close F01 only when the change includes test results, observed queue limits,
memory measurements, slow-reader cleanup evidence and a protocol compatibility
check. Record the implemented revision and any changes to the proposed defaults.
Link that evidence from the review as a dated follow-up; preserve the original
review snapshot and distinguish local validation from deployment verification.
