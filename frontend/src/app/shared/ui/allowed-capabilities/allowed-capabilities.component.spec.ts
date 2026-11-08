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
  const pills = (root: HTMLElement, selector = '.allowed-pill') =>
    Array.from(root.querySelectorAll(selector)).map((el) => el.textContent?.trim());

  it('lists only the allowed capabilities, in the order network, secrets, exec, eval, build', () => {
    const root = render(false).nativeElement as HTMLElement;

    expect(root.textContent).toContain('Allowed:');
    expect(root.querySelectorAll('button').length).toBe(0);
    expect(pills(root)).toEqual(['network', 'eval', 'build']);
  });

  it('says none when no capability is allowed', () => {
    const fixture = render(false);
    fixture.componentRef.setInput('value', {
      enable_network: false,
      enable_secrets: false,
      enable_exec: false,
      enable_eval: false,
      enable_build: false,
    });
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(pills(root)).toEqual([]);
    expect(root.textContent).toContain('none');
  });

  it("grey out an allowed capability that the connected worker doesn't report", () => {
    const fixture = render(false);
    fixture.componentRef.setInput('workerReportedCapabilities', { network: true, secrets: true, exec: true, eval: false, build: true });
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(pills(root, '.allowed-pill--unsupported-by-worker')).toEqual(['eval']);
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
