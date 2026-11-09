/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { Subject, of, throwError } from 'rxjs';
import { CachesService } from '@core/services/caches.service';
import { ApiError } from '@core/services/api.service';
import { CreateCacheDialogComponent } from './create-cache-dialog.component';

@Component({
  selector: 'test-host',
  standalone: true,
  imports: [CreateCacheDialogComponent],
  template: `<app-create-cache-dialog [(visible)]="visible" (created)="created.push($event)" />`,
})
class HostComponent {
  visible = true;
  created: string[] = [];
}

async function setup(createCache: () => unknown, available = true) {
  TestBed.configureTestingModule({
    providers: [{ provide: CachesService, useValue: { createCache, checkCacheNameAvailable: () => of(available) } }],
  });
  const fixture = TestBed.createComponent(HostComponent);
  const settle = async () => {
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.detectChanges();
  };
  await settle();
  const type = async (selector: string, value: string) => {
    const input = document.querySelector<HTMLInputElement>(selector)!;
    input.value = value;
    input.dispatchEvent(new Event('input'));
    await new Promise((resolve) => setTimeout(resolve, 450));
    await settle();
  };
  const submit = async () => {
    Array.from(document.querySelectorAll<HTMLButtonElement>('button')).find((b) => b.textContent?.includes('Create'))!.click();
    await settle();
  };
  const pressEnter = async (selector: string) => {
    document.querySelector(selector)!.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }));
    await settle();
  };
  const pressEscape = async () => {
    document.querySelector('.gr-dialog')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    await settle();
  };
  return { host: fixture.componentInstance, type, submit, pressEnter, pressEscape };
}

describe('CreateCacheDialogComponent', () => {
  it('creates the cache, closes and reports its name', async () => {
    const createCache = vi.fn(() => of('id'));
    const { host, type, submit } = await setup(createCache);
    await type('#cache-display-name', 'Team Cache');
    await submit();
    expect(createCache).toHaveBeenCalledWith(expect.objectContaining({ name: 'team-cache', display_name: 'Team Cache', public: false }));
    expect(host.created).toEqual(['team-cache']);
    expect(host.visible).toBe(false);
  });

  it('stays open with the error when creation fails', async () => {
    const { host, type, submit } = await setup(() => throwError(() => new ApiError('Name taken', 409)));
    await type('#cache-display-name', 'Team Cache');
    await submit();
    expect(host.created).toEqual([]);
    expect(host.visible).toBe(true);
    expect(document.body.textContent).toContain('Name taken');
  });

  it('ignores Enter while the name is taken', async () => {
    const createCache = vi.fn(() => of('id'));
    const { host, type, pressEnter } = await setup(createCache, false);
    await type('#cache-display-name', 'Team Cache');
    await pressEnter('#cache-display-name');
    expect(createCache).not.toHaveBeenCalled();
    expect(host.visible).toBe(true);
  });

  it('ignores Enter while the name check is running', async () => {
    const createCache = vi.fn(() => of('id'));
    const { type, pressEnter } = await setup(createCache);
    const input = document.querySelector<HTMLInputElement>('#cache-display-name')!;
    input.value = 'Team Cache';
    input.dispatchEvent(new Event('input'));
    await pressEnter('#cache-display-name');
    expect(createCache).not.toHaveBeenCalled();
    await type('#cache-display-name', 'Team Cache');
  });

  it('stays open and sends one request while creation runs', async () => {
    const pending = new Subject<string>();
    const createCache = vi.fn(() => pending);
    const { host, type, submit, pressEscape } = await setup(createCache);
    await type('#cache-display-name', 'Team Cache');
    await submit();
    await submit();
    await pressEscape();
    expect(createCache).toHaveBeenCalledTimes(1);
    expect(host.visible).toBe(true);
    pending.next('id');
    pending.complete();
  });
});
