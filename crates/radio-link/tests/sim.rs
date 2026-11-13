use radio_link::sim::*;

struct One;
impl Node for One {
    type Msg = ();
    fn size(_: &()) -> u32 {
        100
    }
    fn receive(&mut self, _: Member, _: (), _: &mut Ctx<'_, ()>) {}
    fn originate(&mut self, _: Change, _: &mut Ctx<'_, ()>) {}
    fn timer(&mut self, _: u64, _: &mut Ctx<'_, ()>) {}
    fn undelivered(&mut self, _: Member, _: (), _: &mut Ctx<'_, ()>) {}
}

#[test]
fn counts_messages_and_bytes() {
    let mut scenario = Scenario::new(1, 50);
    scenario.asleep = 0.0;
    scenario.measure_from = 0;
    let (report, _) = run(&scenario, |me, _, ctx| {
        if me != 0 {
            ctx.send(0, ());
        }
        One
    })
    .unwrap();
    assert_eq!(report.traffic[0].received, 49);
    assert_eq!(report.traffic[0].received_bytes, 4_900);
    for traffic in &report.traffic[1..] {
        assert_eq!((traffic.sent, traffic.sent_bytes), (1, 100));
    }
}

#[path = "sim/push.rs"]
mod push;
use push::Push;
use std::time::Instant;

fn changes() -> Vec<(Millis, Kind)> {
    vec![
        (60_000, Kind::Join),
        (100_000, Kind::Remove),
        (140_000, Kind::Leave),
        (180_000, Kind::Join),
        (220_000, Kind::Remove),
        (260_000, Kind::Leave),
    ]
}
fn push_run(scenario: &Scenario) -> (Report, Vec<Vec<Member>>) {
    let (report, nodes) = run(scenario, Push::start).unwrap();
    (report, nodes.into_iter().map(|n| n.view).collect())
}
#[test]
fn deterministic_with_one_seed() {
    let mut scenario = Scenario::new(9, 500);
    scenario.changes = vec![
        (60_000, Kind::Join),
        (100_000, Kind::Join),
        (150_000, Kind::Remove),
        (200_000, Kind::Leave),
    ];
    let first = push_run(&scenario);
    assert_eq!(first, push_run(&scenario));
    scenario.seed += 1;
    assert_ne!(first.0, push_run(&scenario).0);
}
#[test]
fn every_message_is_accounted_for() {
    let mut scenario = Scenario::new(12, 300);
    scenario.asleep = 0.3;
    scenario.sleep_mean = 5_000;
    scenario.measure_from = 0;
    scenario.changes = changes();
    let report = push_run(&scenario).0;
    let sum = |f: fn(&Traffic) -> u64| report.traffic.iter().map(f).sum::<u64>();
    assert_eq!(
        sum(|t| t.sent),
        sum(|t| t.received) + sum(|t| t.undelivered) + report.in_flight
    );
    assert!(sum(|t| t.undelivered) > 0);
}

#[derive(Debug, PartialEq)]
enum Entry {
    Receive(Millis),
    Timer(Millis),
    SleepAfter(Millis),
    Wake(Millis),
    Failed(Millis),
}
struct Log {
    entries: Vec<Entry>,
    last: Millis,
    members: u32,
    direct: bool,
    pulse: bool,
}
impl Log {
    fn start(_: Member, boot: Boot, _: &mut Ctx<'_, Option<Change>>) -> Self {
        Self {
            entries: Vec::new(),
            last: 0,
            members: match boot {
                Boot::Founding { members } => members,
                _ => 0,
            },
            direct: true,
            pulse: false,
        }
    }
    fn pulse(&mut self, ctx: &mut Ctx<'_, Option<Change>>) {
        let mut to = ctx.random(self.members - 1);
        if to >= ctx.me() {
            to += 1;
        }
        ctx.send(to, None);
        ctx.after(100, 0);
    }
}
impl Node for Log {
    type Msg = Option<Change>;
    fn size(_: &Self::Msg) -> u32 {
        100
    }
    fn receive(&mut self, _: Member, msg: Self::Msg, ctx: &mut Ctx<'_, Self::Msg>) {
        self.last = ctx.now();
        self.entries.push(Entry::Receive(ctx.now()));
        if let Some(change) = msg {
            ctx.applied(change.id);
            ctx.applied(change.id);
        }
    }
    fn originate(&mut self, change: Change, ctx: &mut Ctx<'_, Self::Msg>) {
        if self.direct {
            for to in 0..self.members {
                if to != ctx.me() && to != change.subject {
                    ctx.send(to, Some(change));
                }
            }
        }
    }
    fn timer(&mut self, _: u64, ctx: &mut Ctx<'_, Self::Msg>) {
        self.last = ctx.now();
        self.entries.push(Entry::Timer(ctx.now()));
        if self.pulse {
            self.pulse(ctx);
        }
    }
    fn undelivered(&mut self, _: Member, _: Self::Msg, ctx: &mut Ctx<'_, Self::Msg>) {
        self.last = ctx.now();
        self.entries.push(Entry::Failed(ctx.now()));
    }
    fn slept(&mut self) {
        self.entries.push(Entry::SleepAfter(self.last));
    }
    fn woke(&mut self, ctx: &mut Ctx<'_, Self::Msg>) {
        self.last = ctx.now();
        self.entries.push(Entry::Wake(ctx.now()));
        if self.pulse {
            self.pulse(ctx);
        }
    }
}
#[test]
fn asleep_members_hear_nothing() {
    let mut scenario = Scenario::new(13, 200);
    scenario.asleep = 0.5;
    scenario.sleep_mean = 2_000;
    scenario.duration = 20_000;
    scenario.measure_from = 0;
    let (report, nodes) = run(&scenario, |me, boot, ctx| {
        let mut node = Log::start(me, boot, ctx);
        node.pulse = true;
        node.pulse(ctx);
        node
    })
    .unwrap();
    let mut sleeps = 0;
    for (node, traffic) in nodes.iter().zip(&report.traffic) {
        let mut asleep = false;
        let mut logged_sleep = false;
        for entry in &node.entries {
            match entry {
                Entry::SleepAfter(_) => {
                    assert!(!asleep);
                    asleep = true;
                    logged_sleep = true;
                    sleeps += 1;
                }
                Entry::Wake(_) => {
                    assert!(asleep);
                    asleep = false;
                }
                _ => assert!(!asleep, "{entry:?}"),
            }
        }
        if traffic.online < scenario.duration {
            assert!(logged_sleep);
        }
    }
    assert!(sleeps > 200);
}
#[test]
fn sleep_churn_holds_its_share() {
    let mut scenario = Scenario::new(14, 2_000);
    scenario.sleep_mean = 60_000;
    scenario.duration = 3_600_000;
    scenario.measure_from = 0;
    let (report, _) = run(&scenario, |_, _, _| One).unwrap();
    let online: u64 = report.traffic.iter().map(|t| t.online).sum();
    let share = 1.0 - online as f64 / (2_000.0 * 3_600_000.0);
    assert!((0.08..0.12).contains(&share), "{share}");
}
#[test]
fn joins_take_new_indices_and_receive_views() {
    let mut scenario = Scenario::new(15, 300);
    scenario.asleep = 0.0;
    scenario.changes = vec![
        (60_000, Kind::Join),
        (100_000, Kind::Join),
        (150_000, Kind::Join),
    ];
    let (report, views) = push_run(&scenario);
    assert_eq!(report.members, 303);
    assert_eq!(report.spreads.len(), 3);
    for (i, spread) in report.spreads.iter().enumerate() {
        assert_eq!(spread.change.kind, Kind::Join);
        assert_eq!(spread.change.subject, 300 + i as u32);
        let view = &views[spread.change.subject as usize];
        assert!(view.contains(&spread.change.origin));
        assert!(view.len() > 1);
    }
}
#[test]
fn gone_members_never_run_again() {
    for kind in [Kind::Remove, Kind::Leave] {
        let mut scenario = Scenario::new(16, 30);
        scenario.asleep = 0.0;
        scenario.measure_from = 0;
        scenario.duration = 2_000;
        scenario.window = 500;
        scenario.latency = (10, 10);
        scenario.changes = vec![(500, kind)];
        let (report, nodes) = run(&scenario, |me, boot, ctx| {
            let mut node = Log::start(me, boot, ctx);
            node.pulse = true;
            node.pulse(ctx);
            node
        })
        .unwrap();
        let subject = report.spreads[0].change.subject as usize;
        assert!(nodes[subject].entries.iter().all(|entry| match entry {
            Entry::Receive(t) | Entry::Timer(t) | Entry::Wake(t) | Entry::Failed(t) => *t < 500,
            Entry::SleepAfter(_) => false,
        }));
        assert!(report.traffic.iter().map(|t| t.undelivered).sum::<u64>() > 0);
        assert_eq!(report.traffic[subject].online, 500);
    }
}
#[test]
fn spread_counts_targets_and_exact_delays() {
    for (kind, targets) in [(Kind::Join, 299), (Kind::Remove, 298)] {
        let mut scenario = Scenario::new(17, 300);
        scenario.asleep = 0.0;
        scenario.latency = (10, 10);
        scenario.changes = vec![(60_000, kind)];
        let (report, _) = run(&scenario, Log::start).unwrap();
        let spread = &report.spreads[0];
        assert_eq!(
            (spread.targets, spread.reached, spread.after_99),
            (targets, targets, Some(10))
        );
    }
    let mut scenario = Scenario::new(17, 300);
    scenario.asleep = 0.0;
    scenario.changes = vec![(60_000, Kind::Remove)];
    let (report, _) = run(&scenario, |me, boot, ctx| {
        let mut node = Log::start(me, boot, ctx);
        node.direct = false;
        node
    })
    .unwrap();
    assert_eq!(
        (report.spreads[0].reached, report.spreads[0].after_99),
        (0, None)
    );
}
#[test]
fn push_meets_bars_at_2000() {
    let mut scenario = Scenario::new(18, 2_000);
    scenario.changes = changes();
    let report = push_run(&scenario).0;
    assert_eq!(report.spreads.len(), 6);
    for spread in &report.spreads {
        assert!(spread.reached * 100 >= spread.targets * 99, "{spread:?}");
        assert!(
            spread.after_99.is_some_and(|delay| delay <= 30_000),
            "{spread:?}"
        );
    }
    assert!(report.rate(0.99, Direction::Sent) <= 2_048.0);
    assert!(report.rate(0.99, Direction::Received) <= 2_048.0);
}
#[test]
#[ignore]
fn push_at_171000() {
    let mut scenario = Scenario::new(19, 171_000);
    let kinds = [
        Kind::Join,
        Kind::Remove,
        Kind::Leave,
        Kind::Join,
        Kind::Remove,
        Kind::Leave,
        Kind::Join,
        Kind::Remove,
        Kind::Leave,
        Kind::Join,
    ];
    scenario.changes = kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| (60_000 + i as u64 * 200_000 / 9, kind))
        .collect();
    let start = Instant::now();
    let report = push_run(&scenario).0;
    eprintln!(
        "wall={:?} events={} skipped={}",
        start.elapsed(),
        report.events,
        report.skipped
    );
    for q in [0.5, 0.99, 1.0] {
        eprintln!(
            "q={q} sent={} received={}",
            report.rate(q, Direction::Sent),
            report.rate(q, Direction::Received)
        );
    }
    for spread in &report.spreads {
        eprintln!(
            "{:?} targets={} reached={} after_99={:?}",
            spread.change.kind, spread.targets, spread.reached, spread.after_99
        );
    }
    assert_eq!(report.spreads.len(), 10);
}

#[test]
fn timers_from_before_sleep_stay_cancelled_after_waking() {
    struct Epoch {
        generation: u64,
        woke: bool,
    }
    impl Node for Epoch {
        type Msg = ();
        fn size(_: &()) -> u32 {
            0
        }
        fn receive(&mut self, _: Member, _: (), _: &mut Ctx<'_, ()>) {}
        fn originate(&mut self, _: Change, _: &mut Ctx<'_, ()>) {}
        fn undelivered(&mut self, _: Member, _: (), _: &mut Ctx<'_, ()>) {}
        fn timer(&mut self, token: u64, ctx: &mut Ctx<'_, ()>) {
            assert_eq!(token, self.generation);
            ctx.after(200, self.generation);
        }
        fn slept(&mut self) {
            self.generation += 1;
        }
        fn woke(&mut self, ctx: &mut Ctx<'_, ()>) {
            self.woke = true;
            ctx.after(200, self.generation);
        }
    }
    let mut scenario = Scenario::new(20, 20);
    scenario.asleep = 0.5;
    scenario.sleep_mean = 100;
    scenario.measure_from = 0;
    scenario.duration = 10_000;
    let (_, nodes) = run(&scenario, |_, _, ctx| {
        ctx.after(200, 0);
        Epoch {
            generation: 0,
            woke: false,
        }
    })
    .unwrap();
    assert!(nodes.iter().all(|n| n.woke && n.generation > 1));
}

#[test]
fn duration_excludes_deliveries_and_timers_at_its_boundary() {
    let mut scenario = Scenario::new(21, 2);
    scenario.asleep = 0.0;
    scenario.measure_from = 10;
    scenario.duration = 20;
    scenario.latency = (20, 20);
    let (report, nodes) = run(&scenario, |me, boot, ctx| {
        ctx.send(1 - me, None);
        ctx.after(20, 0);
        Log::start(me, boot, ctx)
    })
    .unwrap();
    assert_eq!(report.events, 0);
    assert_eq!(report.in_flight, 2);
    assert!(nodes.iter().all(|n| n.entries.is_empty()));
    assert!(
        report
            .traffic
            .iter()
            .all(|t| t.online == 10 && t.sent == 0 && t.received == 0)
    );
}

#[test]
fn changes_with_no_eligible_member_take_no_id() {
    let mut scenario = Scenario::new(22, 1);
    scenario.asleep = 0.0;
    scenario.changes = vec![(10, Kind::Remove), (20, Kind::Leave), (30, Kind::Join)];
    let report = run(&scenario, |_, _, _| One).unwrap().0;
    assert_eq!(report.skipped, 2);
    assert_eq!(report.spreads.len(), 1);
    assert_eq!(report.spreads[0].change.id, 0);
    assert_eq!(
        (
            report.spreads[0].targets,
            report.spreads[0].reached,
            report.spreads[0].after_99
        ),
        (0, 0, Some(0))
    );
}

#[test]
fn sends_and_timers_at_one_time_keep_callback_order() {
    use std::{cell::RefCell, rc::Rc};
    type Calls = Rc<RefCell<Vec<(Member, char)>>>;
    struct Order(Calls);
    impl Node for Order {
        type Msg = char;
        fn size(_: &char) -> u32 {
            0
        }
        fn receive(&mut self, _: Member, msg: char, ctx: &mut Ctx<'_, char>) {
            self.0.borrow_mut().push((ctx.me(), msg));
        }
        fn timer(&mut self, _: u64, ctx: &mut Ctx<'_, char>) {
            self.0.borrow_mut().push((ctx.me(), 't'));
        }
        fn originate(&mut self, _: Change, _: &mut Ctx<'_, char>) {}
        fn undelivered(&mut self, _: Member, _: char, _: &mut Ctx<'_, char>) {}
    }
    let mut scenario = Scenario::new(23, 2);
    scenario.asleep = 0.0;
    scenario.latency = (0, 0);
    scenario.measure_from = 0;
    let calls = Calls::default();
    run(&scenario, |me, _, ctx| {
        if me == 0 {
            ctx.send(1, 'a');
            ctx.after(0, 0);
            ctx.send(1, 'b');
        }
        Order(calls.clone())
    })
    .unwrap();
    assert_eq!(*calls.borrow(), [(1, 'a'), (0, 't'), (1, 'b')]);
}

#[test]
#[should_panic(expected = "a layer sent to a member that does not exist")]
fn self_sends_are_refused() {
    let mut scenario = Scenario::new(24, 2);
    scenario.asleep = 0.0;
    let _ = run(&scenario, |me, _, ctx| {
        ctx.send(me, ());
        One
    });
}
#[test]
#[should_panic(expected = "a layer sent to a member that does not exist")]
fn sends_to_future_members_are_refused() {
    let mut scenario = Scenario::new(24, 2);
    scenario.asleep = 0.0;
    let _ = run(&scenario, |_, _, ctx| {
        ctx.send(2, ());
        One
    });
}

#[test]
fn change_window_can_end_just_before_duration() {
    let mut scenario = Scenario::new(25, 3);
    scenario.asleep = 0.0;
    scenario.latency = (10, 10);
    scenario.changes = vec![(scenario.duration - scenario.window - 1, Kind::Join)];
    let report = run(&scenario, Log::start).unwrap().0;
    assert_eq!(report.spreads.len(), 1);
    let spread = &report.spreads[0];
    assert_eq!(
        (spread.targets, spread.reached, spread.after_99),
        (2, 2, Some(10))
    );
}
