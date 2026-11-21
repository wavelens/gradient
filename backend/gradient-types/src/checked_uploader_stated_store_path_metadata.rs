/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::str::FromStr;

use gradient_entity::{OriginalStorePath, StorePathMapper};
use harmonia_store_content_address::ContentAddress;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UploaderStatedFieldError {
    #[error("Reference {0:?} isn't a path in the Nix store")]
    StatedReferenceIsNotStorePath(String),
    #[error("Deriver {0:?} isn't a store path with a .drv suffix")]
    StatedDeriverIsNotDrvStorePath(String),
    #[error("Content address {0:?} is not a Nix content address")]
    StatedContentAddressIsNotNixContentAddress(String),
}

/// Checks on each value happen during construction. Unchecked text from the uploader can never reach the database or narinfo file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedUploadedStorePathReferencesDeriverAndContentAddress {
    references: Vec<String>,
    deriver: Option<String>,
    content_address: Option<String>,
}

impl CheckedUploadedStorePathReferencesDeriverAndContentAddress {
    pub fn checked_from_references_deriver_and_content_address(
        references: Vec<String>,
        deriver: Option<String>,
        content_address: Option<String>,
    ) -> Result<Self, UploaderStatedFieldError> {
        for reference in &references {
            if OriginalStorePath::parse(reference).is_err() {
                return Err(UploaderStatedFieldError::StatedReferenceIsNotStorePath(
                    reference.clone(),
                ));
            }
        }
        let deriver = deriver.filter(|deriver| !matches!(deriver.as_str(), "" | "unknown-deriver"));
        if let Some(deriver) = &deriver {
            let is_drv_store_path =
                OriginalStorePath::parse(deriver).is_ok_and(|path| path.name().ends_with(".drv"));
            if !is_drv_store_path {
                return Err(UploaderStatedFieldError::StatedDeriverIsNotDrvStorePath(
                    deriver.clone(),
                ));
            }
        }
        let content_address = content_address.filter(|ca| !ca.is_empty());
        if let Some(ca) = &content_address
            && ContentAddress::from_str(ca).is_err()
        {
            return Err(
                UploaderStatedFieldError::StatedContentAddressIsNotNixContentAddress(ca.clone()),
            );
        }
        Ok(Self {
            references,
            deriver,
            content_address,
        })
    }

    pub fn rewrite_reference_and_deriver_hashes_to_stored_hashes(
        self,
        mapper: &StorePathMapper,
    ) -> Result<Self, UploaderStatedFieldError> {
        let references = self
            .references
            .into_iter()
            .map(|reference| {
                mapper
                    .inbound_text_if_exact_store_path(&reference)
                    .map_err(|_| UploaderStatedFieldError::StatedReferenceIsNotStorePath(reference))
            })
            .collect::<Result<_, _>>()?;
        let deriver = self
            .deriver
            .map(|deriver| {
                mapper
                    .inbound_text_if_exact_store_path(&deriver)
                    .map_err(|_| UploaderStatedFieldError::StatedDeriverIsNotDrvStorePath(deriver))
            })
            .transpose()?;
        Ok(Self {
            references,
            deriver,
            content_address: self.content_address,
        })
    }

    pub fn with_server_known_derivation_path_from_dispatched_build_job(
        self,
        derivation_path: Option<String>,
    ) -> Self {
        Self {
            deriver: derivation_path,
            ..self
        }
    }

    pub fn references(&self) -> &[String] {
        &self.references
    }

    pub fn deriver(&self) -> Option<&str> {
        self.deriver.as_deref()
    }

    pub fn checked_content_address_text(&self) -> Option<&str> {
        self.content_address.as_deref()
    }

    pub fn parsed_content_address(&self) -> Option<ContentAddress> {
        self.content_address
            .as_deref()
            .and_then(|ca| ContentAddress::from_str(ca).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

    fn check(
        references: &[String],
        deriver: Option<&str>,
        content_address: Option<&str>,
    ) -> Result<CheckedUploadedStorePathReferencesDeriverAndContentAddress, UploaderStatedFieldError>
    {
        CheckedUploadedStorePathReferencesDeriverAndContentAddress::checked_from_references_deriver_and_content_address(
            references.to_vec(),
            deriver.map(str::to_owned),
            content_address.map(str::to_owned),
        )
    }

    #[test]
    fn values_nix_writes_are_accepted() {
        let content_addresses = [
            "text:sha256:006vc8gixyrcynsx4lz1qxingl0mdja3l0xw1nl0j73isg37x944",
            "fixed:sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf",
            "fixed:r:sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf",
        ];
        let references = [format!("/nix/store/{HASH}-hello"), format!("{HASH}-glibc")];
        for ca in content_addresses {
            let checked = check(&references, Some(&format!("{HASH}-hello.drv")), Some(ca))
                .unwrap_or_else(|e| panic!("{ca}: {e}"));
            assert_eq!(checked.checked_content_address_text(), Some(ca));
        }
        for absent in ["", "unknown-deriver"] {
            assert_eq!(check(&[], Some(absent), Some("")).unwrap().deriver(), None);
        }
    }

    #[test]
    fn values_that_are_not_what_the_field_means_are_refused() {
        use UploaderStatedFieldError::*;
        let refused = [
            (vec![format!("{HASH}-hello\nInjected: 1")], None, None),
            (vec!["not-a-store-path".to_owned()], None, None),
            (vec![], Some(format!("{HASH}-hello.drv\nInjected: 1")), None),
            (vec![], Some(format!("{HASH}-hello")), None),
            (vec![], None, Some("fixed:r:sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf\nInjected: 1".to_owned())),
            (vec![], None, Some("unknown:sha256:1bnnhb0pfx49mg15fmk3jx34wj8j24ygqcq7xww9g8qcyaf23rkf".to_owned())),
        ];
        let errors: Vec<_> = refused
            .iter()
            .map(|(references, deriver, ca)| {
                check(references, deriver.as_deref(), ca.as_deref()).unwrap_err()
            })
            .collect();
        assert!(
            matches!(
                errors.as_slice(),
                [
                    StatedReferenceIsNotStorePath(_),
                    StatedReferenceIsNotStorePath(_),
                    StatedDeriverIsNotDrvStorePath(_),
                    StatedDeriverIsNotDrvStorePath(_),
                    StatedContentAddressIsNotNixContentAddress(_),
                    StatedContentAddressIsNotNixContentAddress(_),
                ]
            ),
            "{errors:?}"
        );
    }
}
