/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { commitWebUrl, repositoryWebUrl, splitRepository } from './repository';

describe('splitRepository', () => {
  it('splits the dir query off as the flake directory', () => {
    expect(splitRepository('git@github.com:acme/infra.git?dir=nix'))
      .toEqual({ cloneUrl: 'git@github.com:acme/infra.git', flakeDir: 'nix' });
    expect(splitRepository('https://github.com/acme/infra?dir=nix/hosts'))
      .toEqual({ cloneUrl: 'https://github.com/acme/infra', flakeDir: 'nix/hosts' });
  });

  it('has no flake directory without a dir query', () => {
    expect(splitRepository('https://github.com/acme/infra')).toEqual({ cloneUrl: 'https://github.com/acme/infra', flakeDir: null });
    expect(splitRepository('https://github.com/acme/infra?dir=')).toEqual({ cloneUrl: 'https://github.com/acme/infra', flakeDir: null });
  });
});

describe('repositoryWebUrl', () => {
  it('links the repository without its flake directory', () => {
    expect(repositoryWebUrl('git@github.com:acme/infra.git?dir=nix')).toBe('https://github.com/acme/infra');
    expect(commitWebUrl('https://github.com/acme/infra?dir=nix', 'abc')).toBe('https://github.com/acme/infra/commit/abc');
  });

  it('keeps an https URL and drops the .git suffix', () => {
    expect(repositoryWebUrl('https://github.com/wavelens/gradient.git')).toBe('https://github.com/wavelens/gradient');
    expect(repositoryWebUrl('https://github.com/wavelens/gradient')).toBe('https://github.com/wavelens/gradient');
  });

  it('turns an ssh URL into the https page of the same host without user and port', () => {
    expect(repositoryWebUrl('ssh://git@github.com/wavelens/gradient.git')).toBe('https://github.com/wavelens/gradient');
    expect(repositoryWebUrl('ssh://git@git.example.org:2222/team/repo')).toBe('https://git.example.org/team/repo');
    expect(repositoryWebUrl('git+ssh://git@github.com/wavelens/gradient')).toBe('https://github.com/wavelens/gradient');
  });

  it('turns an scp-style address into an https URL', () => {
    expect(repositoryWebUrl('git@github.com:wavelens/gradient.git')).toBe('https://github.com/wavelens/gradient');
  });

  it('returns null for a local path or file URL', () => {
    expect(repositoryWebUrl('/srv/git/repo')).toBeNull();
    expect(repositoryWebUrl('file:///srv/git/repo')).toBeNull();
  });
});

describe('commitWebUrl', () => {
  it('links the commit on the repository host', () => {
    expect(commitWebUrl('ssh://git@github.com/wavelens/gradient.git', 'abc123'))
      .toBe('https://github.com/wavelens/gradient/commit/abc123');
  });

  it('returns null without a commit or a web repository', () => {
    expect(commitWebUrl('https://github.com/wavelens/gradient', '')).toBeNull();
    expect(commitWebUrl('/srv/git/repo', 'abc123')).toBeNull();
  });
});
