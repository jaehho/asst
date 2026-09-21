//! Making, renaming, recoloring and deleting a list, against a real server,
//! on a list the test creates and removes.
//!
//!   ASST_TEST_DAV_URL=https://cloud.example/remote.php/dav/ \
//!   ASST_TEST_USERNAME=me ASST_TEST_PASSWORD=app-password \
//!   cargo test -p asst-core --test lists -- --ignored --nocapture

use asst_core::caldav::{self, Account, CalDav, RemoteList};

fn account() -> Option<Account> {
    Some(Account {
        dav_url: std::env::var("ASST_TEST_DAV_URL").ok()?,
        username: std::env::var("ASST_TEST_USERNAME").ok()?,
        password: std::env::var("ASST_TEST_PASSWORD").ok()?,
    })
}

#[tokio::test]
#[ignore = "needs a CalDAV server"]
async fn lists_against_the_server() {
    let Some(account) = account() else {
        eprintln!("ASST_TEST_* not set; skipping");
        return;
    };
    let dav = CalDav::discover(&account).await.expect("discovery");
    let href = dav
        .create_list(
            &caldav::list_slug("asst list test"),
            "asst list test",
            Some("#8e44ad"),
        )
        .await
        .expect("MKCOL");
    println!("created {href}");
    let result = exercise(&dav, &href).await;
    // Skip the trash bin, so a test run leaves nothing behind.
    dav.delete_list(&href, true)
        .await
        .expect("remove the test list");
    let gone = dav.lists().await.expect("lists");
    assert!(
        !gone.iter().any(|l| l.href == href),
        "the deleted list is still listed"
    );
    result.expect("exercise");
}

async fn find(dav: &CalDav, href: &str) -> Result<RemoteList, String> {
    dav.lists()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|l| l.href == href)
        .ok_or_else(|| format!("{href} is not listed"))
}

fn same_color(got: &Option<String>, want: &str) -> bool {
    got.as_deref()
        .and_then(caldav::parse_color)
        .is_some_and(|c| c == want)
}

async fn exercise(dav: &CalDav, href: &str) -> Result<(), String> {
    let list = find(dav, href).await?;
    println!("listed as {:?}, color {:?}", list.name, list.color);
    if list.name != "asst list test" || !same_color(&list.color, "#8e44ad") {
        return Err(format!("created list reads back as {list:?}"));
    }

    let name = "asst <renamed> & recolored";
    dav.update_list(href, Some(name), Some("#27983a"))
        .await
        .map_err(|e| format!("PROPPATCH name and color: {e}"))?;
    let list = find(dav, href).await?;
    println!("after PROPPATCH: {:?}, color {:?}", list.name, list.color);
    if list.name != name || !same_color(&list.color, "#27983a") {
        return Err(format!("rename and recolor read back as {list:?}"));
    }

    dav.update_list(href, None, Some("#1492b2"))
        .await
        .map_err(|e| format!("PROPPATCH color: {e}"))?;
    let list = find(dav, href).await?;
    if list.name != name || !same_color(&list.color, "#1492b2") {
        return Err(format!("a color-only change read back as {list:?}"));
    }
    Ok(())
}
