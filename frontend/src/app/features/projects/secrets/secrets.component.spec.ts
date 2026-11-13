/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActivatedRoute, provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EMPTY, Observable, of, throwError } from 'rxjs';
import { ConfirmRequest, ConfirmService } from '@shared/confirm';
import { vi } from 'vitest';
import { SecretsComponent } from './secrets.component';
import { ProjectSecretsService } from '@core/services/project-secrets.service';
import { projectRouteStub } from '@core/resolvers/access-route.testing';
import { AccessState } from '@core/models/access.model';
import { ProjectResponse, ProjectSecretSummary, SopsRecipientResponse } from '@core/models';
import { docsUrl } from '@core/docs';
import { ApiError } from '@core/services/api.service';

const admin: AccessState = { managed: false, canEdit: true, canTrigger: true };

const listed: ProjectSecretSummary[] = [
  { name: 'wandb-token', updated_at: '2026-10-01T00:00:00', updated_by: 'alice', managed: false },
  { name: 'lab-key', updated_at: '2026-10-02T00:00:00', updated_by: null, managed: true },
];

function buttonIn(root: ParentNode, text: string): HTMLButtonElement | null {
  const target = text.toLowerCase();
  return (Array.from(root.querySelectorAll('button')) as HTMLButtonElement[]).find(
    (el) => (el.textContent ?? '').trim().toLowerCase().includes(target),
  ) ?? null;
}

function rowOf(fixture: ComponentFixture<SecretsComponent>, name: string): HTMLElement {
  const rows = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('gr-row')) as HTMLElement[];
  return rows.find((row) => row.textContent?.includes(name))!;
}

const ed25519Recipient: SopsRecipientResponse = {
  age: 'age1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq',
  ssh: 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEd25519 acme',
};

function recipientField(fixture: ComponentFixture<SecretsComponent>): HTMLInputElement {
  return (fixture.nativeElement as HTMLElement).querySelector('gr-copy-field #sops-recipient') as HTMLInputElement;
}

function setup(access: AccessState = admin, project: Partial<ProjectResponse> = {}) {
  const service = {
    sopsRecipient: vi.fn(() => of<SopsRecipientResponse>(ed25519Recipient)),
    list: vi.fn(() => of<ProjectSecretSummary[]>(listed)),
    put: vi.fn(() => of(listed[0])),
    remove: vi.fn(() => of(undefined)),
  };
  const confirm = vi.fn((request: ConfirmRequest): Observable<unknown> => request.action());
  TestBed.configureTestingModule({
    imports: [SecretsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useFactory: () => projectRouteStub('acme', access, project) },
      { provide: ProjectSecretsService, useValue: service },
      { provide: ConfirmService, useValue: { confirm } },
    ],
  });
  const fixture = TestBed.createComponent(SecretsComponent);
  return { fixture, service, confirm };
}

async function settled(fixture: ComponentFixture<SecretsComponent>) {
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
}

function typeInto(id: string, value: string) {
  const field = document.body.querySelector(`#${id}`) as HTMLInputElement | HTMLTextAreaElement;
  field.value = value;
  field.dispatchEvent(new Event('input'));
}

