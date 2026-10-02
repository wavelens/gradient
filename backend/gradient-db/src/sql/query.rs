/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![expect(
    clippy::disallowed_methods,
    reason = "Query::bind and bind_built are the constructors the lint points every other call site at"
)]

use sea_orm::{DatabaseBackend, Statement, Value};
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
}

inventory::collect!(&'static Query);

pub fn registry() -> impl Iterator<Item = &'static Query> {
    inventory::iter::<&'static Query>.into_iter().copied()
}
