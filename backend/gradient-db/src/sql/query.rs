/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A declared statement and the registry that collects them. `Query::bind` is
//! the one constructor of a raw `Statement` in the backend; `backend/clippy.toml`
//! denies the sea-orm constructors so nothing can reach SQL around the registry.

use sea_orm::{DatabaseBackend, Statement, Value};
use std::borrow::Cow;

use super::{Budget, Param};

#[derive(Copy, Clone, Debug)]
pub enum Sql {
    Static(&'static str),
    Built(fn() -> String),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tier {
    Hot,
    Walk,
    Sweep,
}

impl Tier {
    pub const fn budget(self) -> Budget {
        match self {
            Self::Hot => Budget::HOT,
            Self::Walk => Budget::WALK,
            Self::Sweep => Budget::SWEEP,
        }
    }
}

/// Session state a query needs to plan the way production plans it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Flag {
    /// `SET LOCAL work_mem = '64MB'`, as `gradient_db::graph_sql::begin_walk` sets.
    Walk,
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
}

impl Query {
    pub fn text(&self) -> Cow<'static, str> {
        match self.sql {
            Sql::Static(sql) => Cow::Borrowed(sql),
            Sql::Built(build) => Cow::Owned(build()),
        }
    }

    /// `readiness.rs:348`, the form a failure message can be clicked from.
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

    pub fn stmt(&self) -> Statement {
        self.bind([])
    }
}

inventory::collect!(&'static Query);

pub fn registry() -> impl Iterator<Item = &'static Query> {
    inventory::iter::<&'static Query>.into_iter().copied()
}
