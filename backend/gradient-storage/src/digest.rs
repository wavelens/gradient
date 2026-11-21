/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use async_compression::tokio::bufread::ZstdDecoder;
use gradient_util::nix_hash::{NIX32_CHARS, is_nix32_hash, normalize_nar_hash};
use gradient_wire::content_addressed_store_path_from_name_address_references::path_for;
use harmonia_store_content_address::{ContentAddress, ContentAddressMethodAlgorithm};
use harmonia_store_path::{StorePath, StorePathSet};
use harmonia_utils_hash::{Algorithm, Context, Hash, HashFormat as _, Sha256};
use std::collections::HashSet;
use std::pin::Pin;
use std::task::{Poll, ready};
use tokio::io::{AsyncRead, AsyncReadExt as _, BufReader, ReadBuf};

pub fn file_hash_sri(bytes: &[u8]) -> String {
    Sha256::digest(bytes).as_sri().to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("NAR object missing from storage")]
    Missing,
    #[error("NAR size mismatch: expected {expected} bytes, got {actual}")]
    Size { expected: u64, actual: u64 },
    #[error("NAR content hash mismatch: expected {expected}, computed {computed}")]
    Hash { expected: String, computed: String },
    #[error("compressed upload is longer than the declared {limit} bytes")]
    CompressedUploadExceedsClaimedFileSize { limit: u64 },
    #[error("The decoded NAR is longer than the declared {limit} bytes.")]
    DecodedNarLargerThanClaimedNarSize { limit: u64 },
    #[error("Uploaded file is not valid zstd data: {0}")]
    UploadedFileIsNotValidZstd(std::io::Error),
    #[error("The decoded NAR has {actual} bytes, but the expected size is {expected} bytes.")]
    DecodedNarSizeDiffersFromClaimedNarSize { expected: u64, actual: u64 },
    #[error("Decoded NAR hash does not match: expected {expected}, computed {computed}")]
    DecodedNarHashMismatch { expected: String, computed: String },
    #[error("Hash {expected} of the NAR file is not SHA-256")]
    HashAlgorithmIsNotSha256 { expected: String },
    #[error("The decoded NAR does not include the claimed reference {reference}.")]
    ReferenceNotInDecodedNar { reference: String },
    #[error(
        "The asserted reference {reference} is not a store path with a 32-character nix32 hash part."
    )]
    ClaimedReferenceNotStorePathWith32CharacterNix32Hash { reference: String },
    #[error(
        "The .drv suffix and content address {content_address} of store path {store_path} conflict. The content address is not in text format."
    )]
    DrvPathHasNonTextContentAddress {
        store_path: String,
        content_address: String,
    },
    #[error("Content address and uploaded content disagree: {stated} vs {computed}.")]
    ContentAddressHashMismatch { stated: String, computed: String },
    #[error(
        "The content address, name and references of store path {uploaded} do not produce that path. Those inputs produce {computed}."
    )]
    ComputedStorePathDiffersFromUploadedStorePath { uploaded: String, computed: String },
    #[error(
        "The NAR isn't exactly one regular file. It can't match the content address {content_address} of a single file."
    )]
    FlatContentAddressNotSingleRegularFile { content_address: String },
    #[error(transparent)]
    Store(#[from] anyhow::Error),
}

impl VerifyError {
    pub fn is_caused_by_uploader(&self) -> bool {
        !matches!(self, Self::Missing | Self::Store(_))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ClientClaimedNarUploadHashesAndSizes<'a> {
    pub file_hash: &'a str,
    pub file_size: u64,
    pub nar_hash: &'a str,
    pub nar_size: u64,
    pub claimed_store_path_references: &'a [String],
    pub uploaded_nar_with_stated_content_address_to_check:
        Option<UploadedStorePathAndContentAddress<'a>>,
}

#[derive(Debug, Clone, Copy)]
pub struct UploadedStorePathAndContentAddress<'a> {
    pub store_path: &'a str,
    pub content_address: Option<ContentAddress>,
}

impl UploadedStorePathAndContentAddress<'_> {
    fn base_name(&self) -> &str {
        self.store_path
            .rsplit('/')
            .next()
            .unwrap_or(self.store_path)
    }

    fn content_address_method_and_hash_algorithm_to_check(
        &self,
    ) -> Result<Option<ContentAddressMethodAlgorithm>, VerifyError> {
        let is_drv = self.base_name().ends_with(".drv");
        match self.content_address {
            Some(ca) if is_drv && !matches!(ca, ContentAddress::Text(_)) => {
                Err(VerifyError::DrvPathHasNonTextContentAddress {
                    store_path: self.store_path.to_owned(),
                    content_address: ca.to_string(),
                })
            }
            Some(ca) => Ok(Some(ca.method_algorithm())),
            None if is_drv => Ok(Some(ContentAddressMethodAlgorithm::Text)),
            None => Ok(None),
        }
    }
}

/// Only this module can create a value. The presence of a value is proof that the server read all bytes itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerComputedNarAndCompressedFileHashesAndSizes {
    file_hash: String,
    file_size: u64,
    nar_hash: String,
    nar_size: u64,
}

impl ServerComputedNarAndCompressedFileHashesAndSizes {
    pub fn file_hash(&self) -> &str {
        &self.file_hash
    }

