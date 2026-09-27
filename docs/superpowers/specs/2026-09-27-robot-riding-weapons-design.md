# Robot mode, dinosaur riding, first person and weapons — design specification

- **Date:** 2026-09-27
- **Status:** Draft for review. Sections 1–2 were agreed in conversation; sections 3–8 are
  proposed here for the first time
- **Scope:** Viewer (new modes, camera, weapons, HUD) and relay (health, hit validation,
  knockouts). Builds on dinosaur mode ([spec](2026-09-25-dinosaur-mode-design.md)) and the
  bounded relay ([spec](2026-09-25-relay-backpressure-design.md))

## 1. Decisions (from review)

| Decision | Choice |
|---|---|
| Robot | An **original** transforming truck-robot, not Optimus Prime: the game is public, and his name, look and logo are Hasbro trademarks |
| Weapon targets | Other players **and** dinosaurs (dinosaur hits stay local, like the lasso) |
| Getting shot | Health and knockout, then respawn nearby |
| Hit authority | The shooter's client detects hits; the relay keeps health and checks each claim is plausible |
| Sniper | Right mouse zooms to a scope with a reticle |

## 2. Modes and controls (agreed)

Three ways to get around; map mode is unchanged.

| Mode | Movement |
|---|---|
| Truck | As today |
| Robot | Walks ~15 km/h, Shift runs ~40 km/h, Space jumps; follows the terrain; bumps dinosaurs and players like the truck |
| Riding | The robot sits on a dinosaur; WASD steers it at that species' speeds; its behaviour pauses while ridden. Raptors can't be ridden |

| Key | Action |
|---|---|
| X | Transform truck ↔ robot (1.5 s animation: cab → chest, wheels → legs and shoulders, stands up) |
| E | Mount / dismount the nearest rideable dinosaur within ~6 m (robot mode) |
| V | First person ↔ third person |
| Y | Cycle weapons: none → pistol → sniper |
| Left mouse | Fire |
| Right mouse (hold) | Aim; the sniper zooms into its scope |
| Existing | WASD, Space (boost in the truck, jump as the robot), R reset, M map, J dinosaurs, Q lasso (truck only), F lidar |

**First person:** in the robot's head, the truck cab, or just behind the dinosaur's head when
riding. Drawing a weapon switches to first person; holstering (Y back to none) restores the
previous view. The mouse already turns the view (pointer lock); in robot first person it also
turns the robot.

## 3. Robot and transformation

- **Model:** procedural parts like the truck (boxes, cylinders), same green paint, about 7 m
  tall: chest from the cab, arms from the bumpers, legs with the rear wheels at the hips and the
  front wheels on the shoulders, head from the hood scoop.
- **Transform:** each part has a truck pose and a robot pose; the transformation interpolates
  between them over 1.5 s with a slight stagger per part so it reads as unfolding. Movement is
  locked while transforming. It's blocked while riding, and in the air.
- **Movement:** a kinematic character, not rigid-body physics: position follows the terrain
  height, velocity eases toward the input direction, jump is a simple vertical velocity with
  gravity. Collisions with dinosaurs and remote players use capsules (push-out, like the
  dinosaur contact today). The robot can't flip; `R` resets it upright on the ground.
- **Walk cycle:** legs and arms swing with speed, like the dinosaurs' legs.

## 4. Riding

- **Mount:** E in robot mode near a rideable dinosaur (not tied or down) puts the robot on a
  saddle point on its back (per species: height and forward offset).
- **Steering:** WASD turns and moves the dinosaur at its walk speed, Shift at its run speed. It
  follows the terrain with the existing steering code; its AI is suspended and resumes on
  dismount.
- **Dismount:** E again; the robot steps off to the side. Being knocked out also dismounts.
- **Others see you riding:** remote clients spawn a stand-in dinosaur of the reported species
  under your robot, since dinosaurs aren't synchronised.

## 5. Weapons and reticles

| | Pistol | Sniper |
|---|---|---|
| Damage (player) | 20 (headshot 40) | 60 (headshot 100) |
| Fire interval | 0.25 s | 1.5 s (bolt action) |
| Range (relay check) | 300 m | 3,000 m |
| Magazine / reload | 12 / 1.5 s (R reloads while holding a weapon) | 5 / 2.5 s |
| Spread | small, grows while moving | none when scoped, large when not |
| Zoom (right mouse) | 1.3× | 6× (scope) |

