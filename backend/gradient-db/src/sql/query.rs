/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "Query::bind and bind_built are the constructors the lint points every other call site at"
)]

use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::exec_job::ExecJobStatus;
use gradient_entity::transition::Transition;
use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement, TryGetable, Value};
use std::borrow::Cow;

use super::{Budget, Param};

#[derive(Copy, Clone, Debug)]
pub enum Sql {
    Static(&'static str),
    Lazy(fn() -> &'static str),
    Built(fn() -> String),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tier {
    Hot,
    Bulk,
    Walk,
    Sweep,
}

impl Tier {
    pub const fn budget(self) -> Budget {
        match self {
            Self::Hot => Budget::HOT,
            Self::Bulk => Budget::BULK,
            Self::Walk => Budget::WALK,
            Self::Sweep => Budget::SWEEP,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Flag {
    Walk,
    DefaultWorkMem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusWrite {
    pub table: &'static str,
    pub to: i32,
}

impl StatusWrite {
    pub const fn build(transition: Transition<BuildStatus>) -> Self {
        Self {
            table: "derivation_build",
            to: transition.to as i32,
        }
    }

    pub const fn evaluation(transition: Transition<EvaluationStatus>) -> Self {
        Self {
            table: "evaluation",
            to: transition.to as i32,
        }
    }

    pub const fn exec_job(to: ExecJobStatus) -> Self {
        Self {
            table: "exec_job",
            to: to as i32,
        }
    }
}

pub struct Query {
    pub name: &'static str,
    pub sql: Sql,
    pub file: &'static str,
    pub line: u32,
    pub params: &'static [Param],
    pub tier: Tier,
    pub budget: Budget,
    pub flags: &'static [Flag],
    pub status_write: Option<StatusWrite>,
}

impl Query {
    pub fn text(&self) -> Cow<'static, str> {
        match self.sql {
            Sql::Static(sql) => Cow::Borrowed(sql),
            Sql::Lazy(borrow) => Cow::Borrowed(borrow()),
            Sql::Built(build) => Cow::Owned(build()),
        }
    }

    pub fn location(&self) -> String {
        let file = self.file.rsplit('/').next().unwrap_or(self.file);
        format!("{file}:{}", self.line)
    }

    pub fn bind<I>(&self, values: I) -> Statement
    where
        I: IntoIterator<Item = Value>,
    {
        let values: Vec<Value> = values.into_iter().collect();
        assert!(
            values.len() == self.params.len(),
            "{} takes {} value(s), got {}",
            self.name,
            self.params.len(),
            values.len(),
        );

        Statement::from_sql_and_values(DatabaseBackend::Postgres, self.text(), values)
    }

    pub fn bind_built<S, I>(&self, sql: S, values: I) -> Statement
    where
        S: Into<String>,
        I: IntoIterator<Item = Value>,
    {
        Statement::from_sql_and_values(DatabaseBackend::Postgres, sql, values)
    }

    pub fn stmt(&self) -> Statement {
        self.bind([])
    }

    pub async fn scalar<C, I, T>(&self, db: &C, values: I, column: &str) -> Result<T, DbErr>
    where
        C: ConnectionTrait,
        I: IntoIterator<Item = Value>,
        T: TryGetable,
    {
        db.query_one_raw(self.bind(values))
            .await?
            .ok_or_else(|| DbErr::Custom(format!("{} returned no row", self.name)))?
            .try_get::<T>("", column)
    }
}

inventory::collect!(&'static Query);

pub fn registry() -> impl Iterator<Item = &'static Query> {
    inventory::iter::<&'static Query>.into_iter().copied()
}
