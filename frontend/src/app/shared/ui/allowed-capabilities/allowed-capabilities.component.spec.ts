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
  fixture.componentRef.setInput('value', {
    enable_network: true,
    enable_secrets: false,
    enable_exec: false,
    enable_eval: true,
    enable_build: true,
  });
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
    expect(off).toEqual(['secrets', 'exec']);
  });

  it('lists the capabilities in the order network, secrets, exec, eval, build', () => {
    const root = render(false).nativeElement as HTMLElement;
    const labels = Array.from(root.querySelectorAll('[data-allowed]')).map((el) => el.textContent?.trim());
    expect(labels).toEqual(['network', 'secrets', 'exec', 'eval', 'build']);
  });

  it('toggles only the clicked capability when editable', () => {
    const fixture = render(true);
    const secrets = Array.from((fixture.nativeElement as HTMLElement).querySelectorAll('button')).find(
      (b) => b.textContent?.includes('secrets'),
    )!;

    secrets.click();
    fixture.detectChanges();

    expect(fixture.componentInstance.value()).toEqual({
      enable_network: true,
      enable_secrets: true,
      enable_exec: false,
      enable_eval: true,
      enable_build: true,
    });
    expect(secrets.getAttribute('aria-pressed')).toBe('true');
  });
});
