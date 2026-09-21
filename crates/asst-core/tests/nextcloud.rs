//! Against a real server, in a throwaway list it creates and removes.
//!
//!   ASST_TEST_DAV_URL=https://cloud.example/remote.php/dav/ \
//!   ASST_TEST_USERNAME=me ASST_TEST_PASSWORD=app-password \
//!   cargo test -p asst-core --test nextcloud -- --ignored --nocapture

use asst_core::caldav::{Account, CalDav, RemoteError};
use asst_core::ical::Ical;
use asst_core::task::{self, Edit, Task};

fn account() -> Option<Account> {
    Some(Account {
        dav_url: std::env::var("ASST_TEST_DAV_URL").ok()?,
        username: std::env::var("ASST_TEST_USERNAME").ok()?,
        password: std::env::var("ASST_TEST_PASSWORD").ok()?,
    })
}

#[tokio::test]
#[ignore = "needs a CalDAV server"]
async fn round_trip_against_the_server() {
    let Some(account) = account() else {
        eprintln!("ASST_TEST_* not set; skipping");
        return;
    };
    let local = asst_core::time::local_zone();
    let dav = CalDav::discover(&account).await.expect("discovery");
    println!("calendar home: {}", dav.home());

    let lists = dav.lists().await.expect("lists");
    for l in &lists {
        println!(
            "list {:?} token={:?} writable={}",
            l.name, l.sync_token, l.writable
        );
    }
    for l in &lists {
        let changes = dav.changes(&l.href, None).await.expect("initial report");
        let fetched = dav
            .fetch(
                &l.href,
                &changes
                    .changed
                    .iter()
                    .map(|(h, _)| h.clone())
                    .collect::<Vec<_>>(),
            )
            .await
            .expect("multiget");
        // Every object the server holds must survive a parse and write-back untouched.
        for f in &fetched {
            assert_eq!(
                Ical::parse(&f.data).to_string(),
                f.data,
                "round trip of {}",
                f.href
            );
        }
        let again = dav
            .changes(&l.href, changes.token.as_deref())
            .await
            .expect("delta");
        println!(
            "  {}: {} objects, delta since token: {} changed",
            l.name,
            fetched.len(),
            again.changed.len()
        );
    }

    let slug = format!("asst-test-{}", std::process::id());
    let list = dav
        .create_list(&slug, "asst test", Some("#8E44AD"))
        .await
        .expect("MKCOL");
    let result = exercise(&dav, &list, local).await;
    dav.delete_list(&list, true)
        .await
        .expect("remove the test list");
    result.expect("exercise");
}

async fn exercise(dav: &CalDav, list: &str, local: chrono_tz::Tz) -> Result<(), String> {
    let t0 = dav
        .changes(list, None)
        .await
        .map_err(|e| e.to_string())?
        .token
        .ok_or("no token")?;

    let now = chrono::Utc::now();
    let uid = uuid::Uuid::new_v4().to_string();
    let mut ical = task::new_ical(&uid, now);
    task::apply(
        &mut ical,
        &[
            Edit::Summary("asst test, created".into()),
            Edit::Priority(5),
        ],
        now,
        local,
    )
    .unwrap();
    let href = format!("{list}{uid}.ics");
    let body = ical.to_string();

    let e1 = dav
        .create(&href, &body)
        .await
        .map_err(|e| format!("create: {e}"))?;
    println!("PUT create: etag {e1:?}");
    let fetched = dav
        .fetch(list, std::slice::from_ref(&href))
        .await
        .map_err(|e| e.to_string())?;
    let server = fetched.first().ok_or("created object not found")?;
    println!(
        "server copy identical to what was sent: {}",
        server.data == body
    );
    let e1 = e1.unwrap_or_else(|| server.etag.clone());

    match dav.create(&href, &body).await {
        Err(RemoteError::Conflict) => {
            println!("PUT If-None-Match on an existing href: 412 as expected")
        }
        other => return Err(format!("second create should conflict, got {other:?}")),
    }

    let delta = dav
        .changes(list, Some(&t0))
        .await
        .map_err(|e| e.to_string())?;
    if !delta.changed.iter().any(|(h, e)| *h == href && *e == e1) {
        return Err(format!("sync report misses the new object: {delta:?}"));
    }

    let mut edited = Ical::parse(&server.data);
    task::apply(
        &mut edited,
        &[Edit::Complete(chrono::Utc::now())],
        chrono::Utc::now(),
        local,
    )
    .unwrap();
    let e2 = dav
        .update(&href, &edited.to_string(), &e1)
        .await
        .map_err(|e| format!("update: {e}"))?;
    println!("PUT If-Match: etag {e2:?}");
    let e2 = match e2 {
        Some(e) => e,
        None => dav
            .fetch(list, std::slice::from_ref(&href))
            .await
            .map_err(|e| e.to_string())?[0]
            .etag
            .clone(),
    };

    match dav.update(&href, &body, &e1).await {
        Err(RemoteError::Conflict) => println!("PUT with a stale ETag: 412 as expected"),
        other => return Err(format!("stale update should conflict, got {other:?}")),
    }
    let back = dav
        .fetch(list, std::slice::from_ref(&href))
        .await
        .map_err(|e| e.to_string())?;
    let task = Task::from_ical(&Ical::parse(&back[0].data)).ok_or("unparseable")?;
    if task.status != task::Status::Completed {
        return Err("the stale write went through".into());
    }

    match dav
        .changes(list, Some("http://sabre.io/ns/sync/not-a-token"))
        .await
    {
        Err(RemoteError::InvalidToken) => println!("bad sync token: recognized"),
        other => println!("bad sync token gave {other:?} (fallback path still covers it)"),
    }

    let t1 = dav
        .changes(list, None)
        .await
        .map_err(|e| e.to_string())?
        .token
        .ok_or("no token")?;
    match dav.delete(&href, &e1).await {
        Err(RemoteError::Conflict) => println!("DELETE with a stale ETag: 412 as expected"),
        other => return Err(format!("stale delete should conflict, got {other:?}")),
    }
    dav.delete(&href, &e2)
        .await
        .map_err(|e| format!("delete: {e}"))?;
    let delta = dav
        .changes(list, Some(&t1))
        .await
        .map_err(|e| e.to_string())?;
    if !delta.removed.contains(&href) {
        return Err(format!("sync report misses the deletion: {delta:?}"));
    }
    println!("sync report after DELETE lists it as removed");
    Ok(())
}
