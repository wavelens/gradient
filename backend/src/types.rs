use uuid::Uuid;
use sea_orm::DatabaseConnection;

pub type ListResponse = Vec<(Uuid, String)>;

#[derive(Clone)]
pub struct AppState {
    pub conn: DatabaseConnection,
}

