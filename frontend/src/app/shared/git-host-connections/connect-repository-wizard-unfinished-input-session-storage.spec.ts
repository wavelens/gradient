/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  SavedConnectRepositoryWizardInput,
  getMostRecentlySavedUnfinishedConnectRepositoryWizardInputAcrossProjects,
  readProjectConnectRepositoryDraftFromSessionStorage,
  removeProjectWizardDraftFromSessionStorage,
  saveProjectWizardDraftToSessionStorage,
} from './connect-repository-wizard-unfinished-input-session-storage';

function savedInput(project: string, savedAt: number): SavedConnectRepositoryWizardInput {
  return {
    project,
    step: 'git-host',
    form: { repository: `https://github.com/${project}/app`, display_name: 'app', name: 'app', wildcard: 'packages.**', branch: '', polling: true, cron: '', exec_enabled: false },
    selectedConnectionId: null,
    savedAt,
  };
}

describe('saved input of the connect repository wizard', () => {
  afterEach(() => {
    vi.restoreAllMocks();
    sessionStorage.clear();
  });

  it('Return the input that was saved most recently across all projects.', () => {
    saveProjectWizardDraftToSessionStorage(savedInput('older', 1));
    saveProjectWizardDraftToSessionStorage(savedInput('newer', 2));
    sessionStorage.setItem('gradient.connect-repository-draft:broken', '{not json');
    expect(getMostRecentlySavedUnfinishedConnectRepositoryWizardInputAcrossProjects()?.project).toBe('newer');
    expect(readProjectConnectRepositoryDraftFromSessionStorage('older')?.form.repository).toBe('https://github.com/older/app');
    expect(readProjectConnectRepositoryDraftFromSessionStorage('broken')).toBeNull();
    removeProjectWizardDraftFromSessionStorage('newer');
    expect(getMostRecentlySavedUnfinishedConnectRepositoryWizardInputAcrossProjects()?.project).toBe('older');
  });

  it('No dependency on session storage', () => {
    const blocked = () => {
      throw new DOMException('blocked', 'SecurityError');
    };
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(blocked);
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(blocked);
    vi.spyOn(Storage.prototype, 'removeItem').mockImplementation(blocked);
    vi.spyOn(Storage.prototype, 'key').mockImplementation(blocked);
    expect(() => saveProjectWizardDraftToSessionStorage(savedInput('acme', 1))).not.toThrow();
    expect(readProjectConnectRepositoryDraftFromSessionStorage('acme')).toBeNull();
    expect(() => removeProjectWizardDraftFromSessionStorage('acme')).not.toThrow();
    expect(getMostRecentlySavedUnfinishedConnectRepositoryWizardInputAcrossProjects()).toBeNull();
  });
});
