# Dinosaur mode and truck lasso — design specification

- **Date:** 2026-09-25
- **Status:** Implemented and deployed 2026-09-25 (`3b525e6`, density raised in `2ce9d92`).
  Differences from this draft: about half the cells hold a herd (not one in six), the HUD also
  points to the nearest dinosaur, and herbivores don't yet prefer grass/shrub fuel classes
  (§4 "favour ... fuel classes" is not implemented)
- **Scope:** Viewer only. Toggleable roaming dinosaurs around the player's truck, and a lasso
  item on the truck that hog-ties them. No relay changes.

## 1. Outcome

Pressing **J** turns dinosaur mode on or off. When it's on, herds of low-poly dinosaurs walk,
graze, hunt and roam on the terrain around the truck. Pressing **Q** readies the lasso on the
truck. Driving a full circle around a dinosaur while the lasso is ready throws the net over it
(connected); a second full circle tightens it (hog-tied), and the dinosaur drops onto its side,
legs bound, and stays put. The HUD counts dinosaurs hog-tied this session.

## 2. Decisions (from review)

| Decision | Choice |
|---|---|
| Multiplayer | **Local first.** Each player simulates their own dinosaurs. Herds spawn from a shared seed, so everyone starts with the same herds in the same places; movement and hog-tie state can then differ between players. |
| Process | This short spec, then implementation after approval. |

Shared, owner-streamed dinosaurs are a possible follow-up after the relay backpressure work;
nothing here should block it (dinosaur ids are deterministic, see §4).

## 3. Species

Built from primitives (boxes, capsules, cones), like the truck: one shared mesh set and
material per species, animated by moving parts on the CPU.

| Species | Length | Diet | Behaviour | Herd size |
|---|---|---|---|---|
| Tyrannosaurus rex | 12 m | Carnivore | Roams alone; stalks and chases herbivores; eats | 1 |
| Velociraptor | 2 m | Carnivore | Fast packs; curious about the truck, circle it at a distance | 3–6 |
| Stegosaurus | 9 m | Herbivore | Grazes slowly in groups; flees the T. rex | 2–5 |
| Triceratops | 8 m | Herbivore | Grazes; faces down threats instead of fleeing | 2–4 |
| Brachiosaurus | 22 m, 12 m tall | Herbivore | Very slow; browses at head height; ignores most things | 1–3 |

Colours are muted earth tones per species with a slight per-animal hue variation.

## 4. Spawning

- The world is divided into 1 km cells on the EPSG:5070 grid. A cell's contents come from a hash
  of its cell coordinates, so every player gets the same herds in the same cells.
- About half the cells hold a herd (raised from one in six after play-testing: too sparse to find). The species is drawn with weights (herbivores common,
  T. rex rare) and constrained by ground: herbivores favour grass and shrub fuel classes from
  the existing LANDFIRE fuel map where available; nothing spawns on water, rock-only cells,
  or slopes steeper than 30°.
- Herds exist for cells within 1.5 km of the truck and are removed beyond 2 km. At most 40
  dinosaurs are alive at once; the nearest cells win.
- A dinosaur's id is `(cell, index)`. A hog-tied dinosaur stays tied if its cell is revisited
  in the same session (kept in a small set, capped at 500 ids).
- Dinosaurs appear only near a placed truck (drive mode). In map mode they're hidden.

## 5. Behaviour

Each dinosaur runs a small state machine at the 120 Hz fixed step, with decisions re-evaluated
a few times per second.

| State | Entered when | Does |
|---|---|---|
| Wander | Default | Walks toward random points within 150 m of its herd centre |
| Graze / Eat | Randomly, or after a carnivore catches prey | Stops; head down; chewing animation for 5–20 s |
| Flee | Truck within 40 m moving faster than 20 km/h, or a carnivore within 60 m (herbivores) | Runs directly away at top speed |
| Chase | Carnivore sees a herbivore within 120 m (T. rex) or the truck within 80 m (raptors, circling only) | Runs at the target; a catch sends the prey to Down and the carnivore to Eat |
| Down | Caught by a carnivore | Lies on its side; removed after 60 s; the cell respawns it later |
| Netted | Lasso connected (§6) | Tugs against the rope; speed halved; can't move beyond the rope length from the truck |
| Hog-tied | Lasso tightened (§6) | On its side, legs bound together; never moves again this session |

Movement follows the terrain height (`Terrain::height_at`) and turns smoothly. Walk and run
speeds per species (for example T. rex 8/25 km/h, raptor 10/60 km/h, brachiosaurus 3/8 km/h).
Legs swing with speed, tails sway and heads bob; Eat lowers the head and chews.

