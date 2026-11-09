/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { AuthService } from '@core/services/auth.service';
import { CreateCacheButtonComponent } from './create-cache-button.component';

@Component({
  selector: 'test-host',
  standalone: true,
  imports: [CreateCacheButtonComponent],
  template: `<app-create-cache-button (create)="opened = opened + 1" />`,
})
class HostComponent {
  opened = 0;
}

function setup(canCreateCache: boolean) {
  TestBed.configureTestingModule({
    providers: [{ provide: AuthService, useValue: { user: signal({ can_create_cache: canCreateCache }) } }],
  });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  const host: HTMLElement = fixture.nativeElement;
  const button = host.querySelector('button')!;
  const press = () => {
    button.click();
    fixture.detectChanges();
  };
  const hover = () => {
    button.dispatchEvent(new Event('mouseenter'));
    fixture.detectChanges();
  };
  return { host: fixture.componentInstance, button, press, hover };
}

describe('CreateCacheButtonComponent', () => {
  it('opens the dialog while another cache fits', () => {
    const { host, button, press } = setup(true);
    expect(button.getAttribute('aria-disabled')).toBeNull();
    press();
    expect(host.opened).toBe(1);
  });

  it('stays visible and focusable at the cache limit and explains why on hover', () => {
    const { host, button, press, hover } = setup(false);
    expect(button.textContent).toContain('Create Cache');
    expect(button.disabled).toBe(false);
    expect(button.getAttribute('aria-disabled')).toBe('true');
    press();
    expect(host.opened).toBe(0);
    hover();
    expect(document.body.textContent).toContain('maximum number of caches');
  });
});