- **Aiming:** shots are rays from the camera through the screen centre. A crosshair shows in
  first person; the sniper's scope view draws a full-screen overlay: a black vignette, crosshair
  lines with mil-dots, and a small range readout (distance to what's under the reticle).
- **Hit detection (shooter's client):** the ray is tested against the terrain (stopping at the
  first ground hit), the local dinosaurs' capsules, and remote players' hitboxes (truck box,
  robot body capsule plus head sphere, rider plus dinosaur). The nearest hit wins.
- **Effects:** muzzle flash, a short tracer line, an impact puff on terrain, a hit marker on the
  crosshair when a player is hit (red for a headshot), and a damage flash for the victim.
- **Dinosaurs:** a pistol hit makes a dinosaur flee; three pistol hits or one sniper hit bring
  it down (the existing `Down` state). Local only.
- **Who can shoot:** the robot (on foot or riding). The truck can't shoot; transform first.
- **Key conflict:** R reloads while a weapon is out and resets otherwise.

## 6. Networking and scoring

**Client → relay, new message:**

```json
{"t":"hit","target":17,"weapon":"sniper","head":true}
```

**Relay** (new `combat` module, alongside `stats`):
- Keeps health per player (100 on join and respawn).
- Accepts a hit only if all of these hold:
  - the target is connected, alive, and not the shooter;
  - the shooter's last pose is a robot (or riding);
  - the weapon's fire interval has passed since that shooter's last accepted hit for that
    weapon;
  - the distance between the two last poses is within the weapon's range plus 10%.
- Rejected hits are counted in `/stats.json` (by reason) and otherwise ignored.
- Accepted hits broadcast through the bounded fan-out:
  `{"t":"dmg","target":17,"by":4,"hp":40,"weapon":"sniper","head":true}`.
- At zero health: `{"t":"ko","target":17,"by":4,"weapon":"sniper"}`, and the victim respawns 5 s
  later on its own client (dropped a short distance away, robot mode, full health, announced by
  `{"t":"respawn","target":17}`).
- Health, fire timing and knockout state are bounded per connection and removed on leave.

**Pose message:** gains `k` (`"truck"`, `"robot"` or `"ride:<species>"`), `w` (`"none"`,
`"pistol"` or `"sniper"`) and `hp` (the relay's value echoed back, for labels). All three are
optional, so older viewers keep working and still see a truck.

**Existing flip rule:** flipping another truck still ends a run with GAME OVER; it's separate
from health.

**Stats page:** knockouts per player, kills/deaths in the event log ("trucker-6923 sniped
trucker-1433 (headshot, 812 m)"), and the rejected-hit counters.

## 7. HUD

- A weapon panel: weapon name, ammo `8/12`, reload bar.
- Health bar when in robot or riding mode.
- The crosshair in first person, plus hit markers and the scope overlay.
- A kill feed in the existing event log.
- The controls line gains `X transform | E ride | V view | Y weapon`.

## 8. Testing

| Check | How |
|---|---|
| Transform poses | Unit test: every part reaches its robot pose at t = 1 and its truck pose at t = 0 |
| Robot movement | Unit tests: stays on a sloped test terrain, jump lands, capsule pushes out of a dinosaur |
| Ray hits | Unit tests: the ray stops at terrain before a target behind a hill; nearest of two targets wins; head vs body |
| Weapons | Unit tests: fire interval, magazine and reload, headshot damage |
| Relay validation | Unit tests: each rejection reason; damage → knockout → respawn; state removed on leave |
| Protocol | Integration test on the real relay: two clients, one hits the other until knockout; an old-format client still works |
| End to end | Native autopilot: transform, ride a dinosaur, draw the sniper, screenshot the scope and a hit marker. Web: both backends load with no errors |

## 9. Out of scope

Server-side hit detection, anti-cheat beyond §6, team modes, other weapon types, ammunition
pickups, VR input for weapons, and synchronised dinosaurs.
