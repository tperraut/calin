use std::time::Duration;

use super::*;
use crate::clock::ManualClock;
use crate::ece::tests::subscriber;

const STRICT: EndpointPolicy = EndpointPolicy::PublicHttpsOnly;
const START: u64 = 1_800_000_000_000;

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    clock: Arc<ManualClock>,
    store: SubscriptionStore,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("push.json");
    let clock = Arc::new(ManualClock::new(START));
    let store = SubscriptionStore::open(&path, clock.clone()).await.unwrap();
    Fixture {
        _dir: dir,
        path,
        clock,
        store,
    }
}

impl Fixture {
    async fn reopen(&self) -> SubscriptionStore {
        SubscriptionStore::open(&self.path, self.clock.clone())
            .await
            .unwrap()
    }
}

fn request(endpoint: &str) -> PushSubscriptionRequest {
    let (_, keys) = subscriber();
    PushSubscriptionRequest {
        endpoint: endpoint.to_owned(),
        p256dh: base64url(&keys.p256dh),
        auth: base64url(&keys.auth),
        label: None,
    }
}

const FIREFOX: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0";

#[tokio::test]
async fn a_subscription_is_listed_without_its_endpoint() {
    let f = fixture().await;
    let endpoint = "https://updates.push.services.mozilla.com/wpush/v2/gAAAA";
    let info = f
        .store
        .subscribe("alice", &request(endpoint), Some(FIREFOX), STRICT)
        .await
        .unwrap();
    assert_eq!(info.label, "Firefox on Linux");
    assert_eq!(info.service, "updates.push.services.mozilla.com");
    assert_eq!(info.endpoint_sha256, hex(&sha256(endpoint.as_bytes())));
    assert_eq!(info.endpoint_sha256.len(), 64);
    assert_eq!(info.created_at, START as i64);
    assert_eq!(info.last_sent_at, None);
    assert_eq!(f.store.list("alice"), vec![info.clone()]);
    assert!(f.store.list("bob").is_empty());
    // On disk at once, and read back the same.
    assert_eq!(f.reopen().await.list("alice"), vec![info]);
}

#[tokio::test]
async fn labels_are_given_cleaned_or_derived() {
    let f = fixture().await;
    let mut given = request("https://push.example.net/1");
    given.label = Some("  My\nphone  ".to_owned());
    let info = f
        .store
        .subscribe("a", &given, Some(FIREFOX), STRICT)
        .await
        .unwrap();
    assert_eq!(info.label, "Myphone");

    let mut long = request("https://push.example.net/2");
    long.label = Some("x".repeat(500));
    let info = f.store.subscribe("a", &long, None, STRICT).await.unwrap();
    assert_eq!(info.label.chars().count(), MAX_LABEL_CHARS);

    let mut blank = request("https://push.example.net/3");
    blank.label = Some(" ".to_owned());
    let info = f.store.subscribe("a", &blank, None, STRICT).await.unwrap();
    assert_eq!(info.label, "Unknown browser");
}

