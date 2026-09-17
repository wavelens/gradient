/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `sql!` declares a statement and registers it with the plan gate. A statement
//! that is assembled at run time uses `sql_fn!`, which registers the builder so
//! the gate plans generated SQL rather than a copy of it.

/// Declares a statement, its parameter kinds and its budget, and registers it.
#[macro_export]
macro_rules! sql {
    ($(
        $(#[$meta:meta])*
        $vis:vis $name:ident = $sql:expr,
        params = [$($param:ident $(($($arg:expr),+))?),* $(,)?]
        $(, tier = $tier:ident)?
        $(, budget = $budget:expr)?
        $(, flags = [$($flag:ident),* $(,)?])?
        $(,)? ;
    )+) => {$(
        $(#[$meta])*
        $vis static $name: $crate::sql::Query = $crate::sql::Query {
            name: stringify!($name),
            sql: $crate::sql::Sql::Static($sql),
            file: file!(),
            line: line!(),
            params: &[$($crate::sql::Param::$param $(($($arg),+))?),*],
            tier: $crate::sql_tier!($($tier)?),
            budget: $crate::sql_budget!($($budget)? ; $($tier)?),
            flags: &[$($($crate::sql::Flag::$flag),*)?],
        };

        $crate::sql::inventory::submit! { &$name }
    )+};
}

/// The `sql!` of a statement built at run time. The closure is the exemplar the
/// gate plans, so a fence is checked in generated SQL and not in a copy of it.
#[macro_export]
macro_rules! sql_fn {
    ($(
        $(#[$meta:meta])*
        $vis:vis $name:ident = $builder:expr,
        params = [$($param:ident $(($($arg:expr),+))?),* $(,)?]
        $(, tier = $tier:ident)?
        $(, budget = $budget:expr)?
        $(, flags = [$($flag:ident),* $(,)?])?
        $(,)? ;
    )+) => {$(
        $(#[$meta])*
        $vis static $name: $crate::sql::Query = $crate::sql::Query {
            name: stringify!($name),
            sql: $crate::sql::Sql::Built($builder),
            file: file!(),
            line: line!(),
            params: &[$($crate::sql::Param::$param $(($($arg),+))?),*],
            tier: $crate::sql_tier!($($tier)?),
            budget: $crate::sql_budget!($($budget)? ; $($tier)?),
            flags: &[$($($crate::sql::Flag::$flag),*)?],
        };

        $crate::sql::inventory::submit! { &$name }
    )+};
}

/// The `sql!` of a statement held in a `LazyLock<String>`. The closure borrows
/// it, so nothing is rebuilt or cloned on the path that runs it.
#[macro_export]
macro_rules! sql_lazy {
    ($(
        $(#[$meta:meta])*
        $vis:vis $name:ident = $borrow:expr,
        params = [$($param:ident $(($($arg:expr),+))?),* $(,)?]
        $(, tier = $tier:ident)?
        $(, budget = $budget:expr)?
        $(, flags = [$($flag:ident),* $(,)?])?
        $(,)? ;
    )+) => {$(
        $(#[$meta])*
        $vis static $name: $crate::sql::Query = $crate::sql::Query {
            name: stringify!($name),
            sql: $crate::sql::Sql::Lazy($borrow),
            file: file!(),
            line: line!(),
            params: &[$($crate::sql::Param::$param $(($($arg),+))?),*],
            tier: $crate::sql_tier!($($tier)?),
            budget: $crate::sql_budget!($($budget)? ; $($tier)?),
            flags: &[$($($crate::sql::Flag::$flag),*)?],
        };

        $crate::sql::inventory::submit! { &$name }
    )+};
}

#[doc(hidden)]
#[macro_export]
macro_rules! sql_tier {
    () => {
        $crate::sql::Tier::Hot
    };
    ($tier:ident) => {
        $crate::sql::Tier::$tier
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! sql_budget {
    (; $($tier:ident)?) => {
        $crate::sql_tier!($($tier)?).budget()
    };
    ($budget:expr ; $($tier:ident)?) => {
        $budget
    };
}

#[cfg(test)]
mod tests {
    use crate::sql::{Budget, Param, Tier, registry};

    crate::sql! {
        /// Fixture query, registered like any other.
        pub TEST_LOOKUP = "SELECT 1 FROM derivation WHERE id = ANY($1::uuid[])",
            params = [DerivationIds(8)];
    }

    static TEST_LAZY_SQL: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| "SELECT 2 FROM derivation".to_string());

    crate::sql_lazy! {
        pub TEST_LAZY = || TEST_LAZY_SQL.as_str(),
            params = [];
    }

    crate::sql_fn! {
        pub TEST_BUILT = || format!("SELECT {} FROM derivation", 1),
            params = [],
            tier = Sweep;
    }

    #[test]
    fn macro_fills_every_field() {
        assert_eq!(TEST_LOOKUP.name, "TEST_LOOKUP");
        assert_eq!(TEST_LOOKUP.params, &[Param::DerivationIds(8)]);
        assert_eq!(TEST_LOOKUP.tier, Tier::Hot);
        assert_eq!(TEST_LOOKUP.budget.buffers, Budget::HOT.buffers);
        assert!(TEST_LOOKUP.file.ends_with("macros.rs"));
        assert!(TEST_LOOKUP.line > 0);
    }

    #[test]
    fn built_query_renders_and_takes_its_tier() {
        assert_eq!(TEST_BUILT.text(), "SELECT 1 FROM derivation");
        assert_eq!(TEST_BUILT.tier, Tier::Sweep);
        assert_eq!(TEST_BUILT.budget.buffers, Budget::SWEEP.buffers);
    }

    #[test]
    fn a_lazy_statement_is_borrowed_not_rebuilt() {
        assert_eq!(TEST_LAZY.text(), "SELECT 2 FROM derivation");
        assert!(matches!(TEST_LAZY.text(), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn registry_carries_both() {
        let names: Vec<_> = registry().map(|q| q.name).collect();
        assert!(names.contains(&"TEST_LOOKUP"), "{names:?}");
        assert!(names.contains(&"TEST_BUILT"), "{names:?}");
    }

    #[test]
    fn bind_builds_a_statement_with_the_sql() {
        let stmt = TEST_LOOKUP.bind([sea_orm::Value::from(Vec::<uuid::Uuid>::new())]);
        assert!(stmt.sql.contains("FROM derivation"));
    }

    #[test]
    #[should_panic(expected = "TEST_LOOKUP takes 1 value")]
    fn bind_rejects_the_wrong_value_count() {
        let _ = TEST_LOOKUP.bind([]);
    }
}
