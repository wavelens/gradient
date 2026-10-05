/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { AllowedCapabilitiesComponent } from './allowed-capabilities.component';

function render(editable: boolean) {
  TestBed.configureTestingModule({ imports: [AllowedCapabilitiesComponent] });
  const fixture = TestBed.createComponent(AllowedCapabilitiesComponent);
  fixture.componentRef.setInput('value', { enable_fetch: false, enable_eval: true, enable_build: true });
  fixture.componentRef.setInput('editable', editable);
  fixture.detectChanges();
  return fixture;
}

describe('AllowedCapabilitiesComponent', () => {
  it('lists what a worker is allowed and marks the rest as off', () => {
    const root = render(false).nativeElement as HTMLElement;

    expect(root.textContent).toContain('Allowed:');
    expect(root.querySelectorAll('button').length).toBe(0);
    const off = Array.from(root.querySelectorAll('[data-allowed="false"]')).map((el) => el.textContent?.trim());
    expect(off).toEqual(['fetch']);
  });

  it('toggles only the clicked capability when editable', () => {
    const fixture = render(true);
    const fetch = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('button')).find(
      (b) => b.textContent?.includes('fetch'),
    )!;

    fetch.click();
    fixture.detectChanges();

    expect(fixture.componentInstance.value()).toEqual({ enable_fetch: true, enable_eval: true, enable_build: true });
    expect(fetch.getAttribute('aria-pressed')).toBe('true');
  });
});