describe('SecretsComponent', () => {
  it('explains the missing permission to a viewer without listing secrets', async () => {
    const { fixture, service } = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(service.list).not.toHaveBeenCalled();
    expect(text).toContain('No access');
    expect(text).toContain('Only the admins of this project can view this.');
    expect(text).not.toContain('Set secret');
  });

  it('tells a visitor of a public project that only members see secrets', async () => {
    const { fixture } = setup({ managed: false, canEdit: false, canTrigger: false }, { role: null, permissions: [] });
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('Only the members of this project can view this.');
    expect(text).not.toContain('permission');
  });

  it('shows a refused load as one error without the empty state', async () => {
    const { fixture, service } = setup();
    service.list.mockReturnValue(throwError(() => ({ status: 403, message: 'You do not have permission to perform this action.' })));
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('No access');
    expect(text).not.toContain('No secrets');
  });

  it('loads the secrets again on Try Again', async () => {
    const { fixture, service } = setup();
    service.list.mockReturnValueOnce(throwError(() => new Error('down')));
    await settled(fixture);
    (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>('.gr-load-state__error button')!.click();
    await settled(fixture);
    expect(service.list).toHaveBeenCalledTimes(2);
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('wandb-token');
  });

  it('lists each secret name and never a value', async () => {
    const { fixture, service } = setup();
    service.list.mockReturnValue(of(listed.map((secret) => ({ ...secret, value: 's3cr3t-value' }))));
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('wandb-token');
    expect(text).toContain('lab-key');
    expect(text).not.toContain('s3cr3t-value');
  });

  it('puts a new secret from the Set secret dialog without asking', async () => {
    const { fixture, service, confirm } = setup();
    await settled(fixture);
    buttonIn(fixture.nativeElement, 'Set secret')!.click();
    await settled(fixture);
    typeInto('secret-name', 'new-token');
    typeInto('secret-value', 'v');
    await settled(fixture);
    buttonIn(document.body, 'Save')!.click();
    await settled(fixture);
    expect(confirm).not.toHaveBeenCalled();
    expect(service.put).toHaveBeenCalledWith('acme', 'new-token', 'v');
  });

  it('names the length as the problem with a long name of valid characters', async () => {
    const { fixture } = setup();
    await settled(fixture);
    buttonIn(fixture.nativeElement, 'Set secret')!.click();
    await settled(fixture);
    typeInto('secret-name', 'a'.repeat(65));
    await settled(fixture);
    expect(document.body.textContent).toContain('Keep the name to 64 characters or fewer.');
    expect(document.body.textContent).not.toContain('Start with a letter or digit');

    typeInto('secret-name', '-token');
    await settled(fixture);
    expect(document.body.textContent).toContain('Start with a letter or digit');
    expect(fixture.componentInstance.canSave()).toBe(false);
  });

  it('asks before Set secret overwrites an existing secret of the same name', async () => {
    const { fixture, service, confirm } = setup();
    await settled(fixture);
    buttonIn(fixture.nativeElement, 'Set secret')!.click();
    await settled(fixture);
    typeInto('secret-name', 'wandb-token');
    typeInto('secret-value', 'v');
    await settled(fixture);
    buttonIn(document.body, 'Save')!.click();
    await settled(fixture);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ target: 'wandb-token', confirmLabel: 'Replace' }));
    expect(service.put).toHaveBeenCalledWith('acme', 'wandb-token', 'v');
    expect(fixture.componentInstance.formOpen()).toBe(false);
  });

  it('keeps the existing secret when the overwrite is cancelled', async () => {
    const { fixture, service, confirm } = setup();
    confirm.mockImplementation(() => EMPTY);
    await settled(fixture);
    buttonIn(fixture.nativeElement, 'Set secret')!.click();
    await settled(fixture);
    typeInto('secret-name', 'wandb-token');
    typeInto('secret-value', 'v');
    await settled(fixture);
    buttonIn(document.body, 'Save')!.click();
    await settled(fixture);
    expect(service.put).not.toHaveBeenCalled();
    expect(fixture.componentInstance.formOpen()).toBe(true);
  });

  it('shows a lock instead of Replace and Delete on a managed secret', async () => {
    const { fixture } = setup();
    await settled(fixture);
    const row = rowOf(fixture, 'lab-key');
    expect(row.querySelector('app-server-configuration-managed-lock-button')).not.toBeNull();
    expect(buttonIn(row, 'Replace')).toBeNull();
    expect(buttonIn(row, 'Delete')).toBeNull();
    expect(buttonIn(rowOf(fixture, 'wandb-token'), 'Delete')!.getAttribute('aria-disabled')).toBeNull();
  });

  it('asks before deleting a secret, naming it', async () => {
    const { fixture, service, confirm } = setup();
    await settled(fixture);
    buttonIn(rowOf(fixture, 'wandb-token'), 'Delete')!.click();
    await settled(fixture);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ title: 'Delete secret', target: 'wandb-token' }));
    expect(service.remove).toHaveBeenCalledWith('acme', 'wandb-token');
  });

  it('sends no delete when the confirmation is cancelled', async () => {
    const { fixture, service, confirm } = setup();
    confirm.mockImplementation(() => EMPTY);
    await settled(fixture);
    buttonIn(rowOf(fixture, 'wandb-token'), 'Delete')!.click();
    await settled(fixture);
    expect(service.remove).not.toHaveBeenCalled();
  });

  it('keeps a failed save inside the Set secret dialog', async () => {
    const { fixture, service } = setup();
    service.put.mockReturnValue(throwError(() => new ApiError('Value is too long', 400)));
    await settled(fixture);
    buttonIn(fixture.nativeElement, 'Set secret')!.click();
    await settled(fixture);
    typeInto('secret-name', 'new-token');
    typeInto('secret-value', 'v');
    await settled(fixture);
    buttonIn(document.body, 'Save')!.click();
    await settled(fixture);
    expect(document.body.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('Value is too long');
  });

  it('hides Set secret without settings access', async () => {
    const { fixture } = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(buttonIn(fixture.nativeElement, 'Set secret')).toBeNull();
  });

  it('shows the age recipient of an ed25519 key in a copy field', async () => {
    const { fixture } = setup();
    await settled(fixture);
    expect(recipientField(fixture).value).toBe(ed25519Recipient.age);
    expect(fixture.nativeElement.textContent).not.toContain('sops 3.10 or newer');
  });

  it('falls back to the SSH key with a hint when there is no age recipient', async () => {
    const { fixture, service } = setup();
    service.sopsRecipient.mockReturnValue(of({ age: null, ssh: 'ssh-rsa AAAAB3NzaC1yc2E acme' }));
    await settled(fixture);
    expect(recipientField(fixture).value).toBe('ssh-rsa AAAAB3NzaC1yc2E acme');
    expect(fixture.nativeElement.textContent).toContain('Use this SSH key as the age recipient (sops 3.10 or newer)');
  });

  it('links the secrets docs page', async () => {
    const { fixture } = setup();
    await settled(fixture);
    const links = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('a'));
    expect(links.map((link) => link.getAttribute('href'))).toContain(docsUrl('exec/secrets/'));
  });
});
