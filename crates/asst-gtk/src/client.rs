//! Calls to asstd. They run on relm4's tokio runtime; results come back to
//! the GTK thread as command outputs.

use asst_core::api::{
    AddSpec, Added, AsstProxy, Change, ListChange, ListSpec, ListView, Settings,
    SettingsChange, StatusView, TaskView,
};
use asst_core::store::Query;
use serde::de::DeserializeOwned;
use tokio::sync::OnceCell;

static PROXY: OnceCell<AsstProxy<'static>> = OnceCell::const_new();

pub type Result<T> = std::result::Result<T, String>;

pub async fn proxy() -> Result<AsstProxy<'static>> {
    PROXY
        .get_or_try_init(|| async {
            let conn = zbus::Connection::session().await?;
            AsstProxy::new(&conn).await
        })
        .await
        .cloned()
        .map_err(|e| describe(&e))
}

/// A D-Bus error as a sentence: the daemon's own message when it sent one.
pub fn describe(e: &zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
        {
            "asstd is not running (systemctl --user start asstd)".into()
        }
        zbus::Error::MethodError(_, Some(text), _) => text.clone(),
        zbus::Error::FDO(fdo) => match fdo.as_ref() {
            zbus::fdo::Error::ServiceUnknown(_) => {
                "asstd is not running (systemctl --user start asstd)".into()
            }
            zbus::fdo::Error::Failed(m) | zbus::fdo::Error::InvalidArgs(m) => m.clone(),
            other => other.to_string(),
        },
        other => other.to_string(),
    }
}

fn json<T: DeserializeOwned>(r: zbus::Result<String>) -> Result<T> {
    let raw = r.map_err(|e| describe(&e))?;
    serde_json::from_str(&raw).map_err(|e| format!("unexpected answer from asstd: {e}"))
}

pub async fn status() -> Result<StatusView> {
    json(proxy().await?.status().await)
}

pub async fn lists() -> Result<Vec<ListView>> {
    json(proxy().await?.lists().await)
}

pub async fn tasks(q: &Query) -> Result<Vec<TaskView>> {
    json(
        proxy()
            .await?
            .tasks(&serde_json::to_string(q).expect("query serializes"))
            .await,
    )
}

pub async fn add(spec: &AddSpec) -> Result<Added> {
    json(
        proxy()
            .await?
            .add(&serde_json::to_string(spec).expect("spec serializes"))
            .await,
    )
}

pub async fn edit(id: &str, change: &Change) -> Result<TaskView> {
    json(
        proxy()
            .await?
            .edit(
                id,
                &serde_json::to_string(change).expect("change serializes"),
            )
            .await,
    )
}

pub async fn complete(id: &str) -> Result<TaskView> {
    json(proxy().await?.complete(id).await)
}

pub async fn reopen(id: &str) -> Result<TaskView> {
    json(proxy().await?.reopen(id).await)
}

pub async fn delete(id: &str) -> Result<()> {
    proxy().await?.delete(id).await.map_err(|e| describe(&e))
}

pub async fn sync() -> Result<()> {
    proxy()
        .await?
        .sync()
        .await
        .map(|_| ())
        .map_err(|e| describe(&e))
}

pub async fn get(id: &str) -> Result<TaskView> {
    json(proxy().await?.get(id).await)
}

pub async fn duplicate(id: &str) -> Result<TaskView> {
    json(proxy().await?.duplicate(id).await)
}

pub async fn create_list(spec: &ListSpec) -> Result<ListView> {
    json(
        proxy()
            .await?
            .create_list(&serde_json::to_string(spec).expect("spec serializes"))
            .await,
    )
}

pub async fn update_list(href: &str, change: &ListChange) -> Result<ListView> {
    json(
        proxy()
            .await?
            .update_list(
                href,
                &serde_json::to_string(change).expect("change serializes"),
            )
            .await,
    )
}

/// A list's completed tasks, or every list's → how many went.
pub async fn delete_completed(list: Option<&str>) -> Result<u32> {
    proxy()
        .await?
        .delete_completed(list.unwrap_or(""))
        .await
        .map_err(|e| describe(&e))
}

/// A task's iCalendar object, as asst would send it.
pub async fn ics(id: &str) -> Result<String> {
    proxy().await?.ics(id).await.map_err(|e| describe(&e))
}

pub async fn delete_list(href: &str) -> Result<()> {
    proxy()
        .await?
        .delete_list(href)
        .await
        .map_err(|e| describe(&e))
}

pub async fn settings() -> Result<Settings> {
    json(proxy().await?.settings().await)
}

pub async fn set_settings(change: &SettingsChange) -> Result<Settings> {
    json(
        proxy()
            .await?
            .set_settings(&serde_json::to_string(change).expect("change serializes"))
            .await,
    )
}

pub async fn login(server: &str) -> Result<String> {
    proxy().await?.login(server).await.map_err(|e| describe(&e))
}

pub async fn logout() -> Result<()> {
    proxy().await?.logout().await.map_err(|e| describe(&e))
}

/// Run a daemon call from GTK code outside a component: on relm4's tokio
/// runtime (zbus needs it), answered back on the main loop.
pub fn call<T: Send + 'static>(
    fut: impl std::future::Future<Output = T> + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    let handle = relm4::spawn(fut);
    relm4::gtk::glib::spawn_future_local(async move {
        if let Ok(v) = handle.await {
            done(v);
        }
    });
}
