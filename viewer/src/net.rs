//! Multiplayer: every client streams its truck (absolute EPSG:5070 position) through a tiny
//! WebSocket relay; each client simulates only its own truck and pushes off the others.
//! A floating label above every truck shows live network stats.

use crate::truck::{Truck, spawn_model};
use crate::{WorldOrigin, world_pos};
use bevy::math::DVec2;
use bevy::prelude::*;
use ewebsock::{WsEvent, WsMessage, WsReceiver, WsSender};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Pose updates per second sent by each client.
pub const SEND_HZ: f32 = 20.0;

#[derive(Serialize, Deserialize)]
#[serde(tag = "t")]
enum Msg {
    #[serde(rename = "welcome")]
    Welcome { id: u64 },
    #[serde(rename = "s")]
    State { #[serde(default)] id: u64, name: String, x: f64, y: f64, h: f32, q: [f32; 4], v: [f32; 3], b: bool },
    #[serde(rename = "leave")]
    Leave { id: u64 },
    #[serde(rename = "over")]
    Over { #[serde(default)] id: u64, by: String },
    /// Echoed by the relay to the sender only; `c` is the client clock in ms.
    #[serde(rename = "ping")]
    Ping { c: f64 },
}

/// Rolling per-second traffic counters.
#[derive(Default, Clone)]
pub struct Rate {
    msgs: u32,
    bytes: u64,
    window: f32,
    pub msgs_per_s: f32,
    pub bytes_per_s: f32,
}

impl Rate {
    fn add(&mut self, bytes: usize) {
        self.msgs += 1;
        self.bytes += bytes as u64;
    }
    fn tick(&mut self, dt: f32) {
        self.window += dt;
        if self.window >= 1.0 {
            self.msgs_per_s = self.msgs as f32 / self.window;
            self.bytes_per_s = self.bytes as f32 / self.window;
            *self = Rate { msgs_per_s: self.msgs_per_s, bytes_per_s: self.bytes_per_s, ..default() };
        }
    }
}

pub struct Remote {
    pub id: u64,
    pub name: String,
    pub albers: DVec2,
    pub h: f32,
    /// Position in the current world frame.
    pub pos_world: Vec3,
    pub target_rot: Quat,
    pub vel: Vec3,
    pub boosting: bool,
    pub entity: Entity,
    pub beacon: Entity,
    pub label: Entity,
    /// Seconds since the last update.
    pub seen: f32,
    pub rate: Rate,
    /// Smoothed |interval − expected| between updates, seconds.
    pub jitter: f32,
    /// Distance between the drawn (smoothed) and last reported position, metres.
    pub smoothing_err: f32,
    pub total_bytes: u64,
}

#[derive(Resource, Default)]
pub struct Remotes {
    pub trucks: HashMap<u64, Remote>,
    pub my_id: u64,
    pub name: String,
    pub connected: bool,
    pub log: Vec<(String, f32)>,
    pub up: Rate,
    pub down: Rate,
    pub rtt_ms: Option<f32>,
    pub total_up: u64,
    pub total_down: u64,
}

pub struct Socket {
    tx: WsSender,
    rx: WsReceiver,
    open: bool,
    send_timer: f32,
    ping_timer: f32,
    retry: f32,
}

#[derive(Component)]
pub struct NetLabel(pub Option<u64>);

fn relay_url() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(url) = std::env::var("VR_FIRE_RELAY") {
        return url;
    }
    option_env!("VR_FIRE_RELAY").unwrap_or("wss://chilos.dev/vr_fire/ws").to_string()
}

fn connect() -> Option<Socket> {
    let (tx, rx) = ewebsock::connect(relay_url(), ewebsock::Options::default()).ok()?;
    Some(Socket { tx, rx, open: false, send_timer: 0.0, ping_timer: 0.0, retry: 0.0 })
}

fn spawn_label(commands: &mut Commands, owner: Option<u64>) -> Entity {
    commands
        .spawn((
            NetLabel(owner),
            Text::new(""),
            TextFont { font_size: bevy::text::FontSize::Px(12.0), ..default() },
            TextColor(Color::srgb(0.92, 0.95, 0.92)),
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            Node { position_type: PositionType::Absolute, padding: UiRect::axes(px(6), px(3)), ..default() },
            Visibility::Hidden,
        ))
        .id()
}

pub fn setup(world: &mut World) {
    let seed = bevy::platform::time::Instant::now().elapsed().as_nanos() as u64 ^ (world.entities().len() as u64).wrapping_mul(0x9e37_79b9);
    let n = (seed.wrapping_mul(2654435761) ^ (std::ptr::addr_of!(seed) as u64)) % 10_000;
    world.resource_mut::<Remotes>().name = format!("trucker-{n:04}");
    let mut commands = world.commands();
    spawn_label(&mut commands, None);
    world.flush();
    if let Some(s) = connect() {
        world.insert_non_send(s);
    }
}

fn send(socket: &mut Socket, remotes: &mut Remotes, msg: &Msg) {
    let txt = serde_json::to_string(msg).unwrap();
    remotes.up.add(txt.len());
    remotes.total_up += txt.len() as u64;
    socket.tx.send(WsMessage::Text(txt));
}

#[allow(clippy::too_many_arguments)]
pub fn sync(
    time: Res<Time>,
    socket: Option<NonSendMut<Socket>>,
    mut remotes: ResMut<Remotes>,
    mut truck: ResMut<Truck>,
    origin: Res<WorldOrigin>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut tfs: Query<&mut Transform>,
    mut vis: Query<&mut Visibility>,
    cams: Query<&GlobalTransform, With<Camera3d>>,
) {
    let Some(mut socket) = socket else { return };
    let eye = cams.single().map(|c| c.translation()).unwrap_or(Vec3::ZERO);
    let dt = time.delta_secs();
    let now_ms = time.elapsed_secs_f64() * 1000.0;
    let remotes = &mut *remotes;
    for (_, age) in remotes.log.iter_mut() {
        *age += dt;
    }
    remotes.log.retain(|(_, a)| *a < 8.0);
    while let Some(ev) = socket.rx.try_recv() {
        match ev {
            WsEvent::Opened => {
                socket.open = true;
                remotes.connected = true;
            }
            WsEvent::Closed | WsEvent::Error(_) => {
                socket.open = false;
                remotes.connected = false;
                remotes.rtt_ms = None;
            }
            WsEvent::Message(WsMessage::Text(txt)) => {
                remotes.down.add(txt.len());
                remotes.total_down += txt.len() as u64;
                match serde_json::from_str::<Msg>(&txt) {
                    Ok(Msg::Welcome { id }) => remotes.my_id = id,
                    Ok(Msg::Ping { c }) => {
                        let rtt = (now_ms - c) as f32;
                        remotes.rtt_ms = Some(remotes.rtt_ms.map_or(rtt, |r| r + (rtt - r) * 0.3));
                    }
                    Ok(Msg::State { id, name, x, y, h, q, v, b }) if id != remotes.my_id => {
                        let albers = DVec2::new(x, y);
                        let r = remotes.trucks.entry(id).or_insert_with(|| {
                            let hue = (id as f32 * 67.0) % 360.0;
                            let entity = spawn_model(&mut commands, &mut meshes, &mut mats, Color::hsl(hue, 0.8, 0.5), false);
                            commands.entity(entity).insert(Visibility::Inherited);
                            let beacon = commands
                                .spawn((
                                    Mesh3d(meshes.add(Cylinder::new(1.0, 1.0))),
                                    MeshMaterial3d(mats.add(StandardMaterial {
                                        base_color: Color::hsl(hue, 0.9, 0.6),
                                        emissive: Color::hsl(hue, 0.9, 0.5).to_linear() * 8.0,
                                        unlit: true,
                                        ..default()
                                    })),
                                    Transform::default(),
                                ))
                                .id();
                            let label = spawn_label(&mut commands, Some(id));
                            Remote {
                                id,
                                name: name.clone(),
                                albers,
                                h,
                                pos_world: world_pos(&origin, x, y, h),
                                target_rot: Quat::from_array(q).normalize(),
                                vel: Vec3::ZERO,
                                boosting: false,
                                entity,
                                beacon,
                                label,
                                seen: 1.0 / SEND_HZ,
                                rate: Rate::default(),
                                jitter: 0.0,
                                smoothing_err: 0.0,
                                total_bytes: 0,
                            }
                        });
                        r.jitter += ((r.seen - 1.0 / SEND_HZ).abs() - r.jitter) * 0.1;
                        r.rate.add(txt.len());
                        r.total_bytes += txt.len() as u64;
                        r.name = name;
                        r.albers = albers;
                        r.h = h;
                        r.target_rot = Quat::from_array(q).normalize();
                        r.vel = Vec3::from_array(v);
                        r.boosting = b;
                        r.seen = 0.0;
                    }
                    Ok(Msg::Leave { id }) => {
                        if let Some(r) = remotes.trucks.remove(&id) {
                            for e in [r.entity, r.beacon, r.label] {
                                commands.entity(e).despawn();
                            }
                        }
                    }
                    Ok(Msg::Over { id, by }) => {
                        let who = if id == remotes.my_id {
                            "YOU".to_string()
                        } else {
                            remotes.trucks.get(&id).map(|r| r.name.clone()).unwrap_or(format!("player {id}"))
                        };
                        remotes.log.push((format!("{by} flipped {who} - GAME OVER"), 0.0));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if !socket.open {
        socket.retry += dt;
        if socket.retry > 5.0 && !remotes.connected {
            if let Some(s) = connect() {
                *socket = s;
            }
        }
    }
    remotes.up.tick(dt);
    remotes.down.tick(dt);
    // Smooth remote trucks toward their latest reported pose (dead-reckoned with velocity).
    let mut gone = Vec::new();
    for r in remotes.trucks.values_mut() {
        r.seen += dt;
        r.rate.tick(dt);
        if r.seen > 10.0 {
            gone.push(r.id);
            continue;
        }
        let reported = world_pos(&origin, r.albers.x, r.albers.y, r.h);
        let target = reported + r.vel * r.seen.min(0.25);
        r.pos_world = r.pos_world.lerp(target, (dt * 12.0).min(1.0));
        r.smoothing_err = r.pos_world.distance(reported);
        if let Ok(mut tf) = tfs.get_mut(r.entity) {
            tf.translation = r.pos_world;
            tf.rotation = tf.rotation.slerp(r.target_rot, (dt * 12.0).min(1.0));
        }
        if let Ok(mut tf) = tfs.get_mut(r.beacon) {
            // Tall beacon so players can find each other from the map; it starts well above
            // the truck and is hidden up close so it never blocks the view.
            tf.translation = r.pos_world + Vec3::Y * 1560.0;
            tf.scale = Vec3::new(12.0, 3000.0, 12.0);
        }
        if let Ok(mut v) = vis.get_mut(r.beacon) {
            let want = if r.pos_world.distance(eye) > 1500.0 { Visibility::Inherited } else { Visibility::Hidden };
            if *v != want {
                *v = want;
            }
        }
    }
    for id in gone {
        if let Some(r) = remotes.trucks.remove(&id) {
            for e in [r.entity, r.beacon, r.label] {
                commands.entity(e).despawn();
            }
        }
    }
    // Send our pose at SEND_HZ, a ping every second, and our own GAME OVER once.
    socket.send_timer += dt;
    socket.ping_timer += dt;
    if socket.open && socket.ping_timer >= 1.0 {
        socket.ping_timer = 0.0;
        send(&mut socket, remotes, &Msg::Ping { c: now_ms });
    }
    if socket.open && truck.active && socket.send_timer >= 1.0 / SEND_HZ {
        socket.send_timer = 0.0;
        let b = truck.body;
        let msg = Msg::State {
            id: 0,
            name: remotes.name.clone(),
            x: origin.0.x + b.pos.x as f64,
            y: origin.0.y - b.pos.z as f64,
            h: b.pos.y,
            q: b.rot.to_array(),
            v: b.vel.to_array(),
            b: truck.boosting,
        };
        send(&mut socket, remotes, &msg);
    }
    if socket.open {
        if let Some(by) = truck.game_over.clone() {
            if !truck.game_over_sent {
                truck.game_over_sent = true;
                send(&mut socket, remotes, &Msg::Over { id: 0, by });
            }
        }
    }
}

fn kb(bytes_per_s: f32) -> String {
    if bytes_per_s >= 1000.0 { format!("{:.1} KB/s", bytes_per_s / 1000.0) } else { format!("{bytes_per_s:.0} B/s") }
}

/// Position each truck's network label just above it on screen.
pub fn labels(
    remotes: Res<Remotes>,
    truck: Res<Truck>,
    fixed: Res<Time<Fixed>>,
    cams: Query<(&Camera, &GlobalTransform)>,
    mut q: Query<(&NetLabel, &mut Text, &mut Node, &mut Visibility)>,
) {
    let Ok((cam, cam_tf)) = cams.single() else { return };
    let eye = cam_tf.translation();
    for (label, mut text, mut node, mut vis) in &mut q {
        let (pos, body) = match label.0 {
            None => {
                if !truck.active {
                    *vis = Visibility::Hidden;
                    continue;
                }
                let p = truck.prev.pos.lerp(truck.body.pos, fixed.overstep_fraction());
                let status = if remotes.connected { "online" } else { "offline" };
                let rtt = remotes.rtt_ms.map_or("-".into(), |r| format!("{r:.0} ms"));
                (
                    p,
                    format!(
                        "YOU {} #{}  {status}\nrtt {rtt}  up {:.0} msg/s {}  down {}\nsent {:.1} KB  recv {:.1} KB  speed {:.0} km/h",
                        remotes.name,
                        remotes.my_id,
                        remotes.up.msgs_per_s,
                        kb(remotes.up.bytes_per_s),
                        kb(remotes.down.bytes_per_s),
                        remotes.total_up as f32 / 1000.0,
                        remotes.total_down as f32 / 1000.0,
                        truck.body.vel.length() * 3.6,
                    ),
                )
            }
            Some(id) => {
                let Some(r) = remotes.trucks.get(&id) else {
                    *vis = Visibility::Hidden;
                    continue;
                };
                (
                    r.pos_world,
                    format!(
                        "{} #{}{}\n{:.0} msg/s  {}  last {:.0} ms ago\njitter {:.0} ms  smoothing {:.1} m  total {:.1} KB\n{:.0} m away  {:.0} km/h",
                        r.name,
                        r.id,
                        if r.boosting { "  BOOST" } else { "" },
                        r.rate.msgs_per_s,
                        kb(r.rate.bytes_per_s),
                        r.seen * 1000.0,
                        r.jitter * 1000.0,
                        r.smoothing_err,
                        r.total_bytes as f32 / 1000.0,
                        r.pos_world.distance(eye),
                        r.vel.length() * 3.6,
                    ),
                )
            }
        };
        let far = pos.distance(eye) > 2500.0;
        match cam.world_to_viewport(cam_tf, pos + Vec3::Y * 4.5) {
            Ok(screen) if !far => {
                text.0 = body;
                node.left = px(screen.x - 110.0);
                node.top = px(screen.y - 62.0);
                *vis = Visibility::Inherited;
            }
            _ => *vis = Visibility::Hidden,
        }
    }
}
