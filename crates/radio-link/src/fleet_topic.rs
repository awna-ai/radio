use crate::{
    EndpointId,
    fleet_db::{FleetDb, State},
    link::Link,
    topic::{self, ReceiveBar, Received, TopicMessage},
};
use futures_lite::StreamExt;
use iroh::{EndpointAddr, TransportAddr, address_lookup::memory::MemoryLookup};
use iroh_gossip::{Gossip, api::GossipSender};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
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
    known: Mutex<HashSet<EndpointId>>,
    bar: Arc<Mutex<ReceiveBar>>,
    subscription: Mutex<Option<Subscription>>,
    subscribing: tokio::sync::Mutex<()>,
    closed: AtomicBool,
}

impl FleetTopic {
    pub(crate) fn new(gossip: Gossip, lookup: MemoryLookup) -> Self {
        Self {
            gossip,
            lookup,
            db: Mutex::new(None),
            known: Mutex::new(HashSet::new()),
            bar: Arc::new(Mutex::new(ReceiveBar::new())),
            subscription: Mutex::new(None),
            subscribing: tokio::sync::Mutex::new(()),
            closed: AtomicBool::new(false),
        }
    }

    pub(crate) fn configure(&self, db: Arc<Mutex<FleetDb>>) {
        *self.db.lock().unwrap() = Some(db);
    }

    pub(crate) fn db(&self) -> Option<Arc<Mutex<FleetDb>>> {
        self.db.lock().ok()?.clone()
    }

    pub(crate) fn refresh(
        &self,
        db: &Mutex<FleetDb>,
        own: EndpointId,
    ) -> Result<Vec<EndpointId>, &'static str> {
        let db = db.lock().map_err(|_| "the fleet store is busy")?;
        let computers = db.computers()?;
        let mut known = self.known.lock().unwrap();
        let mut accepted = HashSet::new();
        for computer in computers.into_iter().filter(|c| c.state == State::Accepted) {
            let key: EndpointId = computer.key.parse().map_err(|_| "not a computer key")?;
            if key != own {
                self.lookup.set_endpoint_info(EndpointAddr::from_parts(
                    key,
                    computer.addrs.into_iter().map(TransportAddr::Ip),
                ));
                accepted.insert(key);
            }
        }
        for key in known.difference(&accepted) {
            self.lookup.remove_endpoint_info(*key);
        }
        *known = accepted;
        Ok(known.iter().copied().collect())
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
        let (link, bar) = (link.clone(), self.bar.clone());
        let neighbors = Arc::new(Mutex::new(HashSet::new()));
        let seen_neighbors = neighbors.clone();
        let task = tokio::spawn(async move {
            while let Some(event) = receiver.next().await {
                *seen_neighbors.lock().unwrap() = receiver.neighbors().collect();
                let Ok(iroh_gossip::api::Event::Received(message)) = event else {
                    continue;
                };
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
        self.drop_subscription();
        for key in self.known.lock().unwrap().drain() {
            self.lookup.remove_endpoint_info(key);
        }
    }

    pub(crate) async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.clear();
        let _ = self.gossip.shutdown().await;
    }
}
