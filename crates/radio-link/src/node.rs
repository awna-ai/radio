use crate::{
    EndpointId, SecretKey,
    admit::{Admission, Limits},
    code::JoinCode,
    computer_record::ComputerRecord,
    fleet_db::{Computer, FleetDb, State},
    join,
    link::{Event, Link, Local},
};
use std::{
    net::SocketAddr,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
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
const STILL_TELLING: &str = "still telling the other computers; check computer_list later";

/// Call from outside the runtime, without holding the fleet store's lock.
pub struct Node {
    runtime: Arc<Runtime>,
    link: Arc<Link>,
    key: SecretKey,
    db: Arc<Mutex<FleetDb>>,
    limits: Limits,
    publish: watch::Sender<Option<ComputerRecord>>,
    published: Arc<AtomicU64>,
    ended: Arc<AtomicBool>,
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
            limits.clone(),
            local,
        ))?);
        let (stop, stopped) = oneshot::channel();
        let (publish, records) = watch::channel::<Option<ComputerRecord>>(None);
        let ended = Arc::new(AtomicBool::new(false));
        let published = Arc::new(AtomicU64::new(0));
        let (driver_ended, driver_published) = (ended.clone(), published.clone());
        let (driver_runtime, driver_link, driver_key, driver_db) =
            (runtime.clone(), link.clone(), key.clone(), db.clone());
        let driver = std::thread::spawn(move || {
            driver_runtime.block_on(drive(
                driver_link,
                driver_key,
                driver_db,
                stopped,
                records,
                driver_ended.clone(),
                driver_published,
            ));
            driver_ended.store(true, Ordering::Release);
        });
        Ok(Self {
            runtime,
            link,
            key,
            db,
            limits,
            publish,
            published,
            ended,
            stop: Some(stop),
            driver: Some(driver),
        })
    }

    pub fn running(&self) -> bool {
        !self.ended.load(Ordering::Acquire)
            && self
                .driver
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
        if !self.running() {
            return Err("the link stopped");
        }
        let (link, key, db) = (self.link.clone(), self.key.clone(), self.db.clone());
        let (sender, receiver) = sync_channel(1);
        self.runtime.handle().spawn(async move {
            let result = join::join(&link, &key, &db, &code, now).await;
            let _ = sender.send(result);
        });
        wait(receiver, WAIT, STILL_TELLING)?
    }

    pub fn remove(&self, target: &str, now: f64) -> Result<usize, &'static str> {
        if !self.running() {
            return Err("the link stopped");
        }
        let (link, key, db) = (self.link.clone(), self.key.clone(), self.db.clone());
        let target = target.to_owned();
        let (sender, receiver) = sync_channel(1);
        self.runtime.handle().spawn(async move {
            let result = join::remove(&link, &key, &db, &target, now).await;
            let _ = sender.send(result);
        });
        wait(receiver, WAIT, STILL_TELLING)?
    }

    pub fn leave(&self, now: f64) -> Result<usize, &'static str> {
        if !self.running() {
            return Err("the link stopped");
        }
        let (link, key, db) = (self.link.clone(), self.key.clone(), self.db.clone());
        let (sender, receiver) = sync_channel(1);
        self.runtime.handle().spawn(async move {
            let result = join::leave(&link, &key, &db, now).await;
            let _ = sender.send(result);
        });
        wait(receiver, WAIT, STILL_TELLING)?
    }

    pub fn start_send(
        &self,
        computer: &str,
        app: &str,
        name: &str,
        path: &Path,
    ) -> Result<PendingSend, &'static str> {
        let (file, size) = open_file(name, path)?;
        let computer = self
            .db
            .lock()
            .map_err(|_| "the fleet store is busy")?
            .computer(computer)?;
        if computer.state != State::Accepted {
            return Err("the computer is not let in");
        }
        if computer.addrs.is_empty() {
            return Err(crate::endpoint::NO_ADDRESS);
        }
        let id: EndpointId = computer.key.parse().map_err(|_| "not a computer key")?;
        if !self.running() {
            return Err("the link stopped");
        }
        Ok(PendingSend {
            runtime: self.runtime.handle().clone(),
            link: self.link.clone(),
            id,
            addrs: computer.addrs,
            app: app.to_owned(),
            name: name.to_owned(),
            file,
            size,
            timeout: self.limits.file
                + self.limits.dial
                + self.limits.frame
                + Duration::from_secs(5),
        })
    }

    pub fn send_file(
        &self,
        computer: &str,
        app: &str,
        name: &str,
        path: &Path,
    ) -> Result<u64, &'static str> {
        self.start_send(computer, app, name, path)?.wait()
    }

    pub fn block(&self, target: &str) -> Result<Computer, &'static str> {
        join::block(&self.link, &self.db, target)
    }

    #[doc(hidden)]
    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }

    pub fn publish(&self, record: ComputerRecord) {
        let _ = self.publish.send(Some(record));
    }
}

pub struct PendingSend {
    runtime: tokio::runtime::Handle,
    link: Arc<Link>,
    id: EndpointId,
    addrs: Vec<SocketAddr>,
    app: String,
    name: String,
    file: std::fs::File,
    size: u64,
    timeout: Duration,
}

