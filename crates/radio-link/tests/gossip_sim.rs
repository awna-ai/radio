#[path = "sim/gossip.rs"]
mod gossip;

use gossip::{Gossip, Load};
use radio_link::{
    computer_record::{self, ComputerBody},
    membership::{RecordBody, sign},
    sim::*,
    topic::{TopicMessage, announce},
};
use std::time::Instant;

/// An upper bound on a signed introduction: a 32-byte name and two IPv6 addresses, one more IPv6
/// address than the wire accepts, so the bound errs large.
fn introduction_bytes() -> u32 {
    let inviter = iroh::SecretKey::from_bytes(&[1; 32]);
    let newcomer = iroh::SecretKey::from_bytes(&[2; 32]);
    let addrs = vec![
        "[fd12:3456:789a:bcde:f012:3456:789a:bcde]:65535"
            .parse()
            .unwrap();
        2
    ];
    let record = sign(
        RecordBody::Introduction {
            fleet: "f".repeat(64),
            inviter: inviter.public().to_string(),
            counter: u64::MAX,
            newcomer: newcomer.public().to_string(),
            name: "n".repeat(32),
            addrs,
            consent: "c".repeat(128),
        },
        &inviter,
    );
    serde_json::to_vec(&record).unwrap().len() as u32
}
fn removal_bytes() -> u32 {
    let signer = iroh::SecretKey::from_bytes(&[1; 32]);
    let record = sign(
        RecordBody::Removal {
            fleet: "f".repeat(64),
            signer: signer.public().to_string(),
            counter: u64::MAX,
            subject: iroh::SecretKey::from_bytes(&[2; 32]).public().to_string(),
        },
        &signer,
    );
    serde_json::to_vec(&record).unwrap().len() as u32
}
/// The largest signed computer-record announcement, as the topic carries it.
fn announcement_bytes() -> u32 {
    let node = iroh::SecretKey::from_bytes(&[1; 32]);
    let record = computer_record::sign(
        ComputerBody {
            fleet: "f".repeat(64),
            key: node.public().to_string(),
            version: i64::MAX as u64,
            name: "n".repeat(32),
            roles: vec![],
            addrs: vec![],
            apps: Default::default(),
        },
        &node,
    );
    TopicMessage::Announcement(announce(&record, &node))
        .encode()
        .unwrap()
        .len() as u32
}
fn changes(count: usize, from: Millis, to: Millis) -> Vec<(Millis, Kind)> {
    let kinds = [Kind::Join, Kind::Remove, Kind::Leave];
    (0..count)
        .map(|i| {
            (
                from + i as u64 * (to - from) / (count as u64 - 1).max(1),
                kinds[i % 3],
            )
        })
        .collect()
}
fn gossip_run(scenario: &Scenario, load: Load) -> (Report, Vec<Gossip>) {
    let payload = introduction_bytes();
    run(scenario, |me, boot, ctx| {
        Gossip::start(me, boot, payload, load, ctx)
    })
    .unwrap()
}
fn meets_bars(report: &Report) {
    for spread in &report.spreads {
        assert!(spread.reached * 100 >= spread.targets * 99, "{spread:?}");
        assert!(
            spread.after_99.is_some_and(|delay| delay <= 30_000),
            "{spread:?}"
        );
    }
    assert!(report.rate(1.0, Direction::Sent) <= 2_048.0);
    assert!(report.rate(1.0, Direction::Received) <= 2_048.0);
}

#[test]
fn signed_records_fit_the_default_cap() {
    let (introduction, removal) = (introduction_bytes(), removal_bytes());
    eprintln!("introduction={introduction} removal={removal}");
    assert!(removal < introduction);
    assert!(introduction as usize + 128 < 4096, "{introduction}");
}

#[test]
fn a_join_reaches_every_awake_member_of_a_small_fleet() {
    let mut scenario = Scenario::new(31, 40);
    scenario.asleep = 0.0;
    scenario.changes = vec![(60_000, Kind::Join)];
    let (report, _) = gossip_run(&scenario, Load::default());
    let spread = &report.spreads[0];
    assert_eq!(spread.targets, 39, "{spread:?}");
    assert_eq!(spread.reached, spread.targets, "{spread:?}");
}

#[test]
fn deterministic_with_one_seed() {
    let mut scenario = Scenario::new(32, 300);
    scenario.changes = changes(4, 60_000, 200_000);
    let first = gossip_run(&scenario, Load::default()).0;
    assert_eq!(first, gossip_run(&scenario, Load::default()).0);
}

#[test]
fn gossip_meets_bars_at_2000() {
    let mut scenario = Scenario::new(33, 2_000);
    scenario.changes = changes(6, 60_000, 260_000);
    let (report, nodes) = gossip_run(&scenario, Load::default());
    assert_eq!(report.spreads.len(), 6);
    meets_bars(&report);
    assert!(nodes.iter().all(|node| node.most_neighbors() <= 50));
}

#[test]
fn an_announcement_is_smaller_than_an_introduction() {
    let announcement = announcement_bytes();
    eprintln!("announcement={announcement}");
    assert!(announcement < removal_bytes(), "{announcement}");
}

/// Computer-record announcements at 20,000 members, one a day per member, recorded under SC2b.
/// `cargo test --release -p radio-link --test gossip_sim -- --ignored --nocapture announcements`.
#[test]
#[ignore]
fn announcements_at_20000() {
    let mut scenario = Scenario::new(35, 20_000);
    scenario.changes = changes(10, 60_000, 260_000);
    let load = Load {
        record_bytes: announcement_bytes(),
        updates_per_day: 1.0,
    };
    let start = Instant::now();
    let (report, nodes) = gossip_run(&scenario, load);
    eprintln!(
        "{load:?} wall={:?} events={} skipped={} most_neighbors={}",
        start.elapsed(),
        report.events,
        report.skipped,
        nodes.iter().map(Gossip::most_neighbors).max().unwrap_or(0)
    );
    for q in [0.5, 0.99, 1.0] {
        eprintln!(
            "q={q} sent={:.0} received={:.0}",
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
    meets_bars(&report);
}

/// The SC-A7 run at 171,000 members, recorded under SC2a. `cargo test --release -p radio-link
/// --test gossip_sim -- --ignored --nocapture`.
#[test]
#[ignore]
fn gossip_at_171000() {
    let mut scenario = Scenario::new(34, 171_000);
    scenario.changes = changes(10, 60_000, 260_000);
    for load in [
        Load::default(),
        Load {
            record_bytes: 1_024,
            updates_per_day: 1.0,
        },
        Load {
            record_bytes: 48 * 1024,
            updates_per_day: 1.0,
        },
    ] {
        let start = Instant::now();
        let (report, nodes) = gossip_run(&scenario, load);
        eprintln!(
            "{load:?} wall={:?} events={} skipped={} most_neighbors={}",
            start.elapsed(),
            report.events,
            report.skipped,
            nodes.iter().map(Gossip::most_neighbors).max().unwrap_or(0)
        );
        for q in [0.5, 0.99, 1.0] {
            eprintln!(
                "q={q} sent={:.0} received={:.0}",
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
}
