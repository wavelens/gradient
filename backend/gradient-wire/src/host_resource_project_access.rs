/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::fmt;

use bytes::{Bytes, BytesMut};
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::codec::{DecodeError, EncodeError, Proto};

const WORKER_RESOURCE_ALL_PROJECTS_KEYWORD: &str = "all";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerResourceProjectAccess {
    EveryProject,
    OnlyProjects(BTreeSet<Uuid>),
}

impl WorkerResourceProjectAccess {
    pub fn contains(&self, id: &Uuid) -> bool {
        match self {
            Self::EveryProject => true,
            Self::OnlyProjects(projects) => projects.contains(id),
        }
    }

    pub fn intersect_released_projects(&self, other_project_access: &Self) -> Self {
        match (self, other_project_access) {
            (Self::EveryProject, kept_project_access)
            | (kept_project_access, Self::EveryProject) => kept_project_access.clone(),
            (
                Self::OnlyProjects(first_only_projects_project_uuids),
                Self::OnlyProjects(second_only_projects_project_uuids),
            ) => Self::OnlyProjects(
                first_only_projects_project_uuids
                    .intersection(second_only_projects_project_uuids)
                    .copied()
                    .collect(),
            ),
        }
    }

    fn project_access_to_proto(&self) -> ProjectAccessAsOptionalProjectUuidHalves {
        match self {
            Self::EveryProject => None,
            Self::OnlyProjects(projects) => Some(projects.iter().map(Uuid::as_u64_pair).collect()),
        }
    }
}

type ProjectAccessAsOptionalProjectUuidHalves = Option<Vec<(u64, u64)>>;

impl Proto for WorkerResourceProjectAccess {
    fn encode(&self, version: u16, out: &mut BytesMut) -> Result<(), EncodeError> {
        self.project_access_to_proto().encode(version, out)
    }

    fn decode(input: &mut Bytes, version: u16) -> Result<Self, DecodeError> {
        Ok(
            match ProjectAccessAsOptionalProjectUuidHalves::decode(input, version)? {
                None => Self::EveryProject,
                Some(decoded_project_uuid_pairs) => Self::OnlyProjects(
                    decoded_project_uuid_pairs
                        .into_iter()
                        .map(|(project_uuid_high_bits, project_uuid_low_bits)| {
                            Uuid::from_u64_pair(project_uuid_high_bits, project_uuid_low_bits)
                        })
                        .collect(),
                ),
            },
        )
    }

    fn describe(version: u16, out: &mut String) {
        ProjectAccessAsOptionalProjectUuidHalves::describe(version, out);
    }
}

impl Serialize for WorkerResourceProjectAccess {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::EveryProject => serializer.serialize_str(WORKER_RESOURCE_ALL_PROJECTS_KEYWORD),
            Self::OnlyProjects(projects) => projects.serialize(serializer),
        }
    }
}

struct ProjectReleaseFromAllOrUuidListVisitor;

impl<'de> Visitor<'de> for ProjectReleaseFromAllOrUuidListVisitor {
    type Value = WorkerResourceProjectAccess;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("string `\"all\"` or a list of project UUIDs")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        match value {
            WORKER_RESOURCE_ALL_PROJECTS_KEYWORD => Ok(WorkerResourceProjectAccess::EveryProject),
            _ => Err(E::custom(format!(
                "Unknown projects value {value:?}. Set \"all\" or a list of project UUIDs."
            ))),
        }
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut projects = BTreeSet::new();
        while let Some(project) = seq.next_element::<Uuid>()? {
            projects.insert(project);
        }
        Ok(WorkerResourceProjectAccess::OnlyProjects(projects))
    }
}

impl<'de> Deserialize<'de> for WorkerResourceProjectAccess {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ProjectReleaseFromAllOrUuidListVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_release_type_json_parses_all_or_uuid_list_and_serializes_same_json() {
        let release_parsed_from_json_all: WorkerResourceProjectAccess =
            serde_json::from_str(r#""all""#).unwrap();
        assert_eq!(
            release_parsed_from_json_all,
            WorkerResourceProjectAccess::EveryProject
        );
        assert_eq!(
            serde_json::to_string(&release_parsed_from_json_all).unwrap(),
            r#""all""#
        );

        let listed = Uuid::now_v7();
        let json_text_of_list_with_one_project_uuid = serde_json::to_string(&[listed]).unwrap();
        let release_parsed_from_one_uuid_json_list: WorkerResourceProjectAccess =
            serde_json::from_str(&json_text_of_list_with_one_project_uuid).unwrap();
        assert_eq!(
            release_parsed_from_one_uuid_json_list,
            WorkerResourceProjectAccess::OnlyProjects(BTreeSet::from([listed]))
        );
        assert_eq!(
            serde_json::to_string(&release_parsed_from_one_uuid_json_list).unwrap(),
            json_text_of_list_with_one_project_uuid
        );
    }

    #[test]
    fn test_rejects_project_release_type_string_other_than_all_non_uuid_list_entry_and_null() {
        for invalid_projects_json_text in [r#""every""#, r#"["not-a-project"]"#, "null", "{}"] {
            assert!(
                serde_json::from_str::<WorkerResourceProjectAccess>(invalid_projects_json_text)
                    .is_err(),
                "{invalid_projects_json_text}"
            );
        }
    }

    #[test]
    fn test_project_release_type_contains_project() {
        let listed = Uuid::now_v7();
        let unlisted_project_uuid = Uuid::now_v7();
        let release_parsed_from_one_uuid_json_list =
            WorkerResourceProjectAccess::OnlyProjects(BTreeSet::from([listed]));
        assert!(release_parsed_from_one_uuid_json_list.contains(&listed));
        assert!(!release_parsed_from_one_uuid_json_list.contains(&unlisted_project_uuid));
        assert!(!WorkerResourceProjectAccess::OnlyProjects(BTreeSet::new()).contains(&listed));
        assert!(WorkerResourceProjectAccess::EveryProject.contains(&unlisted_project_uuid));
    }
}