#[tokio::test]
async fn subscription_validation_table() {
    let f = fixture().await;
    let (_, keys) = subscriber();
    let good_p256dh = base64url(&keys.p256dh);
    let mut off_curve = keys.p256dh;
    off_curve[40] ^= 0x55;
    let mut compressed = keys.p256dh[..33].to_vec();
    compressed[0] = 0x02;
    let mut bad_tag = keys.p256dh;
    bad_tag[0] = 0x05;

    let p256dh_cases = [
        ("not base64!".to_owned(), false),
        (String::new(), false),
        (base64url(&keys.p256dh[..64]), false),
        (base64url(&[keys.p256dh.as_slice(), &[0]].concat()), false),
        (base64url(&off_curve), false),
        (base64url(&compressed), false),
        (base64url(&bad_tag), false),
        (base64url(&[0u8; 65]), false),
        (format!("{good_p256dh}="), true),
        (good_p256dh.clone(), true),
    ];
    for (index, (p256dh, ok)) in p256dh_cases.into_iter().enumerate() {
        let mut r = request(&format!("https://push.example.net/k{index}"));
        r.p256dh = p256dh.clone();
        let result = f.store.subscribe("a", &r, None, STRICT).await;
        assert_eq!(result.is_ok(), ok, "{p256dh:?}: {result:?}");
        if !ok {
            assert!(
                matches!(result, Err(SubscribeError::InvalidP256dh)),
                "{p256dh:?}"
            );
        }
    }

    for (auth, ok) in [
        (base64url(&[7u8; 16]), true),
        (format!("{}==", base64url(&[7u8; 16])), true),
        (base64url(&[7u8; 15]), false),
        (base64url(&[7u8; 17]), false),
        (String::new(), false),
        ("%%%".to_owned(), false),
    ] {
        let mut r = request("https://push.example.net/auth");
        r.auth = auth.clone();
        let result = f.store.subscribe("a", &r, None, STRICT).await;
        assert_eq!(result.is_ok(), ok, "{auth:?}");
        if !ok {
            assert!(
                matches!(result, Err(SubscribeError::InvalidAuth)),
                "{auth:?}"
            );
        }
    }

    for endpoint in [
        "http://push.example.net/p",
        "https://127.0.0.1/p",
        "https://localhost/p",
        "https://push.example.net:8443/p",
        "https://u:p@push.example.net/p",
        "https://x.internal/p",
    ] {
        let result = f
            .store
            .subscribe("a", &request(endpoint), None, STRICT)
            .await;
        assert!(
            matches!(result, Err(SubscribeError::Endpoint(_))),
            "{endpoint}"
        );
    }
    let too_long = format!("https://push.example.net/{}", "a".repeat(2048));
    assert!(matches!(
        f.store
            .subscribe("a", &request(&too_long), None, STRICT)
            .await,
        Err(SubscribeError::Endpoint(EndpointError::TooLong))
    ));
    // Development only.
    let loopback = request("http://127.0.0.1:9/p");
    assert!(
        f.store
            .subscribe("a", &loopback, None, STRICT)
            .await
            .is_err()
    );
    assert!(
        f.store
            .subscribe(
                "a",
                &loopback,
                None,
                EndpointPolicy::AllowLoopbackHttpForDevelopment
            )
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn resubscribing_an_endpoint_replaces_it_and_keeps_its_id() {
    let f = fixture().await;
    let endpoint = "https://fcm.googleapis.com/fcm/send/abc";
    let first = f
        .store
        .subscribe("alice", &request(endpoint), Some(FIREFOX), STRICT)
        .await
        .unwrap();
    f.clock.advance(Duration::from_secs(60));
    let mut again = request(endpoint);
    again.label = Some("Phone".to_owned());
    let second = f
        .store
        .subscribe("alice", &again, None, STRICT)
        .await
        .unwrap();
    assert_eq!(second.id, first.id);
    assert_eq!(second.created_at, first.created_at);
    assert_eq!(second.label, "Phone");
    assert_eq!(f.store.list("alice"), vec![second]);
    let (_, targets) = f.store.targets("alice");
    assert_eq!(base64url(&targets[0].keys.p256dh), again.p256dh, "new keys");
    assert_eq!(base64url(&targets[0].keys.auth), again.auth);
}

#[tokio::test]
async fn an_endpoint_belongs_to_the_identity_that_subscribed_last() {
    let f = fixture().await;
    let endpoint = "https://fcm.googleapis.com/fcm/send/shared";
    f.store
        .subscribe("alice", &request(endpoint), None, STRICT)
        .await
        .unwrap();
    f.store
        .subscribe("bob", &request(endpoint), None, STRICT)
        .await
        .unwrap();
    assert!(f.store.list("alice").is_empty());
    assert_eq!(f.store.list("bob").len(), 1);
}

#[tokio::test]
async fn the_oldest_subscription_goes_past_the_cap() {
    let f = fixture().await;
    let mut ids = Vec::new();
    for index in 0..=MAX_SUBSCRIPTIONS_PER_IDENTITY {
        let info = f
            .store
            .subscribe(
                "alice",
                &request(&format!("https://push.example.net/{index}")),
                None,
                STRICT,
            )
            .await
            .unwrap();
        ids.push(info.id);
        f.clock.advance(Duration::from_secs(1));
    }
    let listed: Vec<String> = f.store.list("alice").into_iter().map(|i| i.id).collect();
    assert_eq!(listed.len(), MAX_SUBSCRIPTIONS_PER_IDENTITY);
    assert_eq!(listed, ids[1..].to_vec());
    // Another identity is not affected.
    f.store
        .subscribe("bob", &request("https://push.example.net/b"), None, STRICT)
        .await
        .unwrap();
    assert_eq!(f.store.list("bob").len(), 1);

    // Even when the clock stepped back, the newest is the one kept.
    f.clock.set(START - 1_000_000);
    let newest = f
        .store
        .subscribe(
            "alice",
            &request("https://push.example.net/late"),
            None,
            STRICT,
        )
        .await
        .unwrap();
    let listed = f.store.list("alice");
    assert_eq!(listed.len(), MAX_SUBSCRIPTIONS_PER_IDENTITY);
    assert!(listed.iter().any(|i| i.id == newest.id));
}

#[tokio::test]
async fn removal_is_by_id_within_the_identity() {
    let f = fixture().await;
    let a = f
        .store
        .subscribe(
            "alice",
            &request("https://push.example.net/a"),
            None,
            STRICT,
        )
        .await
        .unwrap();
    assert!(!f.store.remove("bob", &a.id).await.unwrap());
    assert!(!f.store.remove("alice", "nope").await.unwrap());
    assert_eq!(f.store.list("alice").len(), 1);
    assert!(f.store.remove("alice", &a.id).await.unwrap());
    assert!(f.store.list("alice").is_empty());
    assert!(f.reopen().await.list("alice").is_empty());
}

#[tokio::test]
async fn preferences_default_to_everything_and_persist() {
    let f = fixture().await;
    assert_eq!(f.store.preferences("alice"), PushPreferences::default());
    assert_eq!(
        PushPreferences::default(),
        PushPreferences {
            waiting: true,
            review: true,
            failed: true,
            merged: true
        }
    );
    let quiet = PushPreferences {
        waiting: true,
        review: false,
        failed: true,
        merged: false,
    };
    f.store.set_preferences("alice", quiet).await.unwrap();
    assert_eq!(f.store.preferences("alice"), quiet);
    assert_eq!(f.store.preferences("bob"), PushPreferences::default());
    assert_eq!(f.reopen().await.preferences("alice"), quiet);
    // Back to the defaults: nothing left to keep for this identity.
    f.store
        .set_preferences("alice", PushPreferences::default())
        .await
        .unwrap();
    let file = std::fs::read_to_string(&f.path).unwrap();
    assert!(!file.contains("alice"), "{file}");
}

#[cfg(unix)]
#[tokio::test]
async fn the_file_is_private_and_written_whole() {
    use std::os::unix::fs::PermissionsExt as _;
    let f = fixture().await;
    assert!(!f.path.exists(), "nothing written before a change");
    f.store
        .subscribe(
            "alice",
            &request("https://push.example.net/a"),
            None,
            STRICT,
        )
        .await
        .unwrap();
    assert_eq!(crate::fsio::mode(&f.path), 0o600);
    assert!(!f.path.with_file_name("push.json.tmp").exists());
    let parsed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&f.path).unwrap()).unwrap();
    assert_eq!(parsed["version"], 1);

    std::fs::set_permissions(&f.path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let reopened = f.reopen().await;
    assert!(reopened.load_report().permissions_corrected);
    assert_eq!(crate::fsio::mode(&f.path), 0o600);
    assert_eq!(
        reopened.load_report().file,
        LoadedFile::Loaded { subscriptions: 1 }
    );
}

#[tokio::test]
async fn every_write_replaces_the_file_atomically() {
    // Many writers at once: each snapshot lands whole, never interleaved.
    let f = fixture().await;
    let mut tasks = Vec::new();
    for index in 0..16 {
        let store = f.store.clone();
        tasks.push(tokio::spawn(async move {
            store
                .subscribe(
                    "alice",
                    &request(&format!("https://push.example.net/{index}")),
                    None,
                    STRICT,
                )
                .await
                .unwrap()
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let reopened = f.reopen().await;
    assert_eq!(reopened.list("alice").len(), 16);
    assert_eq!(reopened.list("alice"), f.store.list("alice"));
    let leftovers: Vec<_> = std::fs::read_dir(f.path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from("push.json")]);
}

#[tokio::test]
async fn a_corrupt_store_is_moved_aside_and_reported_once() {
    let f = fixture().await;
    for content in [
        "{not json",
        "[]",
        "",
        "{\"version\": 9, \"identities\": {}}",
    ] {
        std::fs::write(&f.path, content).unwrap();
        let store = f.reopen().await;
        let LoadedFile::Corrupt { reason, kept_as } = &store.load_report().file else {
            panic!("{content:?}: {:?}", store.load_report());
        };
        assert!(!reason.is_empty());
        assert_eq!(kept_as, &f.path.with_file_name("push.json.corrupt"));
        assert_eq!(std::fs::read_to_string(kept_as).unwrap(), content);
        assert!(store.list("alice").is_empty());

        // The store works, and the next start does not report it again.
        store
            .subscribe(
                "alice",
                &request("https://push.example.net/a"),
                None,
                STRICT,
            )
            .await
            .unwrap();
        let again = f.reopen().await;
        assert_eq!(
            again.load_report().file,
            LoadedFile::Loaded { subscriptions: 1 }
        );
        std::fs::remove_file(&f.path).unwrap();
    }
}

#[tokio::test]
async fn invalid_records_are_dropped_on_load() {
    let f = fixture().await;
    f.store
        .subscribe(
            "alice",
            &request("https://push.example.net/good"),
            None,
            STRICT,
        )
        .await
        .unwrap();
    f.store
        .subscribe(
            "alice",
            &request("https://push.example.net/bad"),
            None,
            STRICT,
        )
        .await
        .unwrap();
    f.store
        .subscribe(
            "alice",
            &request("https://push.example.net/bad2"),
            None,
            STRICT,
        )
        .await
        .unwrap();
    let file = std::fs::read_to_string(&f.path).unwrap();
    // Both `bad` endpoints, in the one identity.
    std::fs::write(
        &f.path,
        file.replace("https://push.example.net/bad", "https://localhost/bad"),
    )
    .unwrap();
    let store = f.reopen().await;
    assert_eq!(store.load_report().invalid_dropped, 2);
    assert_eq!(store.list("alice").len(), 1);
    assert!(
        !std::fs::read_to_string(&f.path)
            .unwrap()
            .contains("localhost")
    );
}

#[tokio::test]
async fn last_success_writes_are_coalesced() {
    let f = fixture().await;
    let info = f
        .store
        .subscribe(
            "alice",
            &request("https://push.example.net/a"),
            None,
            STRICT,
        )
        .await
        .unwrap();
    let writes = f.store.writes();
    for _ in 0..10 {
        f.clock.advance(Duration::from_secs(1));
        f.store.record_success("alice", &info.id).await;
    }
    assert_eq!(f.store.writes(), writes, "nothing written within 30 s");
    assert_eq!(
        f.store.list("alice")[0].last_sent_at,
        Some((START + 10_000) as i64),
        "but known in memory"
    );
    assert_eq!(f.reopen().await.list("alice")[0].last_sent_at, None);

    f.clock.advance(Duration::from_secs(30));
    f.store.record_success("alice", &info.id).await;
    assert_eq!(f.store.writes(), writes + 1);
    assert_eq!(
        f.reopen().await.list("alice")[0].last_sent_at,
        Some((START + 40_000) as i64)
    );

    f.clock.advance(Duration::from_secs(1));
    f.store.record_success("alice", &info.id).await;
    assert_eq!(f.store.writes(), writes + 1);
    f.store.flush().await.unwrap();
    assert_eq!(f.store.writes(), writes + 2);
    f.store.flush().await.unwrap();
    assert_eq!(f.store.writes(), writes + 2, "nothing left to write");
    // An unknown id changes nothing.
    f.store.record_success("alice", "nope").await;
    f.store.record_success("bob", &info.id).await;
    f.store.flush().await.unwrap();
    assert_eq!(f.store.writes(), writes + 2);
}

#[tokio::test]
async fn gone_removes_only_the_same_endpoint() {
    let f = fixture().await;
    let info = f
        .store
        .subscribe(
            "alice",
            &request("https://push.example.net/a"),
            None,
            STRICT,
        )
        .await
        .unwrap();
    let other = Url::parse("https://push.example.net/other").unwrap();
    assert!(
        !f.store
            .remove_gone("alice", &info.id, &other)
            .await
            .unwrap()
    );
    let url = Url::parse("https://push.example.net/a").unwrap();
    assert!(!f.store.remove_gone("bob", &info.id, &url).await.unwrap());
    assert!(f.store.remove_gone("alice", &info.id, &url).await.unwrap());
    assert!(f.reopen().await.list("alice").is_empty());
}

#[tokio::test]
async fn an_unwritable_directory_fails_at_open() {
    let dir = tempfile::tempdir().unwrap();
    let error = SubscriptionStore::open(
        dir.path().join("missing/push.json"),
        Arc::new(ManualClock::new(START)),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, StoreError::Io { .. }));
}

#[tokio::test]
async fn a_store_file_that_cannot_be_read_fails_at_open_and_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    // A directory where the file should be: there, but not readable as one.
    let path = dir.path().join("push.json");
    std::fs::create_dir(&path).unwrap();
    let error = SubscriptionStore::open(&path, Arc::new(ManualClock::new(START)))
        .await
        .unwrap_err();
    let StoreError::Io { path: named, .. } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(named, &path);
    assert!(path.is_dir());
    assert!(!corrupt_path(&path).exists());
}

#[tokio::test]
async fn debug_shows_no_endpoint_or_key() {
    let f = fixture().await;
    let r = request("https://push.example.net/secret-capability-path");
    f.store.subscribe("alice", &r, None, STRICT).await.unwrap();
    let (_, targets) = f.store.targets("alice");
    for shown in [
        format!("{:?}", f.store),
        format!("{:?}", targets),
        format!("{:?}", f.store.path()),
    ] {
        assert!(!shown.contains("secret-capability-path"), "{shown}");
        assert!(!shown.contains(&r.p256dh), "{shown}");
        assert!(!shown.contains(&r.auth), "{shown}");
    }
    assert!(format!("{targets:?}").contains("push.example.net"));
}
