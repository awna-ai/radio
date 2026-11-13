use super::*;

struct Quiet;
impl Node for Quiet {
    type Msg = Self;
    fn size(_: &Self) -> u32 {
        0
    }
    fn receive(&mut self, _: Member, _: Self, _: &mut Ctx<'_, Self>) {}
    fn originate(&mut self, _: Change, _: &mut Ctx<'_, Self>) {}
    fn timer(&mut self, _: u64, _: &mut Ctx<'_, Self>) {}
    fn undelivered(&mut self, _: Member, _: Self, _: &mut Ctx<'_, Self>) {}
}
#[test]
fn messages_need_not_be_clone() {
    let mut scenario = Scenario::new(1, 2);
    scenario.asleep = 0.0;
    scenario.measure_from = 0;
    let (report, _) = run(&scenario, |me, _, ctx| {
        ctx.send(1 - me, Quiet);
        Quiet
    })
    .unwrap();
    assert!(report.traffic.iter().all(|traffic| traffic.received == 1));
}
#[test]
fn random_is_repeatable_and_bounded() {
    let mut first = Rng::new(1);
    let mut same = Rng::new(1);
    let mut other = Rng::new(2);
    let mut differs = false;
    for _ in 0..100 {
        let value = first.next();
        assert_eq!(value, same.next());
        differs |= value != other.next();
    }
    assert!(differs);
    let mut hits = [false; 7];
    for _ in 0..10_000 {
        let value = first.below(7);
        assert!(value < 7);
        hits[value as usize] = true;
        assert!((0.0..1.0).contains(&first.unit()));
    }
    assert!(hits.into_iter().all(|hit| hit));
    assert_eq!(Rng::new(0).next(), 0xe220a8397b1dcdaf);
}
#[test]
#[should_panic(expected = "random needs a bound above zero")]
fn random_requires_a_bound() {
    Rng::new(1).below(0);
}
#[test]
fn defaults_are_exact_and_accepted() {
    let scenario = Scenario::new(1, 10);
    assert_eq!(
        scenario,
        Scenario {
            seed: 1,
            members: 10,
            asleep: 0.10,
            sleep_mean: 600_000,
            latency: (20, 150),
            duration: 300_000,
            measure_from: 60_000,
            window: 30_000,
            changes: Vec::new(),
        }
    );
    assert!(run(&scenario, |_, _, _| Quiet).is_ok());
}
#[test]
fn invalid_scenarios_never_start_a_node() {
    let base = Scenario::new(1, 10);
    let mut invalid = vec![Scenario {
        members: 0,
        ..base.clone()
    }];
    for asleep in [-0.1, 1.0, f64::NAN, f64::INFINITY] {
        invalid.push(Scenario {
            asleep,
            ..base.clone()
        });
    }
    invalid.extend([
        Scenario {
            sleep_mean: 0,
            ..base.clone()
        },
        Scenario {
            latency: (2, 1),
            ..base.clone()
        },
        Scenario {
            measure_from: base.duration,
            ..base.clone()
        },
        Scenario {
            measure_from: base.duration + 1,
            ..base.clone()
        },
        Scenario {
            window: 0,
            ..base.clone()
        },
        Scenario {
            window: u32::MAX as Millis,
            ..base.clone()
        },
        Scenario {
            changes: vec![(base.duration - base.window, Kind::Join)],
            ..base.clone()
        },
        Scenario {
            changes: vec![(u64::MAX, Kind::Join)],
            ..base.clone()
        },
    ]);
    for scenario in invalid {
        assert_eq!(
            run::<Quiet>(&scenario, |_, _, _| panic!(
                "invalid scenario started a node"
            ))
            .err(),
            Some("the scenario is not valid")
        );
    }
    assert!(
        run(
            &Scenario {
                asleep: 0.0,
                sleep_mean: 0,
                ..base
            },
            |_, _, _| Quiet
        )
        .is_ok()
    );
}
#[test]
fn changes_are_sorted_stably() {
    let mut scenario = Scenario::new(1, 10);
    scenario.asleep = 0.0;
    scenario.changes = vec![
        (90_000, Kind::Remove),
        (60_000, Kind::Join),
        (60_000, Kind::Leave),
    ];
    let report = run(&scenario, |_, _, _| Quiet).unwrap().0;
    let order: Vec<_> = report
        .spreads
        .iter()
        .map(|s| (s.at, s.change.kind, s.change.id))
        .collect();
    assert_eq!(
        order,
        [
            (60_000, Kind::Join, 0),
            (60_000, Kind::Leave, 1),
            (90_000, Kind::Remove, 2)
        ]
    );
}
#[test]
fn rates_use_online_time_and_nearest_rank() {
    let report = Report {
        members: 4,
        events: 0,
        skipped: 0,
        in_flight: 0,
        spreads: Vec::new(),
        traffic: vec![
            Traffic {
                sent_bytes: 100,
                received_bytes: 300,
                online: 10_000,
                ..Traffic::default()
            },
            Traffic {
                sent_bytes: 400,
                received_bytes: 200,
                online: 20_000,
                ..Traffic::default()
            },
            Traffic {
                sent_bytes: 300,
                received_bytes: 600,
                online: 10_000,
                ..Traffic::default()
            },
            Traffic {
                sent_bytes: 1_000_000,
                online: 9_999,
                ..Traffic::default()
            },
        ],
    };
    assert_eq!(report.rate(0.0, Direction::Sent), 10.0);
    assert_eq!(report.rate(0.5, Direction::Sent), 20.0);
    assert_eq!(report.rate(1.0, Direction::Sent), 30.0);
    assert_eq!(report.rate(0.5, Direction::Received), 30.0);
    assert_eq!(
        Report {
            traffic: Vec::new(),
            ..report
        }
        .rate(1.0, Direction::Sent),
        0.0
    );
}
