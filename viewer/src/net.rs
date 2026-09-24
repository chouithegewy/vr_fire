//! Multiplayer: every client streams its truck (absolute EPSG:5070 position) through a tiny
//! WebSocket relay; each client simulates only its own truck and pushes off the others.

use crate::truck::{Truck, spawn_model};
use crate::{Anchor, WorldOrigin, world_pos};
use bevy::math::DVec2;
use bevy::prelude::*;
use ewebsock::{WsEvent, WsMessage, WsReceiver, WsSender};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
}

pub struct Remote {
    pub id: u64,
    pub name: String,
    pub albers: DVec2,
    pub h: f32,
    /// Position in the current world frame (updated when the origin moves).
    pub pos_world: Vec3,
    pub target_rot: Quat,
    pub vel: Vec3,
    pub entity: Entity,
    pub beacon: Entity,
    pub seen: f32,
}

#[derive(Resource, Default)]
pub struct Remotes {
    pub trucks: HashMap<u64, Remote>,
    pub my_id: u64,
    pub name: String,
    pub connected: bool,
    pub log: Vec<(String, f32)>,
}

pub struct Socket {
    tx: WsSender,
    rx: WsReceiver,
    open: bool,
    send_timer: f32,
    retry: f32,
}

fn relay_url() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(url) = std::env::var("VR_FIRE_RELAY") {
        return url;
    }
    option_env!("VR_FIRE_RELAY").unwrap_or("wss://chilos.dev/vr_fire/ws").to_string()
}

fn connect() -> Option<Socket> {
    let (tx, rx) = ewebsock::connect(relay_url(), ewebsock::Options::default()).ok()?;
    Some(Socket { tx, rx, open: false, send_timer: 0.0, retry: 0.0 })
}

pub fn setup(world: &mut World) {
    let n = (world.resource::<Time>().elapsed_secs_f64().to_bits() ^ 0x9e37_79b9_7f4a_7c15) % 10_000;
    let seed = (bevy::platform::time::Instant::now().elapsed().as_nanos() as u64).wrapping_add(n);
    world.resource_mut::<Remotes>().name = format!("trucker-{:04}", seed.wrapping_mul(2654435761) % 10_000);
    if let Some(s) = connect() {
        world.insert_non_send(s);
    }
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
) {
    let Some(mut socket) = socket else { return };
    let dt = time.delta_secs();
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
            }
            WsEvent::Message(WsMessage::Text(txt)) => match serde_json::from_str::<Msg>(&txt) {
                Ok(Msg::Welcome { id }) => remotes.my_id = id,
                Ok(Msg::State { id, name, x, y, h, q, v, .. }) if id != remotes.my_id => {
                    let albers = DVec2::new(x, y);
                    let r = remotes.trucks.entry(id).or_insert_with(|| {
                        let hue = (id as f32 * 67.0) % 360.0;
                        let entity = spawn_model(&mut commands, &mut meshes, &mut mats, Color::hsl(hue, 0.8, 0.5));
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
                        Remote {
                            id,
                            name: name.clone(),
                            albers,
                            h,
                            pos_world: Vec3::ZERO,
                            target_rot: Quat::IDENTITY,
                            vel: Vec3::ZERO,
                            entity,
                            beacon,
                            seen: 0.0,
                        }
                    });
                    let first = r.seen == 0.0 && r.pos_world == Vec3::ZERO;
                    r.name = name;
                    r.albers = albers;
                    r.h = h;
                    r.target_rot = Quat::from_array(q).normalize();
                    r.vel = Vec3::from_array(v);
                    r.seen = 0.0;
                    if first {
                        r.pos_world = world_pos(&origin, x, y, h);
                    }
                }
                Ok(Msg::Leave { id }) => {
                    if let Some(r) = remotes.trucks.remove(&id) {
                        commands.entity(r.entity).despawn();
                        commands.entity(r.beacon).despawn();
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
            },
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
    // Smooth remote trucks toward their latest reported pose (dead-reckoned with velocity).
    let mut gone = Vec::new();
    for r in remotes.trucks.values_mut() {
        r.seen += dt;
        if r.seen > 10.0 {
            gone.push(r.id);
            continue;
        }
        let target = world_pos(&origin, r.albers.x, r.albers.y, r.h) + r.vel * r.seen.min(0.25);
        r.pos_world = r.pos_world.lerp(target, (dt * 12.0).min(1.0));
        if let Ok(mut tf) = tfs.get_mut(r.entity) {
            tf.translation = r.pos_world;
            tf.rotation = tf.rotation.slerp(r.target_rot, (dt * 12.0).min(1.0));
        }
        if let Ok(mut tf) = tfs.get_mut(r.beacon) {
            // Tall beacon so players can find each other from map zoom.
            tf.translation = r.pos_world + Vec3::Y * 1500.0;
            tf.scale = Vec3::new(12.0, 3000.0, 12.0);
        }
    }
    for id in gone {
        if let Some(r) = remotes.trucks.remove(&id) {
            commands.entity(r.entity).despawn();
            commands.entity(r.beacon).despawn();
        }
    }
    // Send our state at 20 Hz; announce our own GAME OVER once.
    socket.send_timer += dt;
    if socket.open && truck.active && socket.send_timer >= 0.05 {
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
        socket.tx.send(WsMessage::Text(serde_json::to_string(&msg).unwrap()));
    }
    if socket.open {
        if let Some(by) = truck.game_over.clone() {
            if !truck.game_over_sent {
                truck.game_over_sent = true;
                socket.tx.send(WsMessage::Text(serde_json::to_string(&Msg::Over { id: 0, by }).unwrap()));
            }
        }
    }
    let _ = Anchor(DVec2::ZERO);
}
