use crate::{
    EndpointId,
    fleet_db::{FleetDb, State},
    link::Link,
    topic::{self, ReceiveBar, Received, TopicMembers, TopicMessage},
};
use futures_lite::StreamExt;
use iroh::{EndpointAddr, TransportAddr, address_lookup::memory::MemoryLookup};
use iroh_gossip::{Gossip, api::GossipSender};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::task::JoinHandle;

struct Subscription {
    fleet: String,
    sender: GossipSender,
    receiver: JoinHandle<()>,
    neighbors: Arc<Mutex<HashSet<EndpointId>>>,
}

pub(crate) struct FleetTopic {
    pub(crate) gossip: Gossip,
    pub(crate) lookup: MemoryLookup,
    db: Mutex<Option<Arc<Mutex<FleetDb>>>>,
    members: Arc<Mutex<TopicMembers>>,
    bar: Arc<Mutex<ReceiveBar>>,
    subscription: Mutex<Option<Subscription>>,
    subscribing: tokio::sync::Mutex<()>,
    closed: AtomicBool,
    fetches: Mutex<HashMap<EndpointId, Fetching>>,
    fetch_generation: AtomicU64,
    fetches_started: AtomicU64,
    fetches_finished: AtomicU64,
}

impl FleetTopic {
    pub(crate) fn new(gossip: Gossip, lookup: MemoryLookup) -> Self {
        Self {
            gossip,
            lookup,
            db: Mutex::new(None),
            members: Arc::new(Mutex::new(TopicMembers::default())),
            bar: Arc::new(Mutex::new(ReceiveBar::new())),
            subscription: Mutex::new(None),
            subscribing: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
            fetches: Mutex::new(HashMap::new()),
            fetch_generation: AtomicU64::new(0),
            fetches_started: AtomicU64::new(0),
            fetches_finished: AtomicU64::new(0),
        }
    }

    pub(crate) fn configure(&self, db: Arc<Mutex<FleetDb>>) {
        *self.db.lock().unwrap() = Some(db);
    }

    pub(crate) fn db(&self) -> Option<Arc<Mutex<FleetDb>>> {
        self.db.lock().ok()?.clone()
    }

    pub(crate) fn want_fetch(
        self: &Arc<Self>,
        link: &Link,
        db: Arc<Mutex<FleetDb>>,
        id: EndpointId,
    ) {
        let attempt = {
            let Ok(db) = db.lock() else { return };
            if db.fetch_wanted(&id).ok().flatten().is_none() {
                return;
            }
            let mut fetches = self.fetches.lock().unwrap();
            if self.closed.load(Ordering::Acquire)
                || !claim_fetch(&mut fetches, id, crate::node::now())
            {
                return;
            }
            FetchAttempt {
                id,
                generation: self.fetch_generation.load(Ordering::Relaxed),
            }
        };
        self.fetches_started.fetch_add(1, Ordering::Relaxed);
        let (topic, link) = (self.clone(), link.clone());
        tokio::spawn(async move {
            let held = crate::join::fetch(&link, &db, &id).await;
            let mut fetches = topic.fetches.lock().unwrap();
            attempt.finish(
                &mut fetches,
                topic.fetch_generation.load(Ordering::Relaxed),
                (!held).then(crate::node::now),
            );
            topic.fetches_finished.fetch_add(1, Ordering::Relaxed);
        });
    }

    pub(crate) fn fetch_counts(&self) -> (u64, u64) {
        (
            self.fetches_started.load(Ordering::Relaxed),
            self.fetches_finished.load(Ordering::Relaxed),
        )
    }

