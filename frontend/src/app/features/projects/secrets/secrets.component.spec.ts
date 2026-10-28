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
import { vi } from 'vitest';
import { SecretsComponent } from './secrets.component';
import { ProjectSecretsService } from '@core/services/project-secrets.service';
import { ProjectAccessService } from '@core/services/project-access.service';
import { AccessState } from '@core/models/access.model';
import { ProjectSecretSummary } from '@core/models';

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

function setup(access: AccessState = admin) {
  const service = {
    list: vi.fn(() => of<ProjectSecretSummary[]>(listed)),
    put: vi.fn(() => of(listed[0])),
    remove: vi.fn(() => of(undefined)),
  };
  TestBed.configureTestingModule({
    imports: [SecretsComponent],
    providers: [
      provideRouter([]),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: ActivatedRoute, useValue: { snapshot: { paramMap: convertToParamMap({ project: 'acme' }) } } },
      { provide: ProjectSecretsService, useValue: service },
      { provide: ProjectAccessService, useValue: { forProject: () => Promise.resolve(access) } },
    ],
  });
  const fixture = TestBed.createComponent(SecretsComponent);
  return { fixture, service };
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
  it('lists each secret name and never a value', async () => {
    const { fixture, service } = setup();
    service.list.mockReturnValue(of(listed.map((secret) => ({ ...secret, value: 's3cr3t-value' }))));
    await settled(fixture);
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('wandb-token');
    expect(text).toContain('lab-key');
    expect(text).not.toContain('s3cr3t-value');
  });

  it('puts a new secret from the Set secret dialog', async () => {
    const { fixture, service } = setup();
    await settled(fixture);
    buttonIn(fixture.nativeElement, 'Set secret')!.click();
    await settled(fixture);
    typeInto('secret-name', 'wandb-token');
    typeInto('secret-value', 'v');
    await settled(fixture);
    buttonIn(document.body, 'Save')!.click();
    await settled(fixture);
    expect(service.put).toHaveBeenCalledWith('acme', 'wandb-token', 'v');
  });

  it('disables Replace and Delete on a managed secret', async () => {
    const { fixture } = setup();
    await settled(fixture);
    const row = rowOf(fixture, 'lab-key');
    expect(buttonIn(row, 'Replace')!.disabled).toBe(true);
    expect(buttonIn(row, 'Delete')!.disabled).toBe(true);
    expect(buttonIn(rowOf(fixture, 'wandb-token'), 'Delete')!.disabled).toBe(false);
  });

  it('asks in a dialog before deleting a secret', async () => {
    const { fixture, service } = setup();
    await settled(fixture);
    buttonIn(rowOf(fixture, 'wandb-token'), 'Delete')!.click();
    await settled(fixture);
    expect(service.remove).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain('Delete secret wandb-token?');
    buttonIn(document.body.querySelector('.cdk-overlay-container')!, 'Delete')!.click();
    await settled(fixture);
    expect(service.remove).toHaveBeenCalledWith('acme', 'wandb-token');
  });

  it('hides Set secret without settings access', async () => {
    const { fixture } = setup({ managed: false, canEdit: false, canTrigger: false });
    await settled(fixture);
    expect(buttonIn(fixture.nativeElement, 'Set secret')).toBeNull();
  });
});
