/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export const DOCS_URL = 'https://wavelens.github.io/gradient/';

/** Every docs page the UI links to; a moved page or heading is renamed here once. */
export type DocLink =
  | 'concepts/caches/#using-a-cache'
  | 'concepts/caches/#upstream-types'
  | 'concepts/projects-and-tasks/#project'
  | 'concepts/projects-and-tasks/#task'
  | 'concepts/projects-and-tasks/#concurrency'
  | 'concepts/workers/#capabilities'
  | 'guides/actions/'
  | 'guides/flake-updates/#1-track-the-inputs'
  | 'guides/flake-updates/#2-add-the-open-pr-action'
  | 'guides/forge-gitea/#1-create-the-integrations'
  | 'guides/forge-github/#1-register-the-github-app'
  | 'guides/forge-github/#3-install-the-app'
  | 'guides/remote-worker/#1-pick-a-worker-id'
  | 'guides/remote-worker/#2-register-the-worker'
  | 'guides/remote-worker/#peers-file'
  | 'guides/share-a-cache/#1-use-the-cache-on-a-machine'
  | 'guides/share-a-cache/#2-share-with-another-project'
  | 'reference/events/#event-families'
  | 'reference/events/#webhooks'
  | 'reference/wildcards/'
  | 'ui/members-and-roles/#project-roles'
  | 'ui/members-and-roles/#cache-roles';

export function docsUrl(link: DocLink): string {
  return DOCS_URL + link;
}