    pub fn file_size(&self) -> u64 {
        self.file_size
    }

    pub fn nar_hash(&self) -> &str {
        &self.nar_hash
    }

    pub fn nar_size(&self) -> u64 {
        self.nar_size
    }

    pub fn object_storage_key_for_store_path_hash_and_compressed_file_sha256(
        &self,
        hash: &str,
    ) -> String {
        let file_sha256 = self
            .file_hash
            .strip_prefix("sha256:")
            .unwrap_or(&self.file_hash);
        format!("{hash}-{file_sha256}")
    }
}

#[cfg(feature = "testing")]
pub fn compress_nar_bytes_and_verify_stream(
    nar: &[u8],
) -> ServerComputedNarAndCompressedFileHashesAndSizes {
    let compressed =
        zstd::encode_all(nar, 0).expect("zstd compression of the in-memory byte slice");
    verify_server_hashes_and_sizes_for_in_memory_nar_and_compressed_bytes(nar, &compressed)
}

#[cfg(feature = "testing")]
pub fn compress_nar_bytes_as_two_zstd_frames_and_verify_hashes_and_sizes(
    nar: &[u8],
) -> ServerComputedNarAndCompressedFileHashesAndSizes {
    let (head, tail) = nar.split_at(nar.len() / 2);
    let mut compressed =
        zstd::encode_all(head, 0).expect("zstd compression of the in-memory byte slice");
    compressed
        .extend(zstd::encode_all(tail, 0).expect("zstd compression of the in-memory byte slice"));
    verify_server_hashes_and_sizes_for_in_memory_nar_and_compressed_bytes(nar, &compressed)
}

#[cfg(feature = "testing")]
fn verify_server_hashes_and_sizes_for_in_memory_nar_and_compressed_bytes(
    nar: &[u8],
    compressed: &[u8],
) -> ServerComputedNarAndCompressedFileHashesAndSizes {
    let claimed = ClientClaimedNarUploadHashesAndSizes {
        file_hash: &file_hash_sri(compressed),
        file_size: compressed.len() as u64,
        nar_hash: &file_hash_sri(nar),
        nar_size: nar.len() as u64,
        claimed_store_path_references: &[],
        uploaded_nar_with_stated_content_address_to_check: None,
    };
    futures::executor::block_on(verify_zstd_nar_stream_sha256_and_sizes_against_claims(
        compressed, &claimed,
    ))
    .expect("Honest claims pass the verifier")
}

struct Sha256AndByteCountReaderWithClaimedSizeLimit<R> {
    inner: R,
    hash: Context,
    len: u64,
    limit: u64,
    storage_read_failed: bool,
}

impl<R: AsyncRead + Unpin> AsyncRead for Sha256AndByteCountReaderWithClaimedSizeLimit<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        if let Err(e) = ready!(Pin::new(&mut this.inner).poll_read(cx, buf)) {
            this.storage_read_failed = true;
            return Poll::Ready(Err(e));
        }
        let fresh = &buf.filled()[before..];
        this.len += fresh.len() as u64;
        if this.len > this.limit {
            return Poll::Ready(Err(std::io::Error::other(
                VerifyError::CompressedUploadExceedsClaimedFileSize { limit: this.limit },
            )));
        }
        this.hash.update(fresh);
        Poll::Ready(Ok(()))
    }
}

const NIX32_HASH_PART_CHARACTER_LENGTH: usize = 32;

/// The last 31 bytes of the previous chunk stay in a buffer. The scanner can find hash parts that span two chunks this way. There is one nix32 alphabet check per byte. One set lookup per complete 32-byte window.
struct NarReferenceHashPartScanner<'a> {
    claimed: Vec<(&'a str, [u8; NIX32_HASH_PART_CHARACTER_LENGTH])>,
    unseen: HashSet<[u8; NIX32_HASH_PART_CHARACTER_LENGTH]>,
    previous_chunk_tail: Vec<u8>,
}

