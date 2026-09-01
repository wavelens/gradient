/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { BadgeComponent } from './badge.component';

@Component({
  standalone: true,
  imports: [BadgeComponent],
  template: `<gr-badge severity="success">Completed</gr-badge><gr-badge>Managed</gr-badge>`,
})
class Host {}

describe('gr-badge', () => {
  async function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    return (fixture.nativeElement as HTMLElement).querySelectorAll('gr-badge');
  }

  it('projects its content', async () => {
    expect((await render())[0].textContent).toContain('Completed');
  });

  it('applies the severity class', async () => {
    expect((await render())[0].querySelector('.badge.is-success')).not.toBeNull();
  });

  it('defaults to neutral', async () => {
    expect((await render())[1].querySelector('.badge.is-neutral')).not.toBeNull();
  });
});
