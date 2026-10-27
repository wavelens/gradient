/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { repositoryUrlError } from './repository-url';

describe('repositoryUrlError', () => {
  it('accepts every documented form, with ports and IPv6 hosts', () => {
    for (const url of [
      'ssh://git@git.example.de/user/repo.git',
      'ssh://git@git.example.de:2222/user/repo.git',
      'git+ssh://git@[::1]:22/srv/repo',
      'git@git.example.de:user/repo.git',
      'https://git.example.de:8443/user/repo',
      'github:owner/repo',
      '',
    ]) {
      expect(repositoryUrlError(url), url).toBeNull();
    }
  });

  it('names the scp path that was read as a port', () => {
    const error = repositoryUrlError('ssh://git@git.example.de:user/repo.git');
    expect(error).toContain('not `user`');
    expect(error).toContain('git@host:path');
  });

  it('rejects a missing host, a bare word and local files', () => {
    expect(repositoryUrlError('https:///repo')).toContain('Invalid repository URL');
    expect(repositoryUrlError('ssh://[::1/repo')).toContain('Invalid repository URL');
    expect(repositoryUrlError('notaurl')).toContain('Invalid repository URL');
    expect(repositoryUrlError('file:///srv/repo')).toContain('local files');
  });
});
