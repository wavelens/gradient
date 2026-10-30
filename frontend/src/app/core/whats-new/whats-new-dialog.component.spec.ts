/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { WhatsNewDialogComponent } from './whats-new-dialog.component';
import { WhatsNewService } from './whats-new.service';
import type { ReleaseNotes } from './release-notes.generated';

const NOTES: ReleaseNotes[] = [
  {
    version: '2.1',
    pages: [
      { title: 'Dashboard', html: '<p>Stars</p>', image: { src: 'whats-new/2.1/dashboard.png', alt: 'Dashboard' } },
      { title: 'Exec Jobs', html: '<p>Workers</p>', icon: 'terminal' },
    ],
  },
  { version: '2.0', pages: [{ title: 'Caches', html: '<p>Create</p>' }] },
];

function setup() {
  const close = vi.fn();
  const open = signal(false);
  TestBed.configureTestingModule({
    providers: [{ provide: WhatsNewService, useValue: { open, notes: signal(NOTES), close } }],
  });
  const fixture = TestBed.createComponent(WhatsNewDialogComponent);
  open.set(true);
  fixture.detectChanges();
  const panel = () => document.querySelector('.gr-dialog') as HTMLElement;
  const button = (label: string) =>
    Array.from(panel().querySelectorAll<HTMLButtonElement>('button')).find((b) => b.textContent?.trim().startsWith(label));
  const click = (label: string) => {
    button(label)!.click();
    fixture.detectChanges();
  };
  return { panel, button, click, close, fixture };
}

describe('WhatsNewDialogComponent', () => {
  it('pages through every release, newest first, then closes', () => {
    const { panel, button, click, close } = setup();
    expect(panel().querySelector('h3')!.textContent).toBe('Dashboard');
    expect(panel().querySelector('img')!.getAttribute('src')).toBe('whats-new/2.1/dashboard.png');
    expect(button('Back')).toBeUndefined();
    click('Next');
    expect(panel().querySelector('h3')!.textContent).toBe('Exec Jobs');
    click('Next');
    expect(panel().textContent).toContain('Gradient 2.0');
    expect(button('Next')).toBeUndefined();
    click('Got it');
    expect(close).toHaveBeenCalled();
  });

  it('jumps to a page through its dot and back again', () => {
    const { panel, click, fixture } = setup();
    (panel().querySelectorAll<HTMLButtonElement>('.dot')[2]).click();
    fixture.detectChanges();
    expect(panel().querySelector('h3')!.textContent).toBe('Caches');
    click('Back');
    expect(panel().querySelector('h3')!.textContent).toBe('Exec Jobs');
  });

  it('turns the page on a horizontal swipe', () => {
    const { panel, fixture } = setup();
    const slides = panel().querySelector('.slides')!;
    slides.dispatchEvent(new PointerEvent('pointerdown', { clientX: 300 }));
    slides.dispatchEvent(new PointerEvent('pointerup', { clientX: 200 }));
    fixture.detectChanges();
    expect(panel().querySelector('h3')!.textContent).toBe('Exec Jobs');
  });
});
