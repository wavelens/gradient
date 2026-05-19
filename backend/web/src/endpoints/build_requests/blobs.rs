/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `POST /build-requests/{session}/blobs` — uploads a single blob whose
//! BLAKE3 hash appears in the session's `missing` list. Implementation
//! lands in Task 10.
