// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

//! (De)Serialization of the format used by `nix --log-format internal-json` output.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::{StderrField, StderrResultType};

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Action {
    Start(ActionStart),
    Stop(ActionStop),
    #[serde(rename = "msg")]
    Message(ActionMessage),
    Result(#[serde_as(as = "serde_with::TryFromInto<ActionResultRaw>")] ActionResult),
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionStart {
    pub id: u64,
    #[serde_as(as = "serde_with::TryFromInto<u64>")]
    pub level: crate::Verbosity,
    pub parent: u64,
    pub text: String,
    #[serde(rename = "type")]
    #[serde_as(as = "serde_with::TryFromInto<u64>")]
    pub type_: crate::StderrActivityType,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionStop {
    pub id: u64,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionMessage {
    pub file: Option<String>,
    pub column: Option<u64>,
    pub line: Option<u64>,
    #[serde_as(as = "serde_with::TryFromInto<u64>")]
    pub level: crate::Verbosity,
    pub msg: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ResultFieldError {
    #[error("field has the wrong type: {0}")]
    FieldType(crate::StderrField),
    #[error("field missing")]
    FieldMissing,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResultRaw {
    pub id: u64,
    #[serde(rename = "type")]
    #[serde_as(as = "serde_with::TryFromInto<u64>")]
    pub type_: StderrResultType,
    pub fields: VecDeque<crate::StderrField>,
}
impl ActionResultRaw {
    pub fn take<T: TryFrom<crate::StderrField, Error = crate::StderrField>>(
        &mut self,
    ) -> Result<T, ResultFieldError> {
        self.fields
            .pop_front()
            .ok_or(ResultFieldError::FieldMissing)?
            .try_into()
            .map_err(ResultFieldError::FieldType)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionResult {
    pub id: u64,
    pub data: ActionResultData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionResultData {
    FileLinked(FileLinked),
    BuildLogLine(BuildLogLine),
    UntrustedPath,
    CorruptedPath,
    SetPhase(SetPhase),
    Progress(Progress),
    SetExpected(SetExpected),
    PostBuildLogLine(BuildLogLine),
}

impl TryFrom<ActionResultRaw> for ActionResult {
    type Error = ResultFieldError;
    fn try_from(raw: ActionResultRaw) -> Result<Self, Self::Error> {
        fn data<T>(raw: ActionResultRaw) -> Result<T, ResultFieldError>
        where
            Result<T, ResultFieldError>: FromIterator<StderrField>,
        {
            raw.fields.into_iter().collect()
        }
        type SRT = StderrResultType;
        type ARD = ActionResultData;
        Ok(ActionResult {
            id: raw.id,
            data: match raw.type_ {
                SRT::FileLinked => ARD::FileLinked(data(raw)?),
                SRT::BuildLogLine => ARD::BuildLogLine(data(raw)?),
                SRT::UntrustedPath => ARD::UntrustedPath,
                SRT::CorruptedPath => ARD::CorruptedPath,
                SRT::SetPhase => ARD::SetPhase(data(raw)?),
                SRT::Progress => ARD::Progress(data(raw)?),
                SRT::SetExpected => ARD::SetExpected(data(raw)?),
                SRT::PostBuildLogLine => ARD::PostBuildLogLine(data(raw)?),
            },
        })
    }
}
impl From<ActionResult> for ActionResultRaw {
    fn from(result: ActionResult) -> ActionResultRaw {
        type SRT = StderrResultType;
        type ARD = ActionResultData;
        let (type_, fields) = match result.data {
            ARD::FileLinked(v) => (SRT::FileLinked, v.into_iter().collect()),
            ARD::BuildLogLine(v) => (SRT::BuildLogLine, v.into_iter().collect()),
            ARD::UntrustedPath => (SRT::UntrustedPath, VecDeque::new()),
            ARD::CorruptedPath => (SRT::CorruptedPath, VecDeque::new()),
            ARD::SetPhase(v) => (SRT::SetPhase, v.into_iter().collect()),
            ARD::Progress(v) => (SRT::Progress, v.into_iter().collect()),
            ARD::SetExpected(v) => (SRT::SetExpected, v.into_iter().collect()),
            ARD::PostBuildLogLine(v) => (SRT::PostBuildLogLine, v.into_iter().collect()),
        };
        ActionResultRaw {
            id: result.id,
            type_,
            fields,
        }
    }
}

fn take_field<T: TryFrom<StderrField, Error = StderrField>, I: Iterator<Item = StderrField>>(
    mut iter: I,
) -> Result<T, ResultFieldError> {
    iter.next()
        .ok_or_else(|| ResultFieldError::FieldMissing)?
        .try_into()
        .map_err(ResultFieldError::FieldType)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLinked {
    pub bytes_linked: u64,
}
impl FromIterator<StderrField> for Result<FileLinked, ResultFieldError> {
    fn from_iter<T: IntoIterator<Item = StderrField>>(iter: T) -> Self {
        Ok(FileLinked {
            bytes_linked: take_field(iter.into_iter())?,
        })
    }
}
impl IntoIterator for FileLinked {
    type Item = StderrField;
    type IntoIter = <[StderrField; 1] as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        [self.bytes_linked.into()].into_iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildLogLine {
    pub line: String,
}
impl FromIterator<StderrField> for Result<BuildLogLine, ResultFieldError> {
    fn from_iter<T: IntoIterator<Item = StderrField>>(iter: T) -> Self {
        Ok(BuildLogLine {
            line: take_field(iter.into_iter())?,
        })
    }
}
impl IntoIterator for BuildLogLine {
    type Item = StderrField;
    type IntoIter = <[StderrField; 1] as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        [self.line.into()].into_iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetPhase {
    pub phase: String,
}
impl FromIterator<StderrField> for Result<SetPhase, ResultFieldError> {
    fn from_iter<T: IntoIterator<Item = StderrField>>(iter: T) -> Self {
        Ok(SetPhase {
            phase: take_field(iter.into_iter())?,
        })
    }
}
impl IntoIterator for SetPhase {
    type Item = StderrField;
    type IntoIter = <[StderrField; 1] as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        [self.phase.into()].into_iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub expected: u64,
    pub running: u64,
    pub failed: u64,
}
impl FromIterator<StderrField> for Result<Progress, ResultFieldError> {
    fn from_iter<T: IntoIterator<Item = StderrField>>(iter: T) -> Self {
        let mut iter = iter.into_iter();
        Ok(Progress {
            done: take_field(&mut iter)?,
            expected: take_field(&mut iter)?,
            running: take_field(&mut iter)?,
            failed: take_field(&mut iter)?,
        })
    }
}
impl IntoIterator for Progress {
    type Item = StderrField;
    type IntoIter = <[StderrField; 4] as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        [
            self.done.into(),
            self.expected.into(),
            self.running.into(),
            self.failed.into(),
        ]
        .into_iter()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetExpected {
    pub activity_type: crate::StderrActivityType,
    pub expected: u64,
}
impl FromIterator<StderrField> for Result<SetExpected, ResultFieldError> {
    fn from_iter<T: IntoIterator<Item = StderrField>>(iter: T) -> Self {
        let mut iter = iter.into_iter();
        Ok(SetExpected {
            activity_type: take_field(&mut iter)?,
            expected: take_field(&mut iter)?,
        })
    }
}
impl IntoIterator for SetExpected {
    type Item = StderrField;
    type IntoIter = <[StderrField; 2] as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter {
        [self.activity_type.into(), self.expected.into()].into_iter()
    }
}
