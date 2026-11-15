/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { vi } from 'vitest';
import type { YearlyRecapResponse } from '@core/models';
import { NotifyService } from '@core/services/notify.service';
import { YearlyRecapAutoOpenService } from './yearly-recap-auto-open.service';
import { YearlyRecapFullscreenSlideshowComponent } from './yearly-recap-fullscreen-slideshow.component';

const RECAP: YearlyRecapResponse = {
  year: 2026,
  created_at: '2026-12-01T00:00:00',
  data_since: '2026-01-01',
  opened: false,
  statistics: {
    totals: { evaluations: 10, builds: 20, build_time_ms: 0, exec_jobs: 0 },
    cache: { nars_served: 5, bytes_served: 0, builds_substituted: 0 },
  },
};

function renderOpenFullscreenRecapSlideshowWithStubService() {
  const stub = { recap: signal(RECAP), open: signal(true), closeRecapSlideShowAndMarkOpenedOnServer: vi.fn() };
  const notify = { success: vi.fn(), failure: vi.fn() };
  TestBed.configureTestingModule({
    imports: [YearlyRecapFullscreenSlideshowComponent],
    providers: [
      { provide: YearlyRecapAutoOpenService, useValue: stub },
      { provide: NotifyService, useValue: notify },
    ],
  });
  const fixture = TestBed.createComponent(YearlyRecapFullscreenSlideshowComponent);
  fixture.detectChanges();
  const root = fixture.nativeElement as HTMLElement;
  const colorScheme = () => root.querySelector('.slide')?.getAttribute('data-color-scheme');
  const press = (key: string) => {
    root.querySelector('[role="dialog"]')?.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
    fixture.detectChanges();
  };
  return { fixture, root, stub, colorScheme, press };
}

describe('YearlyRecapFullscreenSlideshowComponent', () => {
  it('Arrow keys move between slides, Escape to close', () => {
    const { colorScheme, press, stub } = renderOpenFullscreenRecapSlideshowWithStubService();
    expect(colorScheme()).toBe('intro');
    press('ArrowRight');
    expect(colorScheme()).toBe('totals');
    press('ArrowLeft');
    expect(colorScheme()).toBe('intro');
    press('Escape');
    expect(stub.closeRecapSlideShowAndMarkOpenedOnServer).toHaveBeenCalledOnce();
  });

  it('Backdrop click closes only on the last slide and only outside the card', () => {
    const { root, colorScheme, press, stub } = renderOpenFullscreenRecapSlideshowWithStubService();
    const backdrop = root.querySelector<HTMLElement>('[role="dialog"]')!;
    backdrop.click();
    expect(stub.closeRecapSlideShowAndMarkOpenedOnServer).not.toHaveBeenCalled();
    press('ArrowRight');
    press('ArrowRight');
    press('ArrowRight');
    expect(colorScheme()).toBe('summary');
    root.querySelector<HTMLElement>('.story')!.click();
    expect(stub.closeRecapSlideShowAndMarkOpenedOnServer).not.toHaveBeenCalled();
    backdrop.click();
    expect(stub.closeRecapSlideShowAndMarkOpenedOnServer).toHaveBeenCalledOnce();
  });

  it('Tap left third to go back, other taps advance, long presses only pause', () => {
    const { root, fixture, colorScheme } = renderOpenFullscreenRecapSlideshowWithStubService();
    const stage = root.querySelector<HTMLElement>('.stage')!;
    vi.spyOn(stage, 'getBoundingClientRect').mockReturnValue({ left: 0, width: 300 } as DOMRect);
    const press = (clientX: number, heldMilliseconds: number) => {
      const down = new PointerEvent('pointerdown', { clientX });
      const up = new PointerEvent('pointerup', { clientX });
      Object.defineProperty(up, 'timeStamp', { value: down.timeStamp + heldMilliseconds });
      stage.dispatchEvent(down);
      fixture.detectChanges();
      expect(root.querySelector('.story')?.classList.contains('paused')).toBe(true);
      stage.dispatchEvent(up);
      fixture.detectChanges();
    };
    press(250, 50);
    expect(colorScheme()).toBe('totals');
    press(50, 50);
    expect(colorScheme()).toBe('intro');
    press(250, 800);
    expect(colorScheme()).toBe('intro');
    expect(root.querySelector('.story')?.classList.contains('paused')).toBe(false);
  });

  it('End of the progress bar advances to the next slide and the last slide has no timer', () => {
    const { root, fixture, colorScheme } = renderOpenFullscreenRecapSlideshowWithStubService();
    const finishProgressBar = () => {
      root.querySelector('.fill')?.dispatchEvent(new Event('animationend'));
      fixture.detectChanges();
    };
    finishProgressBar();
    expect(colorScheme()).toBe('totals');
    finishProgressBar();
    finishProgressBar();
    expect(colorScheme()).toBe('summary');
    expect(root.querySelector('.fill')?.classList.contains('timed')).toBe(false);
    expect(Array.from(root.querySelectorAll('button')).some((b) => b.textContent?.includes('Download image'))).toBe(true);
    expect(root.querySelector('.slide-credit')?.textContent).toBe('NixOS Gradient CI Made OpenSource by Wavelens');
  });

  it('Pause button stops the progress bar', () => {
    const { root, fixture } = renderOpenFullscreenRecapSlideshowWithStubService();
    root.querySelector<HTMLButtonElement>('button[aria-label="Pause slideshow"]')!.click();
    fixture.detectChanges();
    expect(root.querySelector('.story')?.classList.contains('paused')).toBe(true);
    expect(root.querySelector('button[aria-label="Resume slideshow"]')).not.toBeNull();
  });

  it('Keys keep working on the last slide because focus stays inside the dialog', () => {
    const { root, fixture, colorScheme, stub } = renderOpenFullscreenRecapSlideshowWithStubService();
    const pressOnDocument = (key: string) => {
      (document.activeElement ?? document.body).dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }));
      fixture.detectChanges();
    };
    const previous = root.querySelector<HTMLButtonElement>('button[aria-label="Previous slide"]')!;
    expect(previous.disabled).toBe(false);
    expect(previous.getAttribute('aria-disabled')).toBe('true');
    previous.focus();
    previous.click();
    fixture.detectChanges();
    expect(colorScheme()).toBe('intro');
    expect(document.activeElement).toBe(previous);

    pressOnDocument('ArrowRight');
    pressOnDocument('ArrowRight');
    root.querySelector<HTMLButtonElement>('button[aria-label="Pause slideshow"]')!.focus();
    pressOnDocument('ArrowRight');
    expect(colorScheme()).toBe('summary');
    expect(document.activeElement?.getAttribute('aria-label')).toBe('Close Gradient Wrapped');

    document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true }));
    fixture.detectChanges();
    expect(colorScheme()).toBe('cache');
    document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(stub.closeRecapSlideShowAndMarkOpenedOnServer).toHaveBeenCalledOnce();
  });

  it('Keys do nothing while the slideshow is closed', () => {
    const { fixture, stub } = renderOpenFullscreenRecapSlideshowWithStubService();
    stub.open.set(false);
    fixture.detectChanges();
    document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(stub.closeRecapSlideShowAndMarkOpenedOnServer).not.toHaveBeenCalled();
  });
});
