//! iroh-gossip's sans-IO protocol (`proto::State`) as a member of SC1's simulator: one topic, the
//! real messages, sized as postcard writes them with the 4-byte length prefix iroh-gossip frames
//! them with, and peers that serialize as 32 bytes, like an endpoint id.

use bytes::Bytes;
use iroh_gossip::proto::{
    Command, Config, Event, InEvent, Message, OutEvent, PeerData, Scope, State, Timer, TopicId,
};
use radio_link::sim::*;
use rand::{SeedableRng, rngs::StdRng};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::OnceLock,
    time::Duration,
};
use tokio::time::Instant;

const TOPIC: TopicId = TopicId::from_bytes([7; 32]);
/// The members a founding member or a woken one dials first: a Radio knows every member from
/// `fleet.db`, so it bootstraps from a few of them.
const BOOTSTRAP: usize = 3;
const FRAME: u32 = 4;
const CHANGE: u8 = 0;
const UPDATE: u8 = 1;
const UPDATE_TIMER: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Peer(Member);
impl Serialize for Peer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&self.0.to_le_bytes());
        bytes.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Peer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = <[u8; 32]>::deserialize(deserializer)?;
        Ok(Peer(Member::from_le_bytes(bytes[..4].try_into().unwrap())))
    }
}

/// Computer-record updates on top of the membership changes: each member broadcasts one of
/// `record_bytes` about `updates_per_day` times a day, at random.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Load {
    pub record_bytes: u32,
    pub updates_per_day: f64,
}

pub struct Gossip {
    state: State<Peer, StdRng>,
    timers: HashMap<u64, Timer<Peer>>,
    next_timer: u64,
    founding: u32,
    payload: u32,
    load: Load,
    updates: u32,
    applied: HashSet<u32>,
    /// Ordered, so a woken member drops its neighbors in the same order on every run.
    neighbors: BTreeSet<Member>,
    most_neighbors: usize,
}

fn at(now: Millis) -> Instant {
    static BASE: OnceLock<std::time::Instant> = OnceLock::new();
    Instant::from_std(*BASE.get_or_init(std::time::Instant::now)) + Duration::from_millis(now)
}