impl<'a> NarReferenceHashPartScanner<'a> {
    fn new(references: &'a [String]) -> Result<Self, VerifyError> {
        let claimed = references
            .iter()
            .map(|reference| {
                let base_name = reference.rsplit('/').next().unwrap_or(reference);
                base_name
                    .split_once('-')
                    .filter(|(hash_part, _)| is_nix32_hash(hash_part))
                    .and_then(|(hash_part, _)| hash_part.as_bytes().try_into().ok())
                    .map(|hash_part| (reference.as_str(), hash_part))
                    .ok_or_else(|| {
                        VerifyError::ClaimedReferenceNotStorePathWith32CharacterNix32Hash {
                            reference: reference.clone(),
                        }
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let unseen = claimed.iter().map(|(_, hash_part)| *hash_part).collect();
        Ok(Self {
            claimed,
            unseen,
            previous_chunk_tail: Vec::with_capacity(2 * NIX32_HASH_PART_CHARACTER_LENGTH),
        })
    }

    fn feed_next_decoded_nar_bytes_chunk(&mut self, chunk: &[u8]) {
        if self.unseen.is_empty() {
            return;
        }
        let boundary_len = self.previous_chunk_tail.len();
        self.previous_chunk_tail
            .extend_from_slice(&chunk[..chunk.len().min(NIX32_HASH_PART_CHARACTER_LENGTH - 1)]);
        let boundary = std::mem::take(&mut self.previous_chunk_tail);
        self.remove_hash_parts_found_in_bytes_from_unseen_claimed_references(&boundary);
        self.remove_hash_parts_found_in_bytes_from_unseen_claimed_references(chunk);
        let mut joined = boundary;
        joined.truncate(boundary_len);
        joined.extend_from_slice(chunk);
        let keep_from = joined
            .len()
            .saturating_sub(NIX32_HASH_PART_CHARACTER_LENGTH - 1);
        joined.drain(..keep_from);
        self.previous_chunk_tail = joined;
    }

    fn remove_hash_parts_found_in_bytes_from_unseen_claimed_references(&mut self, bytes: &[u8]) {
        let mut nix32_run = 0;
        for (end, byte) in bytes.iter().enumerate() {
            nix32_run = if NIX32_CHARS.contains(byte) {
                nix32_run + 1
            } else {
                0
            };
            if nix32_run >= NIX32_HASH_PART_CHARACTER_LENGTH {
                let window = &bytes[end + 1 - NIX32_HASH_PART_CHARACTER_LENGTH..=end];
                if let Ok(window) = <[u8; NIX32_HASH_PART_CHARACTER_LENGTH]>::try_from(window) {
                    self.unseen.remove(&window);
                }
            }
        }
    }

    fn finish_scan_and_error_on_first_unseen_reference_hash_part(self) -> Result<(), VerifyError> {
        match self
            .claimed
            .iter()
            .find(|(_, hash_part)| self.unseen.contains(hash_part))
        {
            Some((reference, _)) => Err(VerifyError::ReferenceNotInDecodedNar {
                reference: (*reference).to_owned(),
            }),
            None => Ok(()),
        }
    }
}

fn encode_nix_archive_string(text: &str) -> Vec<u8> {
    let mut encoded = (text.len() as u64).to_le_bytes().to_vec();
    encoded.extend_from_slice(text.as_bytes());
    encoded.resize(encoded.len().next_multiple_of(8), 0);
    encoded
}

fn plain_and_executable_regular_file_nar_header_prefixes() -> [Vec<u8>; 2] {
    let encode_all = |texts: &[&str]| -> Vec<u8> {
        texts
            .iter()
            .flat_map(|text| encode_nix_archive_string(text))
            .collect()
    };
    let opening = ["nix-archive-1", "(", "type", "regular"];
    [
        encode_all(&[&opening[..], &["contents"]].concat()),
        encode_all(&[&opening[..], &["executable", "", "contents"]].concat()),
    ]
}

enum SingleFileNarHashParseState {
    CollectingNarHeaderBytesForFileSize(Vec<u8>),
    HashingFileContentsWithBytesRemaining { remaining: u64, padding: usize },
    ClosingZeroPaddingAndParenthesis { expected: Vec<u8>, matched: usize },
    SingleFileEnded,
    NotExactlyOneRegularFile,
}

struct NarSingleRegularFileContentHasher {
    hash: Context,
    state: SingleFileNarHashParseState,
}

impl NarSingleRegularFileContentHasher {
    fn new(algorithm: Algorithm) -> Self {
        Self {
            hash: Context::new(algorithm),
            state: SingleFileNarHashParseState::CollectingNarHeaderBytesForFileSize(Vec::new()),
        }
    }

    fn feed(&mut self, mut chunk: &[u8]) {
        use SingleFileNarHashParseState as State;
        while !chunk.is_empty() {
            match &mut self.state {
                State::CollectingNarHeaderBytesForFileSize(collected) => {
                    let prefixes = plain_and_executable_regular_file_nar_header_prefixes();
                    let longest = prefixes.iter().map(Vec::len).max().unwrap_or(0) + 8;
                    let taken = chunk.len().min(longest - collected.len());
                    collected.extend_from_slice(&chunk[..taken]);
                    chunk = &chunk[taken..];
                    let parsed = prefixes.iter().find_map(|prefix| {
                        let size = collected.get(prefix.len()..prefix.len() + 8)?;
                        collected.starts_with(prefix).then(|| {
                            let size = u64::from_le_bytes(size.try_into().unwrap_or_default());
                            (prefix.len() + 8, size)
                        })
                    });
                    let may_still_match = prefixes.iter().any(|prefix| {
                        let shared = prefix.len().min(collected.len());
                        prefix[..shared] == collected[..shared]
                    });
                    match parsed {
                        Some((header_len, size)) => {
                            let after_header = collected.split_off(header_len);
                            let padding = (8 - (size % 8) as usize) % 8;
                            self.state = State::HashingFileContentsWithBytesRemaining {
                                remaining: size,
                                padding,
                            };
                            self.feed(&after_header);
                        }
                        None if !may_still_match || collected.len() >= longest => {
                            self.state = State::NotExactlyOneRegularFile;
                        }
                        None => {}
                    }
                }
                State::HashingFileContentsWithBytesRemaining { remaining, padding } => {
                    let taken = chunk
                        .len()
                        .min(usize::try_from(*remaining).unwrap_or(usize::MAX));
                    self.hash.update(&chunk[..taken]);
                    chunk = &chunk[taken..];
                    *remaining -= taken as u64;
                    if *remaining == 0 {
                        let mut expected = vec![0; *padding];
                        expected.extend_from_slice(&encode_nix_archive_string(")"));
                        self.state = State::ClosingZeroPaddingAndParenthesis {
                            expected,
                            matched: 0,
                        };
                    }
                }
                State::ClosingZeroPaddingAndParenthesis { expected, matched } => {
                    let taken = chunk.len().min(expected.len() - *matched);
                    if chunk[..taken] != expected[*matched..*matched + taken] {
                        self.state = State::NotExactlyOneRegularFile;
                        return;
                    }
                    chunk = &chunk[taken..];
                    *matched += taken;
                    if *matched == expected.len() {
                        self.state = State::SingleFileEnded;
                    }
                }
                State::SingleFileEnded | State::NotExactlyOneRegularFile => {
                    self.state = State::NotExactlyOneRegularFile;
                    return;
                }
            }
        }
        if let State::HashingFileContentsWithBytesRemaining {
            remaining: 0,
            padding,
        } = self.state
        {
            let mut expected = vec![0; padding];
            expected.extend_from_slice(&encode_nix_archive_string(")"));
            self.state = State::ClosingZeroPaddingAndParenthesis {
                expected,
                matched: 0,
            };
        }
    }

    fn finish(self) -> Option<Hash> {
        matches!(self.state, SingleFileNarHashParseState::SingleFileEnded)
            .then(|| self.hash.finish())
    }
}

enum ContentAddressHashInput {
    HashWholeNar(Context),
    SingleFileContents(NarSingleRegularFileContentHasher),
}

impl ContentAddressHashInput {
    fn from_content_address_method_and_hash_algorithm(
        method_algorithm: ContentAddressMethodAlgorithm,
    ) -> Self {
        match method_algorithm {
            ContentAddressMethodAlgorithm::NixArchive(algorithm) => {
                Self::HashWholeNar(Context::new(algorithm))
            }
            other => {
                Self::SingleFileContents(NarSingleRegularFileContentHasher::new(other.algorithm()))
            }
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        match self {
            Self::HashWholeNar(hash) => hash.update(chunk),
            Self::SingleFileContents(hasher) => hasher.feed(chunk),
        }
    }

    fn finish(self) -> Option<Hash> {
        match self {
            Self::HashWholeNar(hash) => Some(hash.finish()),
            Self::SingleFileContents(hasher) => hasher.finish(),
        }
    }
}

fn verify_content_address_and_uploaded_store_path(
    uploaded: &UploadedStorePathAndContentAddress<'_>,
    method_algorithm: ContentAddressMethodAlgorithm,
    computed_hash: Option<Hash>,
    references: &[String],
) -> Result<(), VerifyError> {
    let computed_hash =
        computed_hash.ok_or_else(|| VerifyError::FlatContentAddressNotSingleRegularFile {
            content_address: uploaded
                .content_address
                .map_or_else(|| method_algorithm.to_string(), |ca| ca.to_string()),
        })?;
    let computed = match (method_algorithm, computed_hash) {
        (ContentAddressMethodAlgorithm::Text, Hash::SHA256(digest)) => ContentAddress::Text(digest),
        (ContentAddressMethodAlgorithm::NixArchive(_), hash) => ContentAddress::NixArchive(hash),
        (_, hash) => ContentAddress::Flat(hash),
    };
    if let Some(stated) = uploaded.content_address
        && stated != computed
    {
        return Err(VerifyError::ContentAddressHashMismatch {
            stated: stated.to_string(),
            computed: computed.to_string(),
        });
    }
    let references = references
        .iter()
        .map(|reference| {
            StorePath::from_base_path(reference.rsplit('/').next().unwrap_or(reference)).map_err(
                |_| VerifyError::ClaimedReferenceNotStorePathWith32CharacterNix32Hash {
                    reference: reference.clone(),
                },
            )
        })
        .collect::<Result<StorePathSet, _>>()?;
    let base_name = uploaded.base_name();
    let name = base_name
        .split_once('-')
        .map_or(base_name, |(_, name)| name);
    let computed_path = path_for(name, &computed, &references)
        .map_or_else(|error| error.to_string(), |path| path.to_string());
    if computed_path != base_name {
        return Err(VerifyError::ComputedStorePathDiffersFromUploadedStorePath {
            uploaded: uploaded.store_path.to_owned(),
            computed: computed_path,
        });
    }
    Ok(())
}

fn finish_sha256_context_to_nix32_string(hash: Context) -> Result<String, VerifyError> {
    let digest = Sha256::try_from(hash.finish())
        .map_err(|_| VerifyError::Store(anyhow::anyhow!("sha256 finalize produced non-sha256")))?;
    Ok(normalize_nar_hash(&digest.as_sri().to_string()))
}

fn normalize_client_hash_or_reject_if_not_sha256(claimed: &str) -> Result<String, VerifyError> {
    let normalized = normalize_nar_hash(claimed);
    if !normalized.starts_with("sha256:") {
        return Err(VerifyError::HashAlgorithmIsNotSha256 {
            expected: claimed.to_owned(),
        });
    }
    Ok(normalized)
}

pub async fn verify_zstd_nar_stream_sha256_and_sizes_against_claims<R: AsyncRead + Unpin>(
    compressed: R,
    claimed: &ClientClaimedNarUploadHashesAndSizes<'_>,
) -> Result<ServerComputedNarAndCompressedFileHashesAndSizes, VerifyError> {
    let claimed_file_hash = normalize_client_hash_or_reject_if_not_sha256(claimed.file_hash)?;
    let claimed_nar_hash = normalize_client_hash_or_reject_if_not_sha256(claimed.nar_hash)?;
    let mut reference_scanner =
        NarReferenceHashPartScanner::new(claimed.claimed_store_path_references)?;
    let mut content_address_check = match claimed.uploaded_nar_with_stated_content_address_to_check
    {
        Some(uploaded) => uploaded
            .content_address_method_and_hash_algorithm_to_check()?
            .map(|method_algorithm| {
                (
                    uploaded,
                    method_algorithm,
                    ContentAddressHashInput::from_content_address_method_and_hash_algorithm(
                        method_algorithm,
                    ),
                )
            }),
        None => None,
    };

    let mut decoder = ZstdDecoder::new(BufReader::new(
        Sha256AndByteCountReaderWithClaimedSizeLimit {
            inner: compressed,
            hash: Context::new(Algorithm::SHA256),
            len: 0,
            limit: claimed.file_size,
            storage_read_failed: false,
        },
    ));
    decoder.multiple_members(true);
    let mut nar_hash = Context::new(Algorithm::SHA256);
    let mut nar_size: u64 = 0;
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = match decoder.read(&mut buf).await {
            Ok(n) => n,
            Err(e) => {
                let source = decoder.get_ref().get_ref();
                return Err(if source.len > source.limit {
                    VerifyError::CompressedUploadExceedsClaimedFileSize {
                        limit: source.limit,
                    }
                } else if source.storage_read_failed {
                    VerifyError::Store(e.into())
                } else {
                    VerifyError::UploadedFileIsNotValidZstd(e)
                });
            }
        };
        if n == 0 {
            break;
        }
        nar_size += n as u64;
        if nar_size > claimed.nar_size {
            return Err(VerifyError::DecodedNarLargerThanClaimedNarSize {
                limit: claimed.nar_size,
            });
        }
        nar_hash.update(&buf[..n]);
        reference_scanner.feed_next_decoded_nar_bytes_chunk(&buf[..n]);
        if let Some((_, _, hash_input)) = &mut content_address_check {
            hash_input.feed(&buf[..n]);
        }
    }

    let compressed = decoder.into_inner().into_inner();
    if compressed.len != claimed.file_size {
        return Err(VerifyError::Size {
            expected: claimed.file_size,
            actual: compressed.len,
        });
    }
    let file_hash = finish_sha256_context_to_nix32_string(compressed.hash)?;
    if file_hash != claimed_file_hash {
        return Err(VerifyError::Hash {
            expected: claimed_file_hash,
            computed: file_hash,
        });
    }
    if nar_size != claimed.nar_size {
        return Err(VerifyError::DecodedNarSizeDiffersFromClaimedNarSize {
            expected: claimed.nar_size,
            actual: nar_size,
        });
    }
    let nar_hash = finish_sha256_context_to_nix32_string(nar_hash)?;
    if nar_hash != claimed_nar_hash {
        return Err(VerifyError::DecodedNarHashMismatch {
            expected: claimed_nar_hash,
            computed: nar_hash,
        });
    }
    reference_scanner.finish_scan_and_error_on_first_unseen_reference_hash_part()?;
    if let Some((uploaded, method_algorithm, hash_input)) = content_address_check {
        verify_content_address_and_uploaded_store_path(
            &uploaded,
            method_algorithm,
            hash_input.finish(),
            claimed.claimed_store_path_references,
        )?;
    }

    Ok(ServerComputedNarAndCompressedFileHashesAndSizes {
        file_hash,
        file_size: compressed.len,
        nar_hash,
        nar_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAR: &[u8] = b"gradient nar integrity check";
    const BLAKE3: &str = "blake3:11cxppanr71mzl1xnyax8rccaj5milx2fx9vnvzk6la672nb6dv4";

    fn zstd_compress_nar_bytes(nar: &[u8]) -> Vec<u8> {
        zstd::encode_all(nar, 3).unwrap()
    }

    fn expected_sri_sha256_hashes_for_compressed_file_and_nar(
        compressed: &[u8],
        nar: &[u8],
    ) -> (String, String) {
        (file_hash_sri(compressed), file_hash_sri(nar))
    }

    async fn run_zstd_nar_verifier_with_claimed_hashes_and_sizes(
        compressed: &[u8],
        file_hash: &str,
        file_size: u64,
        nar_hash: &str,
        nar_size: u64,
    ) -> Result<ServerComputedNarAndCompressedFileHashesAndSizes, VerifyError> {
        verify_zstd_nar_stream_sha256_and_sizes_against_claims(
            compressed,
            &ClientClaimedNarUploadHashesAndSizes {
                file_hash,
                file_size,
                nar_hash,
                nar_size,
                claimed_store_path_references: &[],
                uploaded_nar_with_stated_content_address_to_check: None,
            },
        )
        .await
    }

    const REFERENCED_HASH_PART: &str = "0c9w7q7v4lkmn4ir1a3ciaclnh2hwzxm";
    const SELF_HASH_PART: &str = "1bnnhb0pfx49mg15fmk3jx34wj8j24yg";

    async fn verify_nar_with_claimed_references(
        nar: &[u8],
        references: &[String],
    ) -> Result<ServerComputedNarAndCompressedFileHashesAndSizes, VerifyError> {
        let compressed = zstd_compress_nar_bytes(nar);
        verify_zstd_nar_stream_sha256_and_sizes_against_claims(
            compressed.as_slice(),
            &ClientClaimedNarUploadHashesAndSizes {
                file_hash: &file_hash_sri(&compressed),
                file_size: compressed.len() as u64,
                nar_hash: &file_hash_sri(nar),
                nar_size: nar.len() as u64,
                claimed_store_path_references: references,
                uploaded_nar_with_stated_content_address_to_check: None,
            },
        )
        .await
    }

    #[tokio::test]
    async fn claimed_reference_present_in_nar_and_self_reference_pass() {
        let nar = format!(
            "prefix /nix/store/{REFERENCED_HASH_PART}-dep/lib and /nix/store/{SELF_HASH_PART}-self"
        );
        let references = [
            format!("/nix/store/{REFERENCED_HASH_PART}-dep"),
            format!("{SELF_HASH_PART}-self"),
        ];
        verify_nar_with_claimed_references(nar.as_bytes(), &references)
            .await
            .expect("Every claimed reference occurs in the NAR");
    }

    #[tokio::test]
    async fn claimed_reference_absent_from_nar_is_refused() {
        let nar = format!("only /nix/store/{SELF_HASH_PART}-self");
        let reference = format!("/nix/store/{REFERENCED_HASH_PART}-dep");
        let error =
            verify_nar_with_claimed_references(nar.as_bytes(), std::slice::from_ref(&reference))
                .await
                .unwrap_err();
        assert!(
            matches!(&error, VerifyError::ReferenceNotInDecodedNar { reference: r } if *r == reference)
        );
        assert!(error.is_caused_by_uploader());
    }

    #[tokio::test]
    async fn claimed_reference_that_is_not_a_store_path_is_refused() {
        let error = verify_nar_with_claimed_references(b"nar", &["not-a-store-path".to_owned()])
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            VerifyError::ClaimedReferenceNotStorePathWith32CharacterNix32Hash { .. }
        ));
    }

    #[test]
    fn scanner_finds_claimed_reference_hash_part_across_one_byte_chunks() {
        let references = [format!("/nix/store/{REFERENCED_HASH_PART}-dep")];
        let nar = format!("x{REFERENCED_HASH_PART}y");
        let mut scanner = NarReferenceHashPartScanner::new(&references).unwrap();
        for byte in nar.as_bytes() {
            scanner.feed_next_decoded_nar_bytes_chunk(std::slice::from_ref(byte));
        }
        scanner
            .finish_scan_and_error_on_first_unseen_reference_hash_part()
            .expect("A hash part across 32 chunks is found");
    }

    #[tokio::test]
    async fn claimed_reference_spanning_the_read_buffer_boundary_is_found() {
        let mut nar = vec![b' '; 256 * 1024 - 10];
        nar.extend_from_slice(REFERENCED_HASH_PART.as_bytes());
        nar.extend_from_slice(&[b' '; 64]);
        verify_nar_with_claimed_references(
            &nar,
            &[format!("/nix/store/{REFERENCED_HASH_PART}-dep")],
        )
        .await
        .expect("A hash part across two reads is found");
    }

    #[tokio::test]
    async fn honest_claims_return_server_computed_hashes_and_sizes() {
        let bytes = zstd_compress_nar_bytes(NAR);
        let (file_hash, nar_hash) =
            expected_sri_sha256_hashes_for_compressed_file_and_nar(&bytes, NAR);
        let facts = run_zstd_nar_verifier_with_claimed_hashes_and_sizes(
            &bytes,
            &file_hash,
            bytes.len() as u64,
            &nar_hash,
            NAR.len() as u64,
        )
        .await
        .unwrap();
        assert_eq!(facts.file_hash(), normalize_nar_hash(&file_hash));
        assert_eq!(facts.nar_hash(), normalize_nar_hash(&nar_hash));
        assert_eq!(facts.file_size(), bytes.len() as u64);
        assert_eq!(facts.nar_size(), NAR.len() as u64);
    }

    #[tokio::test]
    async fn claimed_nar_hash_of_other_content_gives_nar_hash_mismatch_error() {
        let bytes = zstd_compress_nar_bytes(NAR);
        let (file_hash, _) = expected_sri_sha256_hashes_for_compressed_file_and_nar(&bytes, NAR);
        let other = file_hash_sri(b"gradient nar integrity CHECK");
        assert!(matches!(
            run_zstd_nar_verifier_with_claimed_hashes_and_sizes(
                &bytes,
                &file_hash,
                bytes.len() as u64,
                &other,
                NAR.len() as u64
            )
            .await
            .unwrap_err(),
            VerifyError::DecodedNarHashMismatch { .. }
        ));
    }

    #[tokio::test]
    async fn read_aborts_when_decoded_bytes_exceed_claimed_nar_size() {
        let bomb = vec![0u8; 8 * 1024 * 1024];
        let bytes = zstd_compress_nar_bytes(&bomb);
        let (file_hash, nar_hash) =
            expected_sri_sha256_hashes_for_compressed_file_and_nar(&bytes, &bomb);
        assert!(matches!(
            run_zstd_nar_verifier_with_claimed_hashes_and_sizes(
                &bytes,
                &file_hash,
                bytes.len() as u64,
                &nar_hash,
                1024
            )
            .await
            .unwrap_err(),
            VerifyError::DecodedNarLargerThanClaimedNarSize { limit: 1024 }
        ));
    }

    #[tokio::test]
    async fn compressed_bytes_over_claimed_file_size_aborts_read() {
        let bytes = zstd_compress_nar_bytes(NAR);
        let (file_hash, nar_hash) =
            expected_sri_sha256_hashes_for_compressed_file_and_nar(&bytes, NAR);
        assert!(matches!(
            run_zstd_nar_verifier_with_claimed_hashes_and_sizes(
                &bytes,
                &file_hash,
                4,
                &nar_hash,
                NAR.len() as u64
            )
            .await
            .unwrap_err(),
            VerifyError::CompressedUploadExceedsClaimedFileSize { limit: 4 }
        ));
    }

    #[tokio::test]
    async fn test_compressed_bytes_with_trailing_garbage_are_refused_as_uploader_error() {
        let bytes = zstd_compress_nar_bytes(NAR);
        let (file_hash, nar_hash) =
            expected_sri_sha256_hashes_for_compressed_file_and_nar(&bytes, NAR);
        let mut tampered = bytes.clone();
        tampered.push(0);
        let err = run_zstd_nar_verifier_with_claimed_hashes_and_sizes(
            &tampered,
            &file_hash,
            tampered.len() as u64,
            &nar_hash,
            NAR.len() as u64,
        )
        .await
        .unwrap_err();
        assert!(err.is_caused_by_uploader(), "{err}");
    }

    #[tokio::test]
    async fn rejects_non_zstd_bytes() {
        let (file_hash, nar_hash) =
            expected_sri_sha256_hashes_for_compressed_file_and_nar(NAR, NAR);
        assert!(matches!(
            run_zstd_nar_verifier_with_claimed_hashes_and_sizes(
                NAR,
                &file_hash,
                NAR.len() as u64,
                &nar_hash,
                NAR.len() as u64
            )
            .await
            .unwrap_err(),
            VerifyError::UploadedFileIsNotValidZstd(_)
        ));
    }

    #[tokio::test]
    async fn claimed_hash_other_than_sha256_is_refused() {
        let bytes = zstd_compress_nar_bytes(NAR);
        let (file_hash, nar_hash) =
            expected_sri_sha256_hashes_for_compressed_file_and_nar(&bytes, NAR);
        for (file, nar) in [(BLAKE3, nar_hash.as_str()), (file_hash.as_str(), BLAKE3)] {
            assert!(matches!(
                run_zstd_nar_verifier_with_claimed_hashes_and_sizes(
                    &bytes,
                    file,
                    bytes.len() as u64,
                    nar,
                    NAR.len() as u64
                )
                .await
                .unwrap_err(),
                VerifyError::HashAlgorithmIsNotSha256 { .. }
            ));
        }
    }

    async fn build_regular_file_nar_bytes_with_contents(contents: &[u8]) -> Vec<u8> {
        use futures::TryStreamExt as _;
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), contents).unwrap();
        let chunks: Vec<_> = harmonia_file_nar::NarByteStream::new(file.path().to_path_buf())
            .try_collect()
            .await
            .unwrap();
        chunks.concat()
    }

    async fn run_nar_verifier_on_bytes_with_uploaded_path_and_optional_stated_content_address(
        nar: &[u8],
        store_path: &str,
        content_address: Option<ContentAddress>,
        references: &[String],
    ) -> Result<ServerComputedNarAndCompressedFileHashesAndSizes, VerifyError> {
        let compressed = zstd_compress_nar_bytes(nar);
        verify_zstd_nar_stream_sha256_and_sizes_against_claims(
            compressed.as_slice(),
            &ClientClaimedNarUploadHashesAndSizes {
                file_hash: &file_hash_sri(&compressed),
                file_size: compressed.len() as u64,
                nar_hash: &file_hash_sri(nar),
                nar_size: nar.len() as u64,
                claimed_store_path_references: references,
                uploaded_nar_with_stated_content_address_to_check: Some(
                    UploadedStorePathAndContentAddress {
                        store_path,
                        content_address,
                    },
                ),
            },
        )
        .await
    }

    fn test_store_path_from_name_content_address_and_references(
        name: &str,
        ca: &ContentAddress,
        references: &[String],
    ) -> String {
        let references = references
            .iter()
            .map(|r| StorePath::from_base_path(r.trim_start_matches("/nix/store/")).unwrap())
            .collect();
        format!("/nix/store/{}", path_for(name, ca, &references).unwrap())
    }

    fn sample_derivation_text_with_one_reference_path_and_reference_paths() -> (Vec<u8>, Vec<String>)
    {
        let reference = format!("/nix/store/{REFERENCED_HASH_PART}-builder.sh");
        let text = format!(
            "Derive([(\"out\",\"\",\"\",\"\")],[],[\"{reference}\"],\"x86_64-linux\",\"/bin/sh\",[],[])"
        );
        (text.into_bytes(), vec![reference])
    }

    #[tokio::test]
    async fn derivation_text_that_does_not_give_its_store_path_is_refused() {
        let (text, references) =
            sample_derivation_text_with_one_reference_path_and_reference_paths();
        let nar = build_regular_file_nar_bytes_with_contents(&text).await;
        let other = ContentAddress::Text(Sha256::digest(b"other derivation text"));
        let store_path = test_store_path_from_name_content_address_and_references(
            "hello.drv",
            &other,
            &references,
        );
        for stated in [None, Some(other)] {
            let error =
                run_nar_verifier_on_bytes_with_uploaded_path_and_optional_stated_content_address(
                    &nar,
                    &store_path,
                    stated,
                    &references,
                )
                .await
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    VerifyError::ComputedStorePathDiffersFromUploadedStorePath { .. }
                        | VerifyError::ContentAddressHashMismatch { .. }
                ),
                "{error}"
            );
            assert!(error.is_caused_by_uploader());
        }
    }

