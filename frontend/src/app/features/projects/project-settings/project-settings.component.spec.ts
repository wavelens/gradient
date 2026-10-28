/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { of } from 'rxjs';
import { ProjectSettingsComponent } from './project-settings.component';
import { ProjectsService } from '@core/services/projects.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { ConfigService } from '@core/services/config.service';
import { AccessState } from '@core/models/access.model';
import { ProjectResponse, SopsRecipientResponse } from '@core/models';
import { ProjectSecretsService } from '@core/services/project-secrets.service';

function projectFor(access: AccessState): ProjectResponse {
  return {
    id: 'o',
    name: 'acme',
    display_name: 'Acme',
    description: '',
    public: false,
    hide_build_requests: false,
    exec_enabled: false,
    managed: access.managed,
    created_by: 'u1',
    created_at: '2026-01-01T00:00:00',
    github_app_available: false,
    role: access.canEdit ? 'Admin' : 'View',
  };
}

function findByText(root: HTMLElement, text: string): HTMLElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

const ed25519Recipient: SopsRecipientResponse = {
  age: 'age1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq',
  ssh: 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEd25519 acme',
};

function setup(
  access: AccessState,
  execEnabled = false,
  recipient: SopsRecipientResponse = ed25519Recipient,
): ComponentFixture<ProjectSettingsComponent> {
  TestBed.configureTestingModule({
    imports: [ProjectSettingsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      {
        provide: ActivatedRoute,
        useValue: { snapshot: { paramMap: convertToParamMap({ project: 'acme' }) } },
      },
      {
        provide: ProjectsService,
        useValue: {
          getProject: () => of(projectFor(access)),
          getSSHKey: () => of(recipient.ssh),
        },
      },
      { provide: ProjectAccessService, useValue: { forProject: () => Promise.resolve(access) } },
      { provide: ProjectSecretsService, useValue: { sopsRecipient: () => of(recipient) } },
    ],
  });
  TestBed.inject(ConfigService).execEnabled = execEnabled;
  const fixture = TestBed.createComponent(ProjectSettingsComponent);
  fixture.detectChanges();
  return fixture;
}

async function settled(fixture: ComponentFixture<ProjectSettingsComponent>) {
  await fixture.whenStable();
  fixture.detectChanges();
}

describe('ProjectSettingsComponent - access gating', () => {
  it('hides Save / Delete under read-only access', async () => {
    const fixture = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(findByText(fixture.nativeElement, 'save changes')).toBeNull();
    expect(findByText(fixture.nativeElement, 'delete project')).toBeNull();
  });

  it('shows but disables Save / Delete under state-managed access', async () => {
    const fixture = setup({ managed: true, canEdit: true, canTrigger: true });
    await settled(fixture);
    const save = findByText(fixture.nativeElement, 'save changes') as HTMLButtonElement | null;
    const del = findByText(fixture.nativeElement, 'delete project') as HTMLButtonElement | null;
    expect(save).not.toBeNull();
    expect(save!.disabled).toBe(true);
    expect(del).not.toBeNull();
    expect(del!.disabled).toBe(true);
  });
});

describe('ProjectSettingsComponent - exec switch', () => {
  const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };

  it('hides the exec switch while the instance has exec jobs off', async () => {
    const fixture = setup(admin);
    await settled(fixture);
    expect((fixture.nativeElement as HTMLElement).querySelector('#exec-enabled')).toBeNull();
  });

  it('shows the exec switch once the instance allows exec jobs', async () => {
    const fixture = setup(admin, true);
    await settled(fixture);
    expect((fixture.nativeElement as HTMLElement).querySelector('#exec-enabled')).not.toBeNull();
  });
});

describe('ProjectSettingsComponent - sops recipient', () => {
  const member: AccessState = { managed: false, canEdit: false, canTrigger: true };

  function recipientField(fixture: ComponentFixture<ProjectSettingsComponent>): HTMLInputElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector('gr-copy-field #sops-recipient');
  }

  it('shows the age recipient of an ed25519 key in a copy field', async () => {
    const fixture = setup(member, true);
    await settled(fixture);
    expect(recipientField(fixture)?.value).toBe(ed25519Recipient.age);
    expect(fixture.nativeElement.textContent).not.toContain('sops 3.10 or newer');
  });

  it('falls back to the SSH key with a hint when there is no age recipient', async () => {
    const fixture = setup(member, true, { age: null, ssh: 'ssh-rsa AAAAB3NzaC1yc2E acme' });
    await settled(fixture);
    expect(recipientField(fixture)?.value).toBe('ssh-rsa AAAAB3NzaC1yc2E acme');
    expect(fixture.nativeElement.textContent).toContain(
      'Use this SSH key as the age recipient (sops 3.10 or newer)',
    );
  });
});
