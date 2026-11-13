use crate::{
    EndpointId, SecretKey,
    admit::{Admission, Limits},
    code::JoinCode,
    computer_record::ComputerRecord,
    fleet_db::{Computer, FleetDb},
    join,
    link::{Event, Link, Local},
};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, sync_channel},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    runtime::Runtime,
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};

pub(crate) const WAIT: Duration = Duration::from_secs(110);

/// Call from outside the runtime, without holding the fleet store's lock.
pub struct Node {
    runtime: Arc<Runtime>,
    link: Arc<Link>,
    key: SecretKey,
    db: Arc<Mutex<FleetDb>>,
    publish: watch::Sender<Option<ComputerRecord>>,
    stop: Option<oneshot::Sender<()>>,
    driver: Option<JoinHandle<()>>,
}

impl Node {
    pub fn start(
        key: SecretKey,
        addrs: &[SocketAddr],
        db: Arc<Mutex<FleetDb>>,
        local: Local,
        limits: Limits,
    ) -> Result<Self, &'static str> {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "the link could not start")?,
        );
        let link = Arc::new(runtime.block_on(Link::bind(
            key.clone(),
            addrs,
            db.clone() as Arc<dyn Admission>,
            limits,
            local,
        ))?);
        let (stop, stopped) = oneshot::channel();
        let (publish, records) = watch::channel::<Option<ComputerRecord>>(None);
        let (driver_runtime, driver_link, driver_key, driver_db) =
            (runtime.clone(), link.clone(), key.clone(), db.clone());
        let driver = std::thread::spawn(move || {
            driver_runtime.block_on(drive(driver_link, driver_key, driver_db, stopped, records));
        });
        Ok(Self {
            runtime,
            link,
            key,
            db,
            publish,
            stop: Some(stop),
            driver: Some(driver),
        })
    }

    pub fn running(&self) -> bool {
        self.driver
            .as_ref()
            .is_some_and(|driver| !driver.is_finished())
    }

    pub fn id(&self) -> EndpointId {
        self.link.id()
    }

    pub fn addrs(&self) -> Vec<SocketAddr> {
        self.link.addrs()
    }

    #[doc(hidden)]
    pub fn link(&self) -> &Link {
        &self.link
    }

    pub fn join(&self, code: JoinCode, now: f64) -> Result<(), &'static str> {
        if self
            .driver
            .as_ref()
            .is_some_and(|driver| driver.is_finished())
        {
            return Err("the link stopped");
        }
        let (link, key, db) = (self.link.clone(), self.key.clone(), self.db.clone());
        let (sender, receiver) = sync_channel(1);
        self.runtime.handle().spawn(async move {
            let result = join::join(&link, &key, &db, &code, now).await;
            let _ = sender.send(result);
        });
        wait(receiver, WAIT)?
    }

    pub fn remove(&self, target: &str, now: f64) -> Result<usize, &'static str> {
        if self
            .driver
            .as_ref()
            .is_some_and(|driver| driver.is_finished())
        {
            return Err("the link stopped");
        }
        let (link, key, db) = (self.link.clone(), self.key.clone(), self.db.clone());
        let target = target.to_owned();
        let (sender, receiver) = sync_channel(1);
        self.runtime.handle().spawn(async move {
            let result = join::remove(&link, &key, &db, &target, now).await;
            let _ = sender.send(result);
        });
        wait(receiver, WAIT)?
    }

    pub fn leave(&self, now: f64) -> Result<usize, &'static str> {
        if self
            .driver
            .as_ref()
            .is_some_and(|driver| driver.is_finished())
        {
            return Err("the link stopped");
        }
        let (link, key, db) = (self.link.clone(), self.key.clone(), self.db.clone());
        let (sender, receiver) = sync_channel(1);
        self.runtime.handle().spawn(async move {
            let result = join::leave(&link, &key, &db, now).await;
            let _ = sender.send(result);
        });
        wait(receiver, WAIT)?
    }

    pub fn block(&self, target: &str) -> Result<Computer, &'static str> {
        join::block(&self.link, &self.db, target)
    }

    pub fn publish(&self, record: ComputerRecord) {
        let _ = self.publish.send(Some(record));
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(driver) = self.driver.take() {
            let _ = driver.join();
        }
    }
}

async fn drive(
    link: Arc<Link>,
    key: SecretKey,
    db: Arc<Mutex<FleetDb>>,
    mut stop: oneshot::Receiver<()>,
    mut records: watch::Receiver<Option<ComputerRecord>>,
) {
    let (sender, mut events) = mpsc::channel(64);
    let server = link.serve(sender);
    let mut joins = JoinSet::new();
    let (publisher_link, publisher_db) = (link.clone(), db.clone());
    joins.spawn(async move {
        while records.changed().await.is_ok() {
            let record = records.borrow_and_update().clone();
            if let Some(record) = record {
                join::publish(&publisher_link, &publisher_db, &record).await;
            }
        }
    });
    let stopped = loop {
        tokio::select! {
            _ = &mut stop => break true,
            event = events.recv() => match event {
                Some(Event::Join { connection, .. }) => {
                    let (link, key, db) = (link.clone(), key.clone(), db.clone());
                    joins.spawn(async move {
                        if let Ok(Some(record)) = join::answer(&link, &key, &db, connection, now()).await {
                            join::introduce(&link, &db, &record).await;
                        }
                    });
                }
                Some(Event::Record { record, .. }) => {
                    let _ = join::apply(&link, &db, &record, now());
                }
                Some(Event::Computer { record, .. }) => {
                    let _ = join::apply_computer(&link, &db, &record);
                }
                Some(_) => {},
                None => break false,
            },
            _ = joins.join_next(), if !joins.is_empty() => {},
        }
    };
    if !stopped {
        let _ = (&mut stop).await;
    }
    server.abort();
    joins.abort_all();
    let _ = tokio::time::timeout(Duration::from_secs(1), link.close()).await;
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn wait<T>(receiver: Receiver<T>, timeout: Duration) -> Result<T, &'static str> {
    receiver.recv_timeout(timeout).map_err(|error| match error {
        RecvTimeoutError::Timeout => "still telling the other computers; check computer_list later",
        RecvTimeoutError::Disconnected => "the link stopped",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_times_out_with_the_delivery_phrase() {
        let (sender, receiver) = sync_channel::<()>(1);
        assert_eq!(
            wait(receiver, Duration::ZERO),
            Err("still telling the other computers; check computer_list later")
        );
        assert!(sender.send(()).is_err());
        assert_eq!(WAIT, Duration::from_secs(110));
    }

    #[test]
    fn a_disconnected_wait_reports_the_link_stopped() {
        let (sender, receiver) = sync_channel::<()>(1);
        drop(sender);
        assert_eq!(wait(receiver, WAIT), Err("the link stopped"));
    }
}
