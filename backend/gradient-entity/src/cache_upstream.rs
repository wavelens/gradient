/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::organization_cache::CacheSubscriptionMode;
use crate::ids::{CacheId, CacheUpstreamId};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Default, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum CacheUpstreamKind {
    #[sea_orm(num_value = 0)]
    Internal,
    #[sea_orm(num_value = 1)]
    GradientProto,
    #[default]
    #[sea_orm(num_value = 2)]
    Http,
}

/// An upstream cache entry attached to a Gradient cache. Discriminated by `kind`.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cache_upstream")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: CacheUpstreamId,
    pub cache: CacheId,
    pub display_name: String,
    pub mode: CacheSubscriptionMode,
    pub kind: CacheUpstreamKind,
    pub upstream_cache: Option<CacheId>,
    pub url: Option<String>,
    pub public_key: Option<String>,
    pub remote_cache_name: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Cache,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Cache => Entity::belongs_to(super::cache::Entity)
                .from(Column::Cache)
                .to(super::cache::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}

#[derive(Debug, Clone, PartialEq)]
pub enum CacheUpstreamSource<'a> {
    Internal {
        cache_id: CacheId,
    },
    GradientProto {
        url: &'a str,
        remote_cache: &'a str,
        public_key: Option<&'a str>,
    },
    Http {
        url: &'a str,
        public_key: &'a str,
    },
}

impl Model {
    pub fn as_source(&self) -> Option<CacheUpstreamSource<'_>> {
        match self.kind {
            CacheUpstreamKind::Internal => self
                .upstream_cache
                .map(|cache_id| CacheUpstreamSource::Internal { cache_id }),
            CacheUpstreamKind::GradientProto => Some(CacheUpstreamSource::GradientProto {
                url: self.url.as_deref()?,
                remote_cache: self.remote_cache_name.as_deref()?,
                public_key: self.public_key.as_deref(),
            }),
            CacheUpstreamKind::Http => Some(CacheUpstreamSource::Http {
                url: self.url.as_deref()?,
                public_key: self.public_key.as_deref()?,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{CacheId, CacheUpstreamId};

    fn base() -> Model {
        Model {
            id: CacheUpstreamId::nil(),
            cache: CacheId::nil(),
            display_name: "x".into(),
            mode: CacheSubscriptionMode::ReadOnly,
            kind: CacheUpstreamKind::Http,
            upstream_cache: None,
            url: None,
            public_key: None,
            remote_cache_name: None,
            api_key: None,
        }
    }

    #[test]
    fn as_source_internal() {
        let id = CacheId::now_v7();
        let m = Model {
            kind: CacheUpstreamKind::Internal,
            upstream_cache: Some(id),
            ..base()
        };
        assert_eq!(
            m.as_source(),
            Some(CacheUpstreamSource::Internal { cache_id: id })
        );
    }

    #[test]
    fn as_source_gradient_proto() {
        let m = Model {
            kind: CacheUpstreamKind::GradientProto,
            url: Some("https://remote.example".into()),
            remote_cache_name: Some("prod".into()),
            public_key: Some("k:abc".into()),
            ..base()
        };
        assert_eq!(
            m.as_source(),
            Some(CacheUpstreamSource::GradientProto {
                url: "https://remote.example",
                remote_cache: "prod",
                public_key: Some("k:abc"),
            })
        );
    }

    #[test]
    fn as_source_http() {
        let m = Model {
            kind: CacheUpstreamKind::Http,
            url: Some("https://cache.nixos.org".into()),
            public_key: Some("cache.nixos.org-1:abc".into()),
            ..base()
        };
        assert_eq!(
            m.as_source(),
            Some(CacheUpstreamSource::Http {
                url: "https://cache.nixos.org",
                public_key: "cache.nixos.org-1:abc",
            })
        );
    }

    #[test]
    fn as_source_inconsistent_returns_none() {
        let m = Model {
            kind: CacheUpstreamKind::Http,
            url: None,
            public_key: None,
            ..base()
        };
        assert!(m.as_source().is_none());
    }
}
