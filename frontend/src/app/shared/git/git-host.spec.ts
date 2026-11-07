/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { firstRepositoryHost, gitHostSettingsLinks, guessGitHost, repositoryHost } from './git-host';

describe('git host helpers', () => {
  it.each([
    ['ssh://git@forgejo.example.com/acme/app.git', 'forgejo.example.com'],
    ['git@Gitea.example.com:acme/app.git', 'gitea.example.com'],
    ['https://gitlab.com/acme/app?dir=nix', 'gitlab.com'],
    ['git+https://codeberg.org/acme/app', 'codeberg.org'],
    ['not a url', null],
  ])('reads the host of %s', (url, host) => {
    expect(repositoryHost(url)).toBe(host);
  });

  it('takes the first parseable host', () => {
    expect(firstRepositoryHost(['nope', 'git@host.example:a/b'])).toBe('host.example');
  });

  it('guesses the Git host type from the host name', () => {
    expect(guessGitHost('github.com')).toBe('forgejo');
    expect(guessGitHost('gitlab.example.com')).toBe('gitlab');
    expect(guessGitHost('git.example.com')).toBe('forgejo');
  });

  it('links to the deploy key and webhook settings of the repository', () => {
    expect(gitHostSettingsLinks('git@forgejo.example.com:acme/app.git', 'forgejo')).toEqual({
      deployKeys: 'https://forgejo.example.com/acme/app/settings/keys',
      webhooks: 'https://forgejo.example.com/acme/app/settings/hooks',
    });
    expect(gitHostSettingsLinks('https://gitlab.com/acme/app', 'gitlab')?.webhooks).toBe('https://gitlab.com/acme/app/-/hooks');
  });
});
