pub use sea_orm_migration::prelude::*;

mod m20241107_135027_create_table_user;
mod m20241107_135442_create_table_organization;
mod m20241107_135941_create_table_project;
mod m20241107_140336_create_table_build;
mod m20241107_140550_create_table_server;
mod m20241107_140600_create_table_evaluation;
mod m20241107_155000_create_table_build_depencdency;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20241107_135027_create_table_user::Migration),
            Box::new(m20241107_135442_create_table_organization::Migration),
            Box::new(m20241107_135941_create_table_project::Migration),
            Box::new(m20241107_140600_create_table_evaluation::Migration),
            Box::new(m20241107_140336_create_table_build::Migration),
            Box::new(m20241107_140550_create_table_server::Migration),
            Box::new(m20241107_155000_create_table_build_depencdency::Migration),
        ]
    }
}
