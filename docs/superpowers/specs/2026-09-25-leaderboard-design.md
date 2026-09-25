# Driver leaderboard — design specification

- **Date:** 2026-09-25
- **Status:** Draft for review; nothing implemented. Waiting on the §2 decisions
- **Depends on:** [relay backpressure (F01)](2026-09-25-relay-backpressure-design.md). Build this
  after the F01 relay rewrite; every relay-originated message uses its bounded fan-out.
- **Scope:** Timed runs on fixed courses and a top-speed board, measured by the relay, shown in
  the viewer and on the public stats page

## 1. Outcome

Players can drive a marked course and get an official time. The relay keeps the best time per
player name on each course and the highest measured speed, publishes a new record to everyone
in the room, and shows the boards on `/vr_fire/stats`. The boards survive relay restarts.

This is a casual board for a prototype, not a competitive one. Physics runs on each client, so
a modified client can fake any pose. The relay applies plausibility checks (§5) to catch
accidents and naive cheating; it does not claim to prevent a determined cheater.

## 2. Decisions to confirm

These are the choices this draft makes. Each is marked so it can be changed before planning.

| # | Decision | Draft choice | Alternative |
|---|---|---|---|
| D1 | What "fastest time" means | Point-to-point time trials on fixed courses | Laps; free "0 → 200 km/h" acceleration runs |
| D2 | Second board | Top speed, measured by the relay over 1 s | None |
| D3 | Mega boost | Allowed; one open class | Separate "no boost" class (the pose already reports `b`) |
| D4 | Identity | Player name as sent (`trucker-NNNN`), one best entry per name | Player-chosen names; accounts are out of scope |
| D5 | Courses | Three straight courses at existing fly-to places (§3) | Road-following courses with checkpoints |
| D6 | Board size | Top 10 per board | Larger boards, or everyone's personal best |

## 3. Courses

