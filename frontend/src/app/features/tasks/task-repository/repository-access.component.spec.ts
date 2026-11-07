/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { of, throwError } from 'rxjs';
import { TaskRepositoryResponse } from '@core/models';
import { TasksService } from '@core/services/tasks.service';
import { ApiError } from '@core/services/api.service';
import { RepositoryAccessComponent } from './repository-access.component';

const state: TaskRepositoryResponse = {
  repository: 'git@forgejo.example.com:acme/app.git',
  uses_ssh: true,
  deploy_key: 'ssh-ed25519 AAAAkey',
  fetch: null,
  build_on_push: null,
  build_on_pull_request: null,
  last_event: null,
  report_status: null,
  integrations: [],
  can_edit: true,
  can_manage_integrations: true,
};

function render(overrides: Partial<TaskRepositoryResponse>, tasks: object) {
  TestBed.configureTestingModule({
    imports: [RepositoryAccessComponent],
    providers: [provideRouter([]), { provide: TasksService, useValue: tasks }],
  });
  const fixture = TestBed.createComponent(RepositoryAccessComponent);
  fixture.componentRef.setInput('project', 'acme');
  fixture.componentRef.setInput('task', 'app');
  fixture.componentRef.setInput('state', { ...state, ...overrides });
  fixture.componentRef.setInput('gitHost', 'forgejo');
  fixture.detectChanges();
  return fixture;
}

describe('RepositoryAccessComponent', () => {
  it('links to the deploy key settings of the repository on the Git host', () => {
    const el = render({}, {}).nativeElement as HTMLElement;
    const hrefs = Array.from(el.querySelectorAll('a')).map((a) => a.getAttribute('href'));
    expect(hrefs).toContain('https://forgejo.example.com/acme/app/settings/keys');
  });

  it('tests access to the saved URL and reloads the state either way', () => {
    const checkRepository = vi.fn(() => throwError(() => new ApiError('Permission denied (publickey)', 400)));
    const fixture = render({}, { checkRepository });
    const changed = vi.fn();
    fixture.componentInstance.changed.subscribe(changed);
    fixture.componentInstance.testAccess();
    expect(checkRepository).toHaveBeenCalledWith('acme', 'app', 'git@forgejo.example.com:acme/app.git');
    expect(fixture.componentInstance.testError()).toContain('Permission denied');
    expect(changed).toHaveBeenCalled();
  });

  it('refuses a local path as repository URL without saving', () => {
    const updateTask = vi.fn(() => of('ok'));
    const fixture = render({}, { updateTask });
    const comp = fixture.componentInstance;
    comp.startEdit();
    comp.draftUrl.set('file:///etc');
    comp.saveUrl();
    expect(comp.urlError()).toContain('remote repository');
    expect(updateTask).not.toHaveBeenCalled();
  });

  it('hides the edit and test buttons from read-only members', () => {
    const el = render({ can_edit: false }, {}).nativeElement as HTMLElement;
    expect(el.textContent).not.toContain('Test access');
    expect(el.textContent).not.toContain('Edit URL');
  });
});