    pub(crate) fn refresh(
        &self,
        db: &Mutex<FleetDb>,
        own: EndpointId,
    ) -> Result<Vec<EndpointId>, &'static str> {
        let db = db.lock().map_err(|_| "the fleet store is busy")?;
        let computers = db.computers()?;
        let mut members = self.members.lock().unwrap();
        let mut accepted = HashSet::new();
        let mut listed = HashSet::new();
        for computer in computers
            .into_iter()
            .filter(|c| matches!(c.state, State::Accepted | State::Pending | State::Blocked))
        {
            let key: EndpointId = computer.key.parse().map_err(|_| "not a computer key")?;
            if key != own {
                listed.insert(key);
                if computer.state == State::Accepted {
                    self.lookup.set_endpoint_info(EndpointAddr::from_parts(
                        key,
                        computer.addrs.into_iter().map(TransportAddr::Ip),
                    ));
                    accepted.insert(key);
                }
            }
        }
        for key in members.accepted.difference(&accepted) {
            self.lookup.remove_endpoint_info(*key);
        }
        *members = TopicMembers { accepted, listed };
        Ok(members.accepted.iter().copied().collect())
    }

    pub(crate) async fn sync(&self, link: &Link) -> Result<(), &'static str> {
        let _subscribing = self.subscribing.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err("the link stopped");
        }
        let Some(db) = self.db() else { return Ok(()) };
        let fleet = db.lock().map_err(|_| "the fleet store is busy")?.fleet()?;
        let Some(fleet) = fleet else {
            self.clear();
            return Ok(());
        };
        let bootstrap = self.refresh(&db, link.id())?;
        let sender = self
            .subscription
            .lock()
            .unwrap()
            .as_ref()
            .filter(|s| s.fleet == fleet.body.id)
            .map(|s| s.sender.clone());
        if let Some(sender) = sender {
            sender
                .join_peers(bootstrap)
                .await
                .map_err(|_| "the fleet topic stopped")?;
            return Ok(());
        }
        self.drop_subscription();
        let topic = self
            .gossip
            .subscribe(topic::topic_id(&fleet.body.id), bootstrap)
            .await
            .map_err(|_| "the fleet topic could not be joined")?;
        let (sender, mut receiver) = topic.split();
        let mut subscription = self.subscription.lock().unwrap();
        if self.closed.load(Ordering::Acquire)
            || db
                .lock()
                .map_err(|_| "the fleet store is busy")?
                .fleet()?
                .as_ref()
                .map(|f| &f.body.id)
                != Some(&fleet.body.id)
        {
            return Ok(());
        }
        let (link, bar, members) = (link.clone(), self.bar.clone(), self.members.clone());
        let neighbors = Arc::new(Mutex::new(HashSet::new()));
        let seen_neighbors = neighbors.clone();
        let fleet_id = fleet.body.id.clone();
        let task = tokio::spawn(async move {
            while let Some(event) = receiver.next().await {
                *seen_neighbors.lock().unwrap() = receiver.neighbors().collect();
                let message = match event {
                    Ok(iroh_gossip::api::Event::Received(message)) => message,
                    Ok(iroh_gossip::api::Event::Lagged) => {
                        // Lost removals and leaves are covered by their direct sends and announcements by the next one; an introduction sent only on the topic is lost until catch-up.
                        continue;
                    }
                    _ => continue,
                };
                if topic::precheck(
                    &message.content,
                    &fleet_id,
                    &members,
                    &bar,
                    crate::node::now(),
                )
                .is_err()
                {
                    continue;
                }
                let applied = {
                    let (Ok(mut db), Ok(mut bar)) = (db.lock(), bar.lock()) else {
                        continue;
                    };
                    topic::receive(
                        &mut db,
                        &mut bar,
                        &link.id(),
                        link.name(),
                        &message.content,
                        crate::node::now(),
                    )
                };
                if let Ok(Received::Record(applied)) = applied {
                    crate::join::effects(&link, &db, &applied);
                }
            }
        });
        *subscription = Some(Subscription {
            fleet: fleet.body.id,
            sender,
            receiver: task,
            neighbors,
        });
        Ok(())
    }

    pub(crate) fn accepted(&self) -> Vec<EndpointId> {
        self.members
            .lock()
            .unwrap()
            .accepted
            .iter()
            .copied()
            .collect()
    }

    pub(crate) fn neighbors(&self) -> Vec<EndpointId> {
        self.subscription
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.neighbors.lock().unwrap().iter().copied().collect())
            .unwrap_or_default()
    }

    pub(crate) async fn broadcast(&self, message: &TopicMessage) -> Result<(), &'static str> {
        let bytes = message.encode()?;
        let sender = self
            .subscription
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.sender.clone())
            .ok_or("not in a fleet topic")?;
        sender
            .broadcast(bytes.into())
            .await
            .map_err(|_| "the fleet topic stopped")
    }

    fn drop_subscription(&self) {
        if let Some(subscription) = self.subscription.lock().unwrap().take() {
            subscription.receiver.abort();
        }
    }

    pub(crate) fn clear(&self) {
        {
            let mut fetches = self.fetches.lock().unwrap();
            self.fetch_generation.fetch_add(1, Ordering::Relaxed);
            fetches.clear();
        }
        self.drop_subscription();
        let mut members = self.members.lock().unwrap();
        members.listed.clear();
        for key in members.accepted.drain() {
            self.lookup.remove_endpoint_info(key);
        }
    }

    pub(crate) async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.clear();
        let _ = self.gossip.shutdown().await;
    }
}