A course is a start gate and a finish gate. A gate is a line segment on the ground in EPSG:5070
(the viewer's world grid), 200 m wide, centred on a point and perpendicular to the course
direction. Courses are straight so they work over open terrain without roads.

Initial courses (centres in lon/lat, converted to EPSG:5070 when loaded):

| Id | Name | Start centre | Direction | Length |
|---|---|---|---|---|
| `placerville` | Placerville sprint | 38.450 N, 120.650 W (fly-to 5) | East | 3 km |
| `death-valley` | Death Valley flat-out | 36.460 N, 116.870 W (fly-to 4) | North | 5 km |
| `yosemite` | Yosemite Valley run | 37.740 N, 119.590 W (fly-to 1) | West | 4 km |

The finish centre is the start centre moved by the course length in the course direction on
the EPSG:5070 grid. Before implementation, drive each course once in the viewer and adjust a
start point if the straight line crosses water or a cliff; record any change in this table.

Courses live in one Rust table in the `vr_fire` crate (`src/courses.rs`) so the relay and the
viewer use the same geometry. A course id never changes meaning once it has records; a changed
course gets a new id.

## 4. Timing

The relay times runs from the poses it already receives (20 Hz, `x`/`y` in EPSG:5070):

1. For each player, keep the previous pose position and the relay's **monotonic receive time**.
2. On each new pose, test the segment from the previous to the new position against every
   course's start and finish gates (segment–segment intersection on the grid).
3. Crossing a start gate in the course direction starts (or restarts) a run on that course.
   Crossing its finish gate in the course direction while a valid run is open finishes it.
4. The crossing time is interpolated along the segment:
   `t = t_prev + (t_new - t_prev) × fraction_along_segment`. Times are recorded in
   milliseconds.

Receive times include network jitter (tens of milliseconds). Interpolation removes the
50 ms pose-interval quantisation; jitter remains and is accepted for a casual board.

Crossing a gate backwards does nothing. A run left open for 10 minutes is discarded.

**Top speed (D2):** the relay measures speed as distance between poses received about one
second apart, divided by the receive-time difference. It never uses the client-reported
velocity `v`. The board keeps each name's highest valid measurement.

## 5. Run validity

A run (and any top-speed sample) is discarded when any of these happen while it is open:

| Check | Rule |
|---|---|
| Gap | No pose from that player for more than 1 s |
| Jump | Implied speed between consecutive poses above 800 km/h (the fastest observed boosted truck reached about 500 km/h) |
| Map | A presence pose (`m: true`) arrives |
| Drop | The existing drop detection fires (a jump of more than 1.5 km) |
| Flip | The player reports GAME OVER (`t: "over"`) |
| Disconnect | The connection closes |

The R key (reset) lifts the truck 3 m in place; it doesn't break a run on its own.

## 6. Protocol

Client → relay: no change. Timing uses the existing `s` poses.

Relay → clients, one new event, sent through the F01 bounded fan-out as an `Event`:

```json
{"t":"rec","board":"placerville","name":"trucker-6923","ms":41250,"rank":1,"best":true}
```

- `board` is a course id or `top-speed` (then `kmh` replaces `ms`).
- Sent to **all** peers when a run enters its board's top 10 (`best: true` when it is also that
  name's new personal best).
- Sent to the **finishing player only** for any other valid finish, so they always see their time.

Older viewers ignore unknown `t` values (they already skip unrecognised messages), so the event
is backward compatible. The `rec` event counts against nothing on the ingress side because it
originates at the relay.

## 7. Viewer

- **Gates:** each gate is drawn as two tall posts (the existing beacon style) at its ends, with
  a banner between them at close range. Start posts are green, finish posts are checkered or
  white.
- **Run timer:** the viewer runs its own timer from its own gate crossings, for display only,
  and shows it large in the HUD while a run is open. When the relay's `rec` event arrives it
  replaces the local time with the official one.
- **Records:** a `rec` event shows a toast for 6 s (`trucker-6923 set a Placerville record: 41.25 s`)
  in the existing game-over log area.
- **Leaderboard panel:** the L key toggles a panel with the top 10 of the nearest course and the
  top-speed board, fetched from `/vr_fire/stats.json` when opened (no new socket traffic).
- **Map:** gates appear on the minimap and in map mode, so players can find courses.

## 8. Storage and stats page

- Boards are stored in `leaderboard.json` next to `RELAY_LOG`. The relay rewrites it after each
  board change: write a temporary file, then rename it over the old one, so a crash never leaves
  a half-written board.
- Each board keeps at most 10 entries with `name`, `ms` (or `kmh`), `at` (Unix seconds) and
  `boost` (whether mega boost was used during the run, for a later D3 split). Memory and disk
  use are fixed.
- A load failure (missing or corrupt file) starts with empty boards and logs one line; it never
  stops the relay.
- `/stats` gains a "Leaderboards" section per board (HTML-escaped names). `/stats.json` gains an
  additive `leaderboard` object; existing keys are unchanged.
- The run history is not stored. The existing JSONL activity log gets one `record` event line
  per top-10 entry. No IP addresses, as with the existing log.

## 9. Acceptance checks

| Check | Required observation |
|---|---|
| Gate crossing | Unit tests: forward crossing detected; backward, parallel and near-miss segments ignored; a segment crossing both gates in one step (a very fast truck) produces start then finish. |
| Interpolation | A synthetic run crossing at known fractions gives the exact expected milliseconds. |
| Validity | Each rule in §5 discards an open run, in its own test. |
| Top speed | Measured from positions and receive times; a spoofed `v` has no effect. |
| Ranking | One entry per name; a slower run never replaces a faster one; ties keep the earlier time; the board never exceeds 10. |
| Persistence | Records survive a relay restart; a corrupt file starts empty boards without crashing; the atomic replace leaves either the old or the new file. |
| Protocol | Older viewers keep working with `rec` events in the stream; new viewers show the toast and the official time. |
| End to end | A headless viewer driven along the Placerville course gets an official time within 0.2 s of its own timer, and the record appears on `/stats` and in the other client's toast. |

## 10. Out of scope

Anti-cheat beyond §5, accounts or chosen names, road-following or lap courses, replays or
ghosts, per-course weather or time of day, and moderation of names (names are already cleaned
and escaped).

## 11. Implementation breakdown

| File | Responsibility |
|---|---|
| `src/courses.rs` (crate `vr_fire`) | Course table, gate geometry, segment crossing and interpolation |
| `relay/src/leaderboard.rs` (new) | Per-player run state, validity rules, boards, persistence |
| `relay/src/main.rs` | Feed poses/over/leave into the leaderboard; send `rec` through the bounded fan-out |
| `relay/src/stats.rs` | Leaderboard section on `/stats` and additive `/stats.json` object |
| `viewer/src/courses.rs` (new) | Gate posts, local run timer, `rec` toasts, L-key panel, minimap gates |
| `viewer/README.md` | Courses, controls and the "casual board" caveat |
