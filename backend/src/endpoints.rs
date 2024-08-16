use actix_web::HttpRequest;
use actix_web::{get, post, HttpResponse, Responder};

#[get("/project/list")]
async fn get_project_list() -> impl Responder {
    HttpResponse::Ok().body("")
}