/// The minimum pause after a computer-record fetch fails.
const FETCH_RETRY_AFTER: f64 = 60.0;

enum Fetching {
    InFlight,
    FailedAt(f64),
}

struct FetchAttempt {
    id: EndpointId,
    generation: u64,
}

impl FetchAttempt {
    fn finish(
        self,
        fetches: &mut HashMap<EndpointId, Fetching>,
        generation: u64,
        failed_at: Option<f64>,
    ) {
        if self.generation == generation
            && matches!(fetches.get(&self.id), Some(Fetching::InFlight))
        {
            if let Some(now) = failed_at {
                fetches.insert(self.id, Fetching::FailedAt(now));
            } else {
                fetches.remove(&self.id);
            }
        }
    }
}

fn claim_fetch(fetches: &mut HashMap<EndpointId, Fetching>, id: EndpointId, now: f64) -> bool {
    match fetches.get(&id) {
        Some(Fetching::InFlight) => return false,
        Some(Fetching::FailedAt(last)) if now - last < FETCH_RETRY_AFTER => return false,
        _ => {}
    }
    fetches.insert(id, Fetching::InFlight);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_fetch_is_not_started_while_one_is_in_flight() {
        let mut fetches = HashMap::new();
        let id = crate::SecretKey::generate().public();
        assert!(claim_fetch(&mut fetches, id, 1000.0));
        assert!(!claim_fetch(&mut fetches, id, 1000.0));
    }

    #[test]
    fn a_failed_fetch_waits_for_the_back_off() {
        let mut fetches = HashMap::new();
        let id = crate::SecretKey::generate().public();
        fetches.insert(id, Fetching::FailedAt(1000.0));
        assert!(!claim_fetch(&mut fetches, id, 1059.0));
        assert!(claim_fetch(&mut fetches, id, 1000.0 + FETCH_RETRY_AFTER));
    }
    fn old_completion_keeps_a_new_fetch(failed_at: Option<f64>) {
        let mut fetches = HashMap::new();
        let id = crate::SecretKey::generate().public();
        assert!(claim_fetch(&mut fetches, id, 1000.0));
        let old = FetchAttempt { id, generation: 0 };
        fetches.clear();
        assert!(claim_fetch(&mut fetches, id, 1001.0));
        old.finish(&mut fetches, 1, failed_at);
        assert!(matches!(fetches.get(&id), Some(Fetching::InFlight)));
        assert!(!claim_fetch(&mut fetches, id, 1002.0));
    }

    #[test]
    fn an_old_success_does_not_clear_a_new_fetch() {
        old_completion_keeps_a_new_fetch(None);
    }

    #[test]
    fn an_old_failure_does_not_back_off_a_new_fetch() {
        old_completion_keeps_a_new_fetch(Some(1002.0));
    }
}
