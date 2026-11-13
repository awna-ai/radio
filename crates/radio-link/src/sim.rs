use std::{cmp::Ordering, collections::BinaryHeap};

pub type Member = u32;
pub type Millis = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Join,
    Remove,
    Leave,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub id: u32,
    pub kind: Kind,
    pub subject: Member,
    pub origin: Member,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boot {
    Founding { members: u32 },
    Joining { inviter: Member },
}

pub trait Node {
    type Msg;
    /// Bytes this message takes on the wire; the simulator adds nothing to it.
    fn size(msg: &Self::Msg) -> u32;
    fn receive(&mut self, from: Member, msg: Self::Msg, ctx: &mut Ctx<'_, Self::Msg>);
    /// This member starts a change (it is the change's origin).
    fn originate(&mut self, change: Change, ctx: &mut Ctx<'_, Self::Msg>);
    fn timer(&mut self, token: u64, ctx: &mut Ctx<'_, Self::Msg>);
    /// A message this member sent found its receiver asleep or gone.
    fn undelivered(&mut self, to: Member, msg: Self::Msg, ctx: &mut Ctx<'_, Self::Msg>);
    fn slept(&mut self) {}
    /// Every timer was cancelled when the member fell asleep; set again here what should run.
    fn woke(&mut self, _ctx: &mut Ctx<'_, Self::Msg>) {}
}

pub struct Ctx<'a, M> {
    me: Member,
    now: Millis,
    rng: &'a mut Rng,
    out: &'a mut Vec<Output<M>>,
}
impl<M> Ctx<'_, M> {
    pub fn me(&self) -> Member {
        self.me
    }
    pub fn now(&self) -> Millis {
        self.now
    }
    pub fn send(&mut self, to: Member, msg: M) {
        self.out.push(Output::Send(to, msg));
    }
    pub fn after(&mut self, delay: Millis, token: u64) {
        self.out.push(Output::Timer(delay, token));
    }
    pub fn applied(&mut self, change: u32) {
        self.out.push(Output::Applied(change));
    }
    pub fn random(&mut self, below: u32) -> u32 {
        self.rng.below(below)
    }
}

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u32) -> u32 {
        assert!(n > 0, "random needs a bound above zero");
        ((self.next() as u128 * n as u128) >> 64) as u32
    }
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Scenario {
    pub seed: u64,
    pub members: u32,
    pub asleep: f64,
    pub sleep_mean: Millis,
    pub latency: (Millis, Millis),
    pub duration: Millis,
    pub measure_from: Millis,
    pub window: Millis,
    pub changes: Vec<(Millis, Kind)>,
}
impl Scenario {
    pub fn new(seed: u64, members: u32) -> Scenario {
        Scenario {
            seed,
            members,
            asleep: 0.10,
            sleep_mean: 600_000,
            latency: (20, 150),
            duration: 300_000,
            measure_from: 60_000,
            window: 30_000,
            changes: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Traffic {
    pub sent: u64,
    pub sent_bytes: u64,
    pub received: u64,
    pub received_bytes: u64,
    pub undelivered: u64,
    pub online: Millis,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spread {
    pub change: Change,
    pub at: Millis,
    pub targets: u32,
    pub reached: u32,
    pub after_99: Option<Millis>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Sent,
    Received,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub members: u32,
    pub events: u64,
    pub skipped: u32,
    pub in_flight: u64,
    pub traffic: Vec<Traffic>,
    pub spreads: Vec<Spread>,
}
impl Report {
    pub fn rate(&self, q: f64, direction: Direction) -> f64 {
        assert!((0.0..=1.0).contains(&q));
        let mut rates: Vec<_> = self
            .traffic
            .iter()
            .filter(|t| t.online >= 10_000)
            .map(|t| {
                let bytes = match direction {
                    Direction::Sent => t.sent_bytes,
                    Direction::Received => t.received_bytes,
                };
                bytes as f64 * 1000.0 / t.online as f64
            })
            .collect();
        rates.sort_by(f64::total_cmp);
        if rates.is_empty() {
            0.0
        } else {
            rates[((q * rates.len() as f64).ceil() as usize).max(1) - 1]
        }
    }
}

enum Output<M> {
    Send(Member, M),
    Timer(Millis, u64),
    Applied(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Awake,
    Asleep,
    Gone,
}
struct State {
    status: Status,
    epoch: u64,
    since: Millis,
    awake_index: usize,
    live_index: usize,
}
struct Pending {
    candidates: Vec<bool>,
    delays: Vec<u32>,
}
enum Event<M> {
    Deliver { from: Member, to: Member, msg: M },
    Timer { me: Member, epoch: u64, token: u64 },
    Sleep(Member),
    Wake(Member),
    Change(Kind),
    Measure(usize),
}
struct Scheduled<M> {
    at: Millis,
    last: bool,
    sequence: u64,
    event: Event<M>,
}
impl<M> PartialEq for Scheduled<M> {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.last, self.sequence) == (other.at, other.last, other.sequence)
    }
}
impl<M> Eq for Scheduled<M> {}
impl<M> PartialOrd for Scheduled<M> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<M> Ord for Scheduled<M> {
    fn cmp(&self, other: &Self) -> Ordering {
        (other.at, other.last, other.sequence).cmp(&(self.at, self.last, self.sequence))
    }
}
struct Engine<'a, N: Node> {
    scenario: &'a Scenario,
    rng: Rng,
    now: Millis,
    sequence: u64,
    heap: BinaryHeap<Scheduled<N::Msg>>,
    nodes: Vec<N>,
    states: Vec<State>,
    awake: Vec<Member>,
    live: Vec<Member>,
    pending: Vec<Option<Pending>>,
    report: Report,
}
impl<N: Node> Engine<'_, N> {
    fn schedule(&mut self, at: Millis, event: Event<N::Msg>) {
        self.heap.push(Scheduled {
            at,
            last: matches!(event, Event::Measure(_)),
            sequence: self.sequence,
            event,
        });
        self.sequence += 1;
    }
    fn register(&mut self) -> Member {
        let me = self.states.len() as Member;
        self.states.push(State {
            status: Status::Awake,
            epoch: 0,
            since: self.now,
            awake_index: self.awake.len(),
            live_index: self.live.len(),
        });
        self.awake.push(me);
        self.live.push(me);
        self.report.traffic.push(Traffic::default());
        self.report.members += 1;
        me
    }
    fn start(
        &mut self,
        me: Member,
        boot: Boot,
        start: &mut impl FnMut(Member, Boot, &mut Ctx<'_, N::Msg>) -> N,
    ) {
        let mut out = Vec::new();
        let node = start(
            me,
            boot,
            &mut Ctx {
                me,
                now: self.now,
                rng: &mut self.rng,
                out: &mut out,
            },
        );
        self.nodes.push(node);
        self.outputs(me, out);
    }
    fn call(&mut self, me: Member, call: impl FnOnce(&mut N, &mut Ctx<'_, N::Msg>)) {
        let mut out = Vec::new();
        call(
            &mut self.nodes[me as usize],
            &mut Ctx {
                me,
                now: self.now,
                rng: &mut self.rng,
                out: &mut out,
            },
        );
        self.outputs(me, out);
    }
    fn counting(&self) -> bool {
        self.now >= self.scenario.measure_from
    }
    fn outputs(&mut self, me: Member, out: Vec<Output<N::Msg>>) {
        for output in out {
            match output {
                Output::Send(to, msg) => {
                    assert!(
                        to != me && (to as usize) < self.states.len(),
                        "a layer sent to a member that does not exist"
                    );
                    if self.counting() {
                        let traffic = &mut self.report.traffic[me as usize];
                        traffic.sent += 1;
                        traffic.sent_bytes += N::size(&msg) as u64;
                    }
                    let (min, max) = self.scenario.latency;
                    let width = max as u128 - min as u128 + 1;
                    let delay = min + ((self.rng.next() as u128 * width) >> 64) as u64;
                    self.schedule(
                        self.now.saturating_add(delay),
                        Event::Deliver { from: me, to, msg },
                    );
                }
                Output::Timer(delay, token) => self.schedule(
                    self.now.saturating_add(delay),
                    Event::Timer {
                        me,
                        epoch: self.states[me as usize].epoch,
                        token,
                    },
                ),
                Output::Applied(id) => self.applied(me, id),
            }
        }
    }
    fn applied(&mut self, me: Member, id: u32) {
        if let Some(Some(pending)) = self.pending.get_mut(id as usize)
            && let Some(delay) = pending.delays.get_mut(me as usize)
            && *delay == u32::MAX
        {
            let elapsed = self.now - self.report.spreads[id as usize].at;
            *delay = elapsed.min(u32::MAX as u64 - 1) as u32;
        }
    }
    fn online(&mut self, me: Member) {
        let state = &mut self.states[me as usize];
        if state.status == Status::Awake {
            self.report.traffic[me as usize].online += self
                .now
                .saturating_sub(state.since.max(self.scenario.measure_from));
        }
        state.since = self.now;
    }
    fn remove_awake(&mut self, me: Member) {
        let index = self.states[me as usize].awake_index;
        self.awake.swap_remove(index);
        if let Some(&moved) = self.awake.get(index) {
            self.states[moved as usize].awake_index = index;
        }
    }
    fn gone(&mut self, me: Member) {
        self.online(me);
        if self.states[me as usize].status == Status::Awake {
            self.remove_awake(me);
        }
        let index = self.states[me as usize].live_index;
        self.live.swap_remove(index);
        if let Some(&moved) = self.live.get(index) {
            self.states[moved as usize].live_index = index;
        }
        self.states[me as usize].status = Status::Gone;
        self.states[me as usize].epoch += 1;
    }
    fn churn(&mut self, me: Member, sleeping: bool) {
        if self.scenario.asleep == 0.0 {
            return;
        }
        let mean = self.scenario.sleep_mean as f64
            * if sleeping {
                1.0
            } else {
                (1.0 - self.scenario.asleep) / self.scenario.asleep
            };
        let delay = (-mean * (1.0 - self.rng.unit()).ln()).round().max(1.0) as Millis;
        self.schedule(
            self.now.saturating_add(delay),
            if sleeping {
                Event::Wake(me)
            } else {
                Event::Sleep(me)
            },
        );
    }
    fn sleep(&mut self, me: Member) {
        self.online(me);
        self.remove_awake(me);
        self.states[me as usize].status = Status::Asleep;
        self.states[me as usize].epoch += 1;
        self.nodes[me as usize].slept();
        self.churn(me, true);
    }
    fn pick_awake(&mut self, except: Option<Member>) -> Member {
        let exclude = except.filter(|m| self.states[*m as usize].status == Status::Awake);
        let mut index =
            self.rng
                .below(self.awake.len() as u32 - u32::from(exclude.is_some())) as usize;
        if exclude.is_some_and(|m| index >= self.states[m as usize].awake_index) {
            index += 1;
        }
        self.awake[index]
    }
    fn change(
        &mut self,
        kind: Kind,
        start: &mut impl FnMut(Member, Boot, &mut Ctx<'_, N::Msg>) -> N,
    ) {
        if self.awake.is_empty() || (kind == Kind::Remove && self.live.len() < 2) {
            self.report.skipped += 1;
            return;
        }
        let (subject, origin) = match kind {
            Kind::Join => {
                let origin = self.pick_awake(None);
                (self.register(), origin)
            }
            Kind::Leave => {
                let me = self.pick_awake(None);
                (me, me)
            }
            Kind::Remove => {
                let exclude =
                    (self.awake.len() == 1).then(|| self.states[self.awake[0] as usize].live_index);
                let mut index = self
                    .rng
                    .below(self.live.len() as u32 - u32::from(exclude.is_some()))
                    as usize;
                if exclude.is_some_and(|excluded| index >= excluded) {
                    index += 1;
                }
                let subject = self.live[index];
                (subject, self.pick_awake(Some(subject)))
            }
        };
        let change = Change {
            id: self.report.spreads.len() as u32,
            kind,
            subject,
            origin,
        };
        let candidates = self
            .states
            .iter()
            .enumerate()
            .map(|(me, state)| {
                state.status == Status::Awake && me != subject as usize && me != origin as usize
            })
            .collect();
        self.pending.push(Some(Pending {
            candidates,
            delays: vec![u32::MAX; self.states.len()],
        }));
        self.report.spreads.push(Spread {
            change,
            at: self.now,
            targets: 0,
            reached: 0,
            after_99: None,
        });
        self.applied(origin, change.id);
        if kind == Kind::Join {
            self.applied(subject, change.id);
            self.start(subject, Boot::Joining { inviter: origin }, start);
            self.churn(subject, false);
        }
        if kind == Kind::Remove {
            self.gone(subject);
        }
        self.call(origin, |node, ctx| node.originate(change, ctx));
        if kind == Kind::Leave {
            self.gone(subject);
        }
        self.schedule(
            self.now + self.scenario.window,
            Event::Measure(change.id as usize),
        );
    }
    fn measure(&mut self, id: usize) {
        let pending = self.pending[id].take().unwrap();
        let spread = &mut self.report.spreads[id];
        let mut delays = Vec::new();
        for (me, candidate) in pending.candidates.into_iter().enumerate() {
            if candidate && self.states[me].status == Status::Awake {
                spread.targets += 1;
                let delay = pending.delays[me];
                if delay != u32::MAX && delay as Millis <= self.scenario.window {
                    delays.push(delay as Millis);
                }
            }
        }
        spread.reached = delays.len() as u32;
        let rank = (99 * spread.targets as u64).div_ceil(100) as usize;
        delays.sort_unstable();
        spread.after_99 = if rank == 0 {
            Some(0)
        } else {
            delays.get(rank - 1).copied()
        };
    }
    fn event(
        &mut self,
        event: Event<N::Msg>,
        start: &mut impl FnMut(Member, Boot, &mut Ctx<'_, N::Msg>) -> N,
    ) {
        match event {
            Event::Deliver { from, to, msg } => {
                if self.states[to as usize].status == Status::Awake {
                    if self.counting() {
                        let traffic = &mut self.report.traffic[to as usize];
                        traffic.received += 1;
                        traffic.received_bytes += N::size(&msg) as u64;
                    }
                    self.call(to, |node, ctx| node.receive(from, msg, ctx));
                } else {
                    if self.counting() {
                        self.report.traffic[from as usize].undelivered += 1;
                    }
                    if self.states[from as usize].status == Status::Awake {
                        self.call(from, |node, ctx| node.undelivered(to, msg, ctx));
                    }
                }
            }
            Event::Timer { me, epoch, token } => {
                if self.states[me as usize].status == Status::Awake
                    && self.states[me as usize].epoch == epoch
                {
                    self.call(me, |node, ctx| node.timer(token, ctx));
                }
            }
            Event::Sleep(me) if self.states[me as usize].status == Status::Awake => self.sleep(me),
            Event::Wake(me) if self.states[me as usize].status == Status::Asleep => {
                self.states[me as usize].status = Status::Awake;
                self.states[me as usize].since = self.now;
                self.states[me as usize].awake_index = self.awake.len();
                self.awake.push(me);
                self.call(me, |node, ctx| node.woke(ctx));
                self.churn(me, false);
            }
            Event::Change(kind) => self.change(kind, start),
            Event::Measure(id) => self.measure(id),
            _ => {}
        }
    }
}

pub fn run<N: Node>(
    scenario: &Scenario,
    mut start: impl FnMut(Member, Boot, &mut Ctx<'_, N::Msg>) -> N,
) -> Result<(Report, Vec<N>), &'static str> {
    if scenario.members == 0
        || !(0.0..1.0).contains(&scenario.asleep)
        || (scenario.asleep > 0.0 && scenario.sleep_mean == 0)
        || scenario.latency.0 > scenario.latency.1
        || scenario.measure_from >= scenario.duration
        || scenario.window == 0
        || scenario.window >= u32::MAX as Millis
        || scenario.changes.iter().any(|(at, _)| {
            at.checked_add(scenario.window)
                .is_none_or(|end| end >= scenario.duration)
        })
    {
        return Err("the scenario is not valid");
    }
    let mut engine = Engine {
        scenario,
        rng: Rng::new(scenario.seed),
        now: 0,
        sequence: 0,
        heap: BinaryHeap::new(),
        nodes: Vec::new(),
        states: Vec::new(),
        awake: Vec::new(),
        live: Vec::new(),
        pending: Vec::new(),
        report: Report {
            members: 0,
            events: 0,
            skipped: 0,
            in_flight: 0,
            traffic: Vec::new(),
            spreads: Vec::new(),
        },
    };
    for _ in 0..scenario.members {
        engine.register();
    }
    for me in 0..scenario.members {
        engine.start(
            me,
            Boot::Founding {
                members: scenario.members,
            },
            &mut start,
        );
    }
    if scenario.asleep > 0.0 {
        for me in 0..scenario.members {
            if engine.rng.unit() < scenario.asleep {
                engine.sleep(me);
            } else {
                engine.churn(me, false);
            }
        }
    }
    let mut changes = scenario.changes.clone();
    changes.sort_by_key(|(at, _)| *at);
    for (at, kind) in changes {
        engine.schedule(at, Event::Change(kind));
    }
    while engine
        .heap
        .peek()
        .is_some_and(|event| event.at < scenario.duration)
    {
        let event = engine.heap.pop().unwrap();
        engine.now = event.at;
        engine.report.events += 1;
        engine.event(event.event, &mut start);
    }
    engine.now = scenario.duration;
    for me in 0..engine.states.len() {
        engine.online(me as Member);
    }
    engine.report.in_flight = engine
        .heap
        .iter()
        .filter(|e| matches!(e.event, Event::Deliver { .. }))
        .count() as u64;
    Ok((engine.report, engine.nodes))
}

#[cfg(test)]
#[path = "sim_tests.rs"]
mod tests;
