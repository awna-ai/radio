use super::*;
use crate::handler::{Envelope, Handlers, Messages, Ran};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Fake {
    result: Mutex<Option<Ran>>,
    runs: AtomicUsize,
    reasons: Mutex<Vec<String>>,
}
impl Handlers for Fake {
    fn has_handler(&self, _app: &str) -> bool {
        true
    }
    fn run(&self, envelope: &Envelope<'_>) -> Ran {
        assert_eq!(envelope.app, "notes");
        assert_eq!(envelope.body, "opaque");
        assert_eq!(envelope.from_name, Some("garden"));
        self.runs.fetch_add(1, Ordering::SeqCst);
        self.result.lock().unwrap().take().unwrap()
    }
    fn refused(&self, _from: &EndpointId, _name: Option<&str>, _app: &str, reason: &str) {
        self.reasons.lock().unwrap().push(reason.into());
    }
}
#[tokio::test]
async fn handler_results_return_reply_or_one_audited_refusal() {
    for (result, expected) in [
        (
            Ran::Reply {
                reply: "answer".into(),
                exit: 3,
            },
            Answer::Reply {
                reply: "answer".into(),
                exit: 3,
            },
        ),
        (
            Ran::NoHandler,
            Answer::Refused {
                reason: "no_handler".into(),
            },
        ),
        (
            Ran::Timeout,
            Answer::Refused {
                reason: "handler_timeout".into(),
            },
        ),
        (
            Ran::BadReply,
            Answer::Refused {
                reason: "bad_reply".into(),
            },
        ),
    ] {
        let fake = Arc::new(Fake {
            result: Mutex::new(Some(result)),
            runs: AtomicUsize::new(0),
            reasons: Mutex::new(Vec::new()),
        });
        let messages = Arc::new(Messages::new(fake.clone()));
        let envelope = Envelope {
            app: "notes",
            from: SecretKey::generate().public(),
            from_name: Some("garden"),
            id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            body: "opaque",
        };
        assert_eq!(
            run_handler(&messages, &envelope, std::time::Duration::from_secs(5)).await,
            expected
        );
        assert_eq!(fake.runs.load(Ordering::SeqCst), 1);
        match expected {
            Answer::Refused { reason } => assert_eq!(*fake.reasons.lock().unwrap(), [reason]),
            _ => assert!(fake.reasons.lock().unwrap().is_empty()),
        }
    }
}
#[tokio::test]
async fn a_missing_handler_is_refused_without_running() {
    struct Missing(AtomicUsize, Mutex<Vec<String>>);
    impl Handlers for Missing {
        fn has_handler(&self, _app: &str) -> bool {
            false
        }
        fn run(&self, _envelope: &Envelope<'_>) -> Ran {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ran::NoHandler
        }
        fn refused(&self, _from: &EndpointId, _name: Option<&str>, _app: &str, reason: &str) {
            self.1.lock().unwrap().push(reason.into());
        }
    }
    let fake = Arc::new(Missing(AtomicUsize::new(0), Mutex::new(Vec::new())));
    let messages = Arc::new(Messages::new(fake.clone()));
    let envelope = Envelope {
        app: "notes",
        from: SecretKey::generate().public(),
        from_name: None,
        id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        body: "opaque",
    };
    assert_eq!(
        run_handler(&messages, &envelope, std::time::Duration::from_secs(1)).await,
        Answer::Refused {
            reason: "no_handler".into()
        }
    );
    assert_eq!(fake.0.load(Ordering::SeqCst), 0);
    assert_eq!(*fake.1.lock().unwrap(), ["no_handler"]);
}
#[tokio::test]
async fn a_timed_out_handler_holds_slots_until_the_worker_finishes() {
    struct Blocked(Mutex<std::sync::mpsc::Receiver<()>>);
    impl Handlers for Blocked {
        fn has_handler(&self, _app: &str) -> bool {
            true
        }
        fn run(&self, _envelope: &Envelope<'_>) -> Ran {
            self.0.lock().unwrap().recv().unwrap();
            Ran::Reply {
                reply: String::new(),
                exit: 0,
            }
        }
        fn refused(&self, _from: &EndpointId, _name: Option<&str>, _app: &str, _reason: &str) {}
    }
    let (release, blocked) = std::sync::mpsc::channel();
    let messages = Arc::new(Messages::new(Arc::new(Blocked(Mutex::new(blocked)))));
    let key = SecretKey::generate().public();
    let envelope = Envelope {
        app: "notes",
        from: key,
        from_name: None,
        id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        body: "opaque",
    };
    assert_eq!(
        run_handler(&messages, &envelope, std::time::Duration::from_millis(20)).await,
        Answer::Refused {
            reason: "handler_timeout".into()
        }
    );
    assert!(matches!(messages.reserve(key), Err("busy")));
    release.send(()).unwrap();
    timeout(std::time::Duration::from_secs(5), async {
        loop {
            if messages.reserve(key).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn blocked_runs_limit_four_senders_and_one_per_sender_before_calling_the_hook() {
    struct Blocked {
        release: (Mutex<bool>, std::sync::Condvar),
        runs: AtomicUsize,
        reasons: Mutex<Vec<String>>,
    }
    impl Handlers for Blocked {
        fn has_handler(&self, _app: &str) -> bool {
            true
        }
        fn run(&self, _envelope: &Envelope<'_>) -> Ran {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let (released, wake) = &self.release;
            let mut released = released.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            Ran::Reply {
                reply: "answer".into(),
                exit: 0,
            }
        }
        fn refused(&self, _from: &EndpointId, _name: Option<&str>, _app: &str, reason: &str) {
            self.reasons.lock().unwrap().push(reason.into());
        }
    }
    struct Release(Arc<Blocked>);
    impl Drop for Release {
        fn drop(&mut self) {
            *self.0.release.0.lock().unwrap() = true;
            self.0.release.1.notify_all();
        }
    }
    let fake = Arc::new(Blocked {
        release: (Mutex::new(false), std::sync::Condvar::new()),
        runs: AtomicUsize::new(0),
        reasons: Mutex::new(Vec::new()),
    });
    let release = Release(fake.clone());
    let messages = Arc::new(Messages::new(fake.clone()));
    let keys: Vec<_> = (0..5).map(|_| SecretKey::generate().public()).collect();
    let mut workers = Vec::new();
    for key in &keys[..4] {
        let (messages, key) = (messages.clone(), *key);
        workers.push(tokio::spawn(async move {
            run_handler(
                &messages,
                &Envelope {
                    app: "notes",
                    from: key,
                    from_name: None,
                    id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    body: "opaque",
                },
                std::time::Duration::from_secs(10),
            )
            .await
        }));
    }
    let started = timeout(std::time::Duration::from_secs(5), async {
        while fake.runs.load(Ordering::SeqCst) < 4 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(started.is_ok(), "four handler runs must reach the hook");
    for key in [keys[4], keys[0]] {
        assert_eq!(
            run_handler(
                &messages,
                &Envelope {
                    app: "notes",
                    from: key,
                    from_name: None,
                    id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    body: "opaque"
                },
                std::time::Duration::from_secs(1)
            )
            .await,
            Answer::Refused {
                reason: "busy".into()
            }
        );
    }
    assert_eq!(fake.runs.load(Ordering::SeqCst), 4);
    assert_eq!(*fake.reasons.lock().unwrap(), ["busy", "busy"]);
    drop(release);
    for worker in workers {
        assert!(matches!(worker.await.unwrap(), Answer::Reply { .. }));
    }
}
#[test]
fn message_refusal_sentences_are_exact() {
    for (reason, phrase) in [
        ("no_handler", "that app takes no messages on that computer"),
        ("busy", "that computer is busy; try again"),
        (
            "clock",
            "the clocks on the two computers differ by more than 2 minutes",
        ),
        ("replay", "that computer refused the message"),
        ("too_big", "that computer refused the message"),
        (
            "handler_timeout",
            "that app's handler did not finish in time",
        ),
        ("bad_reply", "that app's handler gave an unusable reply"),
        ("other", "the computer did not answer"),
    ] {
        assert_eq!(message_refusal(reason), phrase);
    }
}

#[tokio::test]
async fn a_cancelled_stream_waiter_still_audits_the_handlers_later_refusal() {
    struct Delayed {
        result: Mutex<std::sync::mpsc::Receiver<Ran>>,
        entered: tokio::sync::Notify,
        audited: tokio::sync::Notify,
        reasons: Mutex<Vec<String>>,
    }
    impl Handlers for Delayed {
        fn has_handler(&self, _app: &str) -> bool {
            true
        }
        fn run(&self, _envelope: &Envelope<'_>) -> Ran {
            self.entered.notify_one();
            self.result.lock().unwrap().recv().unwrap()
        }
        fn refused(&self, _from: &EndpointId, _name: Option<&str>, _app: &str, reason: &str) {
            self.reasons.lock().unwrap().push(reason.into());
            self.audited.notify_one();
        }
    }
    for (result, expected) in [
        (Ran::NoHandler, "no_handler"),
        (Ran::Timeout, "handler_timeout"),
        (Ran::BadReply, "bad_reply"),
    ] {
        let (release, result_receiver) = std::sync::mpsc::channel();
        let fake = Arc::new(Delayed {
            result: Mutex::new(result_receiver),
            entered: tokio::sync::Notify::new(),
            audited: tokio::sync::Notify::new(),
            reasons: Mutex::new(Vec::new()),
        });
        let messages = Arc::new(Messages::new(fake.clone()));
        let owned_messages = messages.clone();
        let key = SecretKey::generate().public();
        let waiter = tokio::spawn(async move {
            run_handler(
                &owned_messages,
                &Envelope {
                    app: "notes",
                    from: key,
                    from_name: Some("garden"),
                    id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    body: "opaque",
                },
                std::time::Duration::from_secs(10),
            )
            .await
        });
        let entered = timeout(std::time::Duration::from_secs(5), fake.entered.notified()).await;
        assert!(
            entered.is_ok(),
            "the blocking handler must start before cancellation"
        );
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        release.send(result).unwrap();
        let audited = timeout(std::time::Duration::from_secs(1), fake.audited.notified()).await;
        assert!(
            audited.is_ok(),
            "a cancelled stream waiter must still audit {expected}"
        );
        assert_eq!(*fake.reasons.lock().unwrap(), [expected]);
        assert!(messages.reserve(key).is_ok());
    }
}