impl PendingSend {
    pub fn wait(self) -> Result<u64, &'static str> {
        let (sender, receiver) = sync_channel(1);
        self.runtime.spawn(async move {
            let result = async {
                let peer = self.link.dial(self.id, &self.addrs).await?;
                peer.send_file(&self.app, &self.name, self.file, self.size)
                    .await?;
                Ok(self.size)
            }
            .await;
            let _ = sender.send(result);
        });
        wait(receiver, self.timeout, "the send did not finish in time")?
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
    ended: Arc<AtomicBool>,
    published: Arc<AtomicU64>,
) {
    let (sender, mut events) = mpsc::channel(64);
    let server = link.serve(sender);
    let mut joins = JoinSet::new();
    let (publisher_link, publisher_db) = (link.clone(), db.clone());
    joins.spawn(async move {
        while records.changed().await.is_ok() {
            let record = records.borrow_and_update().clone();
            if let Some(record) = record {
                published.fetch_add(1, Ordering::Relaxed);
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
                Some(Event::Arrived { from, app, name, size }) => {
                    if let Ok(mut db) = db.lock() {
                        let computer = db.computer(&from.to_string()).ok()
                            .and_then(|computer| computer.name)
                            .unwrap_or_else(|| from.to_string());
                        let _ = db.add_arrival(&app, &name, size, &computer, &from.to_string(), now());
                    }
                }
                Some(_) => {},
                None => break false,
            },
            _ = joins.join_next(), if !joins.is_empty() => {},
        }
    };
    ended.store(true, Ordering::Release);
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

fn open_file(name: &str, path: &Path) -> Result<(std::fs::File, u64), &'static str> {
    if !crate::wire::file_name(name) {
        return Err("not a file name");
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "the file could not be opened")?;
    let metadata = file
        .metadata()
        .map_err(|_| "the file could not be opened")?;
    if !metadata.is_file() {
        return Err("not a regular file");
    }
    if metadata.len() > crate::wire::FILE_CAP {
        return Err("the file is larger than 64 MiB");
    }
    Ok((file, metadata.len()))
}

fn wait<T>(
    receiver: Receiver<T>,
    timeout: Duration,
    message: &'static str,
) -> Result<T, &'static str> {
    receiver.recv_timeout(timeout).map_err(|error| match error {
        RecvTimeoutError::Timeout => message,
        RecvTimeoutError::Disconnected => "the link stopped",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_wait_uses_its_timeout_sentence() {
        let (_sender, receiver) = sync_channel::<()>(1);
        assert_eq!(
            wait(receiver, Duration::ZERO, "the send did not finish in time"),
            Err("the send did not finish in time")
        );
    }

    #[test]
    fn a_source_fifo_open_does_not_wait_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let path = std::env::temp_dir().join(format!("radio-node-pipe-{}", std::process::id()));
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the path is NUL-terminated and valid for the call.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let (sent, received) = sync_channel(1);
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            let _ = sent.send(open_file("letter", &worker_path).err());
        });
        let result = received.recv_timeout(Duration::from_secs(2));
        std::fs::remove_file(path).unwrap();
        assert_eq!(result, Ok(Some("not a regular file")));
        worker.join().unwrap();
    }

    #[test]
    fn file_prechecks_reject_invalid_names_sources_and_size() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!("radio-node-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source");
        std::fs::write(&path, b"hello").unwrap();
        for name in ["../x", "a/b", ".hidden", "", &"x".repeat(256), "line\nfeed"] {
            assert_eq!(open_file(name, &path).err(), Some("not a file name"));
        }
        let link = dir.join("link");
        symlink(&path, &link).unwrap();
        assert_eq!(
            open_file("letter", &link).err(),
            Some("the file could not be opened")
        );
        assert_eq!(open_file("letter", &dir).err(), Some("not a regular file"));
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(crate::wire::FILE_CAP + 1)
            .unwrap();
        assert_eq!(
            open_file("letter", &path).err(),
            Some("the file is larger than 64 MiB")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_wait_times_out_with_the_delivery_phrase() {
        let (sender, receiver) = sync_channel::<()>(1);
        assert_eq!(
            wait(
                receiver,
                Duration::ZERO,
                "still telling the other computers; check computer_list later"
            ),
            Err("still telling the other computers; check computer_list later")
        );
        assert!(sender.send(()).is_err());
        assert_eq!(WAIT, Duration::from_secs(110));
    }

    #[test]
    fn a_send_spawned_after_runtime_shutdown_reports_the_link_stopped() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let handle = runtime.handle().clone();
        drop(runtime);
        let (sender, receiver) = sync_channel(1);
        handle.spawn(async move {
            let _ = sender.send(Ok::<_, &'static str>(0u64));
        });
        assert_eq!(
            wait(
                receiver,
                Duration::from_secs(1),
                "the send did not finish in time"
            ),
            Err("the link stopped")
        );
    }

    #[test]
    fn a_disconnected_wait_reports_the_link_stopped() {
        let (sender, receiver) = sync_channel::<()>(1);
        drop(sender);
        assert_eq!(
            wait(
                receiver,
                WAIT,
                "still telling the other computers; check computer_list later"
            ),
            Err("the link stopped")
        );
    }
}
