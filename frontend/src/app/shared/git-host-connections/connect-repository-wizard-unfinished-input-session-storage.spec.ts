/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  SavedConnectRepositoryWizardInput,
  readConnectRepositoryWizardInputFromSessionStorage,
  removeSavedConnectRepositoryWizardInput,
  saveProjectWizardDraftToSessionStorage,
} from './connect-repository-wizard-unfinished-input-session-storage';

function savedInput(project: string): SavedConnectRepositoryWizardInput {
  return {
    project,
    step: 'git-host',
    form: { repository: `https://github.com/${project}/app`, display_name: 'app', name: 'app', wildcard: 'packages.**', branch: '', polling: true, cron: '', exec_enabled: false },
    selectedConnectionId: null,
  };
}

describe('saved input of the connect repository wizard', () => {
  afterEach(() => {
    vi.restoreAllMocks();
    sessionStorage.clear();
  });

  it('Keep one saved input and ignore broken values', () => {
    saveProjectWizardDraftToSessionStorage(savedInput('older'));
    saveProjectWizardDraftToSessionStorage(savedInput('newer'));
    expect(readConnectRepositoryWizardInputFromSessionStorage()?.project).toBe('newer');
    removeSavedConnectRepositoryWizardInput();
    expect(readConnectRepositoryWizardInputFromSessionStorage()).toBeNull();
    sessionStorage.setItem('gradient.connect-repository-draft', '{not json');
    expect(readConnectRepositoryWizardInputFromSessionStorage()).toBeNull();
  });

  it('No dependency on session storage', () => {
    const blocked = () => {
      throw new DOMException('blocked', 'SecurityError');
    };
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(blocked);
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(blocked);
    vi.spyOn(Storage.prototype, 'removeItem').mockImplementation(blocked);
    expect(() => saveProjectWizardDraftToSessionStorage(savedInput('acme'))).not.toThrow();
    expect(readConnectRepositoryWizardInputFromSessionStorage()).toBeNull();
    expect(() => removeSavedConnectRepositoryWizardInput()).not.toThrow();
  });
});