**Truck contact:** each dinosaur is a capsule that pushes the truck the same way remote trucks
do. A running T. rex or triceratops hits hard enough to flip the truck; being flipped by a
dinosaur shows "Flattened by a T. rex" and uses the existing R reset (it's not the multiplayer
GAME OVER).

## 6. Lasso

**Item:** the truck always carries a coiled yellow rope on a short pole behind the cab. That is
its visual identifier; other players see it on your truck too because it's part of the truck
model. While the lasso is ready, the coil glows and a loop of rope spins above the pole.

**Controls:** Q toggles the lasso ready/stowed. It stows itself after a hog-tie, or after 60 s
with no target.

**Targeting:** while ready, the target is the nearest untied dinosaur within
`lasso radius = 25 m + 1.5 × body length`. The target is outlined, and the HUD shows its
species and a loop-progress ring.

**Loops:** the relay isn't involved; all of this is local.

1. Track the truck's angle around the target: `θ = atan2(truck − dinosaur)` on the ground plane.
   Each frame, add the wrapped change in θ to a running sweep.
2. The sweep counts only while the truck stays within the lasso radius and keeps moving
   (above 8 km/h). Leaving the radius, stopping for 2 s, or changing target resets the sweep.
3. Either direction works, but reversing subtracts: the sweep's absolute value is what counts.
4. **First full loop (|sweep| ≥ 360°):** the net connects. A rope is drawn from the pole to the
   dinosaur's neck, and the dinosaur becomes Netted. The sweep restarts.
5. **Second full loop while connected:** the rope tightens and the dinosaur becomes Hog-tied.
   The HUD count goes up, and the rope detaches after a short cinch animation.
6. **Breaking free:** while Netted, if the truck is more than `rope length = lasso radius + 15 m`
   from the dinosaur, the rope snaps and the dinosaur returns to Flee. A T. rex also snaps it
   after 20 s of tugging unless it's hog-tied first. Raptors are small enough that the circle
   can be tight.

The dinosaur's own movement makes loops harder: fleeing animals must be herded, and a
triceratops turns to face the truck.

## 7. Controls and HUD

| Key | Action |
|---|---|
| J | Dinosaur mode on/off (off by default) |
| Q | Ready/stow the lasso (drive mode only) |

HUD line while dinosaur mode is on: `DINOS 23 nearby | hog-tied 3 | lasso READY → Stegosaurus 210°/360°`.
The controls line gains `J dinosaurs | Q lasso`.

## 8. Performance

At most 40 dinosaurs, with shared meshes and materials (about 12 parts each, so under 500 draw
entities). Behaviour decisions run at 5 Hz; motion runs at the fixed step. Target: no measurable
frame-time change on the desktop, and under 1 ms on the WebGL2 build (checked with the HUD
diagnostics line).

## 9. Acceptance checks

| Check | Required observation |
|---|---|
| Spawn determinism | The same cell yields the same species, count and positions on two runs; water and steep cells spawn nothing. |
| Sweep | Unit tests: a full circle adds 360°; a circle with a reversal in the middle doesn't complete; leaving the radius resets; wrap-around at ±180° is handled. |
| Lasso sequence | Scripted test: loop → Netted, second loop → Hog-tied; overshooting the rope length while Netted snaps it. |
| Behaviour | A herbivore flees a truck driven at it; a T. rex near a herd chases, catches, and eats. |
| Toggle | J removes every dinosaur entity and restores the frame time to the dino-off baseline. |
| Browser | The WebGPU and WebGL2 builds run with dinosaur mode on without errors; the frame-time p99 stays within the §8 budget. |
| Autopilot | A native autopilot run with dinosaur mode on saves screenshots of a herd, a netted dinosaur and a hog-tied one. |

## 10. Out of scope

Shared dinosaurs across players, dinosaurs affecting the fire model, sounds, dinosaur attacks
on remote trucks, saving hog-tie counts across sessions, and the leaderboard (a "most hog-tied"
board could be added to the leaderboard spec later).

## 11. Implementation breakdown

| File | Responsibility |
|---|---|
| `viewer/src/dinos/mod.rs` (new) | Plugin, J toggle, spawning/despawning by cell, the 40-dinosaur cap |
| `viewer/src/dinos/species.rs` (new) | Species table, procedural models, per-part animation |
| `viewer/src/dinos/brain.rs` (new) | State machine, steering on terrain, truck contact |
| `viewer/src/lasso.rs` (new) | Truck lasso item, Q toggle, targeting, loop sweep, rope rendering |
| `viewer/src/truck.rs` | Lasso pole and coil on the model; dinosaur contact forces |
| `viewer/src/main.rs` | Plugin registration, HUD line, controls text |
| `viewer/README.md` | Controls and the local-first note |
