/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';
import { Observable, of, throwError } from 'rxjs';
import { CommandPaletteComponent } from './command-palette.component';
import { CommandPaletteService } from './command-palette.service';
import { SearchService } from '@core/services/search.service';
import { SearchHit } from '@core/models';

const HITS: SearchHit[] = [
  { kind: 'task', label: 'hosts', sublabel: 'infra', route: '/project/infra/task/hosts', starred: true },
  { kind: 'nar', label: 'hello-2.12.1', sublabel: 'main', route: '/caches/main/nars?hash=abc', starred: false },
];

function render(result: () => Observable<SearchHit[]> = () => of(HITS)) {
  const search = vi.fn<(q: string) => Observable<SearchHit[]>>(result);
  TestBed.configureTestingModule({
    imports: [CommandPaletteComponent],
    providers: [provideRouter([]), { provide: SearchService, useValue: { search } }],
  });
  const f = TestBed.createComponent(CommandPaletteComponent);
  f.detectChanges();
  return { f, search, root: f.nativeElement as HTMLElement };
}

function press(key: string, init: KeyboardEventInit = {}): void {
  document.dispatchEvent(new KeyboardEvent('keydown', { key, ...init }));
}

describe('CommandPaletteComponent', () => {
  afterEach(() => vi.useRealTimers());

  it('opens on / from anywhere outside a text field', () => {
    const { f, root } = render();
    press('/');
    f.detectChanges();
    expect(root.querySelector('input')).not.toBeNull();
  });

  it('leaves / to a focused text field and ignores Ctrl+K', () => {
    render();
    const palette = TestBed.inject(CommandPaletteService);
    const field = document.body.appendChild(document.createElement('textarea'));
    field.dispatchEvent(new KeyboardEvent('keydown', { key: '/', bubbles: true }));
    press('k', { ctrlKey: true });
    field.remove();
    expect(palette.isOpen()).toBe(false);
  });

  it('closes on Escape', () => {
    const { f, root } = render();
    const palette = TestBed.inject(CommandPaletteService);
    palette.open();
    press('Escape');
    f.detectChanges();
    expect(palette.isOpen()).toBe(false);
    expect(root.querySelector('input')).toBeNull();
  });

  it('moves through hits with Ctrl+J and Ctrl+K', () => {
    const { f, root } = render();
    TestBed.inject(CommandPaletteService).open();
    f.detectChanges();
    const input = root.querySelector('input')!;
    const selected = () => root.querySelector('[aria-selected="true"]')!.id;
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'j', ctrlKey: true }));
    f.detectChanges();
    expect(selected()).toBe('palette-hit-1');
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'k', ctrlKey: true }));
    f.detectChanges();
    expect(selected()).toBe('palette-hit-0');
  });

  it('searches as you type and opens the chosen hit with Enter', () => {
    vi.useFakeTimers();
    const { f, search, root } = render();
    TestBed.inject(CommandPaletteService).open();
    f.detectChanges();
    const input = root.querySelector('input')!;
    input.value = 'hel';
    input.dispatchEvent(new Event('input'));
    vi.advanceTimersByTime(200);
    f.detectChanges();
    expect(search).toHaveBeenLastCalledWith('hel');
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown' }));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter' }));
    expect(navigate).toHaveBeenCalledWith('/caches/main/nars?hash=abc');
    expect(TestBed.inject(CommandPaletteService).isOpen()).toBe(false);
  });

  it('debounces typing into a single request', () => {
    vi.useFakeTimers();
    const { f, search, root } = render();
    TestBed.inject(CommandPaletteService).open();
    f.detectChanges();
    const input = root.querySelector('input')!;
    for (const q of ['h', 'he', 'hel']) {
      input.value = q;
      input.dispatchEvent(new Event('input'));
      vi.advanceTimersByTime(50);
    }
    vi.advanceTimersByTime(200);
    expect(search.mock.calls.map(([q]) => q)).toEqual(['', 'hel']);
  });

  it('loads starred items on open', () => {
    const { f, search } = render();
    TestBed.inject(CommandPaletteService).open();
    f.detectChanges();
    expect(search).toHaveBeenCalledWith('');
  });

  it('renders hits as a listbox inside a modal dialog', () => {
    const { f, root } = render();
    TestBed.inject(CommandPaletteService).open();
    f.detectChanges();
    const dialog = root.querySelector('[role="dialog"]')!;
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    const options = root.querySelectorAll('[role="listbox"] [role="option"]');
    expect(options.length).toBe(2);
    expect(options[0].getAttribute('aria-selected')).toBe('true');
    expect(document.activeElement).toBe(root.querySelector('input'));
  });

  it('navigates when a hit is clicked', () => {
    const { f, root } = render();
    TestBed.inject(CommandPaletteService).open();
    f.detectChanges();
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigateByUrl').mockResolvedValue(true);
    root.querySelectorAll<HTMLElement>('[role="option"]')[0].click();
    expect(navigate).toHaveBeenCalledWith('/project/infra/task/hosts');
    expect(TestBed.inject(CommandPaletteService).isOpen()).toBe(false);
  });

  it('shows a failed search inline', () => {
    const { f, root } = render(() => throwError(() => new Error('boom')));
    TestBed.inject(CommandPaletteService).open();
    f.detectChanges();
    expect(root.querySelector('[role="alert"]')?.textContent).toContain('Search failed');
    expect(root.querySelectorAll('[role="option"]').length).toBe(0);
  });
});