impl Gossip {
    pub fn start(me: Member, boot: Boot, payload: u32, load: Load, ctx: &mut Ctx<'_, Msg>) -> Self {
        let seed = (u64::from(ctx.random(u32::MAX)) << 32) | u64::from(ctx.random(u32::MAX));
        let config = Config {
            max_message_size: (payload.max(load.record_bytes) as usize + 1024).max(4096),
            ..Config::default()
        };
        let founding = match boot {
            Boot::Founding { members } => members,
            Boot::Joining { .. } => 0,
        };
        let mut node = Gossip {
            state: State::new(
                Peer(me),
                PeerData::new(Bytes::new()),
                config,
                StdRng::seed_from_u64(seed),
            ),
            timers: HashMap::new(),
            next_timer: 0,
            founding,
            payload,
            load,
            updates: 0,
            applied: HashSet::new(),
            neighbors: BTreeSet::new(),
            most_neighbors: 0,
        };
        let peers = match boot {
            Boot::Founding { .. } => node.some_members(ctx),
            Boot::Joining { inviter } => vec![Peer(inviter)],
        };
        node.handle(InEvent::Command(TOPIC, Command::Join(peers)), ctx);
        node.next_update(ctx);
        node
    }
    pub fn most_neighbors(&self) -> usize {
        self.most_neighbors
    }
    fn some_members(&self, ctx: &mut Ctx<'_, Msg>) -> Vec<Peer> {
        let mut peers = Vec::new();
        let others = self.founding.saturating_sub(1) as usize;
        while peers.len() < BOOTSTRAP.min(others) {
            let member = ctx.random(self.founding);
            if member != ctx.me() && !peers.contains(&Peer(member)) {
                peers.push(Peer(member));
            }
        }
        peers
    }
    fn next_update(&mut self, ctx: &mut Ctx<'_, Msg>) {
        if self.load.updates_per_day > 0.0 {
            let unit = (f64::from(ctx.random(u32::MAX)) + 1.0) / (f64::from(u32::MAX) + 1.0);
            let mean = 86_400_000.0 / self.load.updates_per_day;
            ctx.after((-unit.ln() * mean) as Millis, UPDATE_TIMER);
        }
    }
    fn broadcast(&mut self, tag: u8, id: u32, bytes: u32, ctx: &mut Ctx<'_, Msg>) {
        let mut content = vec![0u8; bytes.max(9) as usize];
        content[0] = tag;
        content[1..5].copy_from_slice(&ctx.me().to_le_bytes());
        content[5..9].copy_from_slice(&id.to_le_bytes());
        let command = Command::Broadcast(Bytes::from(content), Scope::Swarm);
        self.handle(InEvent::Command(TOPIC, command), ctx);
    }
    fn handle(&mut self, event: InEvent<Peer>, ctx: &mut Ctx<'_, Msg>) {
        let out: Vec<_> = self.state.handle(event, at(ctx.now()), None).collect();
        for event in out {
            match event {
                OutEvent::SendMessage(to, message) => {
                    if to.0 != ctx.me() {
                        ctx.send(to.0, message);
                    }
                }
                OutEvent::ScheduleTimer(delay, timer) => {
                    let token = self.next_timer;
                    self.next_timer += 1;
                    self.timers.insert(token, timer);
                    ctx.after(
                        delay
                            .as_micros()
                            .div_ceil(1000)
                            .try_into()
                            .unwrap_or(Millis::MAX),
                        token,
                    );
                }
                OutEvent::EmitEvent(_, Event::NeighborUp(peer)) => {
                    self.neighbors.insert(peer.0);
                    self.most_neighbors = self.most_neighbors.max(self.neighbors.len());
                }
                OutEvent::EmitEvent(_, Event::NeighborDown(peer)) => {
                    self.neighbors.remove(&peer.0);
                }
                OutEvent::EmitEvent(_, Event::Received(received)) => {
                    let content = &received.content;
                    if content[0] == CHANGE {
                        let id = u32::from_le_bytes(content[5..9].try_into().unwrap());
                        if self.applied.insert(id) {
                            ctx.applied(id);
                        }
                    }
                }
                OutEvent::DisconnectPeer(_) | OutEvent::PeerData(..) => {}
            }
        }
    }
}

pub type Msg = Message<Peer>;

impl Node for Gossip {
    type Msg = Msg;
    fn size(msg: &Msg) -> u32 {
        msg.size().expect("a gossip message has a size") as u32 + FRAME
    }
    fn receive(&mut self, from: Member, msg: Msg, ctx: &mut Ctx<'_, Msg>) {
        self.handle(InEvent::RecvMessage(Peer(from), msg), ctx);
    }
    fn originate(&mut self, change: Change, ctx: &mut Ctx<'_, Msg>) {
        self.applied.insert(change.id);
        self.broadcast(CHANGE, change.id, self.payload, ctx);
    }
    fn timer(&mut self, token: u64, ctx: &mut Ctx<'_, Msg>) {
        if token == UPDATE_TIMER {
            self.updates += 1;
            self.broadcast(UPDATE, self.updates, self.load.record_bytes, ctx);
            self.next_update(ctx);
        } else if let Some(timer) = self.timers.remove(&token) {
            self.handle(InEvent::TimerExpired(timer), ctx);
        }
    }
    fn undelivered(&mut self, to: Member, _: Msg, ctx: &mut Ctx<'_, Msg>) {
        self.handle(InEvent::PeerDisconnected(Peer(to)), ctx);
    }
    /// A Mac that slept lost its connections; when it wakes, every pending timer fires at once, in
    /// the order it was set, due or not.
    fn woke(&mut self, ctx: &mut Ctx<'_, Msg>) {
        for peer in std::mem::take(&mut self.neighbors) {
            self.handle(InEvent::PeerDisconnected(Peer(peer)), ctx);
        }
        let mut timers: Vec<_> = std::mem::take(&mut self.timers).into_iter().collect();
        timers.sort_by_key(|(token, _)| *token);
        for (_, timer) in timers {
            self.handle(InEvent::TimerExpired(timer), ctx);
        }
        if self.founding > 1 {
            let peers = self.some_members(ctx);
            self.handle(InEvent::Command(TOPIC, Command::Join(peers)), ctx);
        }
        self.next_update(ctx);
    }
}
