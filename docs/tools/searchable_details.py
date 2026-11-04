# SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
# SPDX-License-Identifier: AGPL-3.0-only

import re

IMPORTANT_FURTHER_STEPS = re.compile(
    r'<details(?![^>]*\sopen[\s=/>])'
    r'(?=[^>]*\sclass="[^"]*\bimportant-further-steps\b)[^>]*>'
)


def on_page_content(html, page, config, files):
    return IMPORTANT_FURTHER_STEPS.sub(
        lambda match: match.group().replace(
            "<details", '<details open data-auto-close="true"', 1
        ),
        html,
    )
