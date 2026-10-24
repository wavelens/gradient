/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub(super) fn fingerprint(drv_path: &str, output: &str, out_path: &str) -> String {
    serde_json::json!({
        "key": { "drvPath": drv_path, "outputName": output },
        "value": { "outPath": out_path },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_fingerprint_is_the_unsigned_realisation_json() {
        assert_eq!(
            super::fingerprint("aaaa-bar.drv", "out", "bbbb-bar"),
            r#"{"key":{"drvPath":"aaaa-bar.drv","outputName":"out"},"value":{"outPath":"bbbb-bar"}}"#
        );
    }
}
