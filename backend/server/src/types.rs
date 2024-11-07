use uuid::Uuid;
use sea_orm::DatabaseConnection;
use nix_daemon::nix::DaemonStore;
use async_ssh2_lite::{ AsyncChannel, TokioTcpStream };
use std::sync::Arc;
use entity::*;
use chrono::{DateTime, NaiveDateTime};
use std::sync::LazyLock;

pub type ListResponse = Vec<(Uuid, String)>;
pub type NixStore = DaemonStore<AsyncChannel<TokioTcpStream>>;
pub type DBConn = Arc<DatabaseConnection>;

pub const NULL_TIME: LazyLock<NaiveDateTime> = LazyLock::new(|| {
    DateTime::from_timestamp(0, 0).unwrap().naive_utc()
});

#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<DatabaseConnection>,
}

pub type EBuild = build::Entity;
pub type EOrganization = organization::Entity;
pub type EProject = project::Entity;
pub type EServer = server::Entity;
pub type EUser = user::Entity;

pub type MBuild = build::Model;
pub type MOrganization = organization::Model;
pub type MProject = project::Model;
pub type MServer = server::Model;
pub type MUser = user::Model;

pub type ABuild = build::ActiveModel;
pub type AOrganization = organization::ActiveModel;
pub type AProject = project::ActiveModel;
pub type AServer = server::ActiveModel;
pub type AUser = user::ActiveModel;

pub type CBuild = build::Column;
pub type COrganization = organization::Column;
pub type CProject = project::Column;
pub type CServer = server::Column;
pub type CUser = user::Column;