    #[tokio::test]
    async fn derivation_store_path_matching_derivation_text_and_references_passes_with_and_without_text_content_address()
     {
        let (text, references) =
            sample_derivation_text_with_one_reference_path_and_reference_paths();
        let nar = build_regular_file_nar_bytes_with_contents(&text).await;
        let ca = ContentAddress::Text(Sha256::digest(&text));
        let store_path =
            test_store_path_from_name_content_address_and_references("hello.drv", &ca, &references);
        for stated in [None, Some(ca)] {
            run_nar_verifier_on_bytes_with_uploaded_path_and_optional_stated_content_address(
                &nar,
                &store_path,
                stated,
                &references,
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn refuses_derivation_stating_fixed_non_text_content_address() {
        let (text, _) = sample_derivation_text_with_one_reference_path_and_reference_paths();
        let nar = build_regular_file_nar_bytes_with_contents(&text).await;
        let ca = ContentAddress::Flat(Hash::SHA256(Sha256::digest(&text)));
        let store_path =
            test_store_path_from_name_content_address_and_references("hello.drv", &ca, &[]);
        let error =
            run_nar_verifier_on_bytes_with_uploaded_path_and_optional_stated_content_address(
                &nar,
                &store_path,
                Some(ca),
                &[],
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, VerifyError::DrvPathHasNonTextContentAddress { .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn rejects_fixed_output_path_with_mismatched_content_address_hash() {
        let nar = build_regular_file_nar_bytes_with_contents(b"tarball bytes").await;
        let claimed = Sha256::digest(b"the tarball the hash was taken from");
        for ca in [
            ContentAddress::Flat(Hash::SHA256(claimed)),
            ContentAddress::NixArchive(Hash::SHA256(claimed)),
        ] {
            let store_path =
                test_store_path_from_name_content_address_and_references("source.tar.gz", &ca, &[]);
            let error =
                run_nar_verifier_on_bytes_with_uploaded_path_and_optional_stated_content_address(
                    &nar,
                    &store_path,
                    Some(ca),
                    &[],
                )
                .await
                .unwrap_err();
            assert!(
                matches!(error, VerifyError::ContentAddressHashMismatch { .. }),
                "{error}"
            );
        }
    }

    #[tokio::test]
    async fn honest_flat_and_recursive_fixed_output_uploads_pass_content_address_check() {
        let contents = b"tarball bytes";
        let nar = build_regular_file_nar_bytes_with_contents(contents).await;
        for ca in [
            ContentAddress::Flat(Hash::SHA256(Sha256::digest(contents))),
            ContentAddress::NixArchive(Hash::SHA256(Sha256::digest(&nar))),
        ] {
            let store_path =
                test_store_path_from_name_content_address_and_references("source.tar.gz", &ca, &[]);
            run_nar_verifier_on_bytes_with_uploaded_path_and_optional_stated_content_address(
                &nar,
                &store_path,
                Some(ca),
                &[],
            )
            .await
            .unwrap_or_else(|error| panic!("{ca}: {error}"));
        }
    }

    #[tokio::test]
    async fn refuses_text_content_address_for_nar_without_single_regular_file() {
        use futures::TryStreamExt as _;
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a"), b"a").unwrap();
        let chunks: Vec<_> = harmonia_file_nar::NarByteStream::new(directory.path().to_path_buf())
            .try_collect()
            .await
            .unwrap();
        let nar = chunks.concat();
        let ca = ContentAddress::Text(Sha256::digest(b"a"));
        let store_path =
            test_store_path_from_name_content_address_and_references("hello.drv", &ca, &[]);
        let error =
            run_nar_verifier_on_bytes_with_uploaded_path_and_optional_stated_content_address(
                &nar,
                &store_path,
                None,
                &[],
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                VerifyError::FlatContentAddressNotSingleRegularFile { .. }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn single_file_hasher_matches_when_fed_one_byte_at_a_time() {
        for contents in [&b""[..], b"x", b"exactly8", b"nine bytes"] {
            let nar = build_regular_file_nar_bytes_with_contents(contents).await;
            let mut hasher = NarSingleRegularFileContentHasher::new(Algorithm::SHA256);
            for byte in &nar {
                hasher.feed(std::slice::from_ref(byte));
            }
            assert_eq!(
                hasher.finish(),
                Some(Hash::SHA256(Sha256::digest(contents)))
            );
        }
    }
}
