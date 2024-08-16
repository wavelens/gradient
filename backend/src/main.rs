mod endpoints;

use actix_web::{App, HttpServer};
use diesel::SqliteConnection;

type DbPool = r2d2::Pool<r2d2::ConnectionManager<SqliteConnection>>;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let manager = r2d2::ConnectionManager::<SqliteConnection>::new("gradient.db");
    let pool = r2d2::Pool::builder()
        .build(manager)
        .expect("database URL should be valid path to SQLite DB file");

    HttpServer::new(|| {
        App::new()
            .service(endpoints::get_project_list)
    })
    .bind(("127.0.0.1", 3000))?
    .run()
    .await
}

