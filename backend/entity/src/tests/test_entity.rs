/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// #[cfg(test)]
// mod tests {
//     use sea_orm::{
//         entity::prelude::*, entity::*, tests_cfg::*,
//         DatabaseBackend, MockDatabase, Transaction,
//     };

//     #[async_std::test]
//     async fn test_find_cake() -> Result<(), DbErr> {
//         let db = MockDatabase::new(DatabaseBackend::Postgres)
//             .append_query_results([
//                 vec![cake::Model {
//                     id: 1,
//                     name: "New York Cheese".to_owned(),
//                 }],
//                 vec![
//                     cake::Model {
//                         id: 1,
//                         name: "New York Cheese".to_owned(),
//                     },
//                     cake::Model {
//                         id: 2,
//                         name: "Chocolate Forest".to_owned(),
//                     },
//                 ],
//             ])
//             .append_query_results([
//                 [(
//                     cake::Model {
//                         id: 1,
//                         name: "Apple Cake".to_owned(),
//                     },
//                     fruit::Model {
//                         id: 2,
//                         name: "Apple".to_owned(),
//                         cake_id: Some(1),
//                     },
//                 )],
//             ])
//             .into_connection();

//         assert_eq!(
//             cake::Entity::find().one(&db).await?,
//             Some(cake::Model {
//                 id: 1,
//                 name: "New York Cheese".to_owned(),
//             })
//         );

//         // Find all cakes from MockDatabase
//         // Return the second query result
//         assert_eq!(
//             cake::Entity::find().all(&db).await?,
//             [
//                 cake::Model {
//                     id: 1,
//                     name: "New York Cheese".to_owned(),
//                 },
//                 cake::Model {
//                     id: 2,
//                     name: "Chocolate Forest".to_owned(),
//                 },
//             ]
//         );

//         // Find all cakes with its related fruits
//         assert_eq!(
//             cake::Entity::find()
//                 .find_also_related(fruit::Entity)
//                 .all(&db)
//                 .await?,
//             [(
//                 cake::Model {
//                     id: 1,
//                     name: "Apple Cake".to_owned(),
//                 },
//                 Some(fruit::Model {
//                     id: 2,
//                     name: "Apple".to_owned(),
//                     cake_id: Some(1),
//                 })
//             )]
//         );

//         // Checking transaction log
//         assert_eq!(
//             db.into_transaction_log(),
//             [
//                 Transaction::from_sql_and_values(
//                     DatabaseBackend::Postgres,
//                     r#"SELECT "cake"."id", "cake"."name" FROM "cake" LIMIT $1"#,
//                     [1u64.into()]
//                 ),
//                 Transaction::from_sql_and_values(
//                     DatabaseBackend::Postgres,
//                     r#"SELECT "cake"."id", "cake"."name" FROM "cake""#,
//                     []
//                 ),
//                 Transaction::from_sql_and_values(
//                     DatabaseBackend::Postgres,
//                     r#"SELECT "cake"."id" AS "A_id", "cake"."name" AS "A_name", "fruit"."id" AS "B_id", "fruit"."name" AS "B_name", "fruit"."cake_id" AS "B_cake_id" FROM "cake" LEFT JOIN "fruit" ON "cake"."id" = "fruit"."cake_id""#,
//                     []
//                 ),
//             ]
//         );

//         Ok(())
//     }
// }
