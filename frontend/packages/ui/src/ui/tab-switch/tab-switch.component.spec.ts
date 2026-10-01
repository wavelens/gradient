/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { TestBed } from '@angular/core/testing';
import { TabSwitchComponent } from './tab-switch.component';

@Component({
  standalone: true,
  imports: [TabSwitchComponent, FormsModule],
  template: `
    <gr-tab-switch
      ariaLabel="Time window"
      [options]="windows"
      optionLabel="label"
      optionValue="value"
      [ngModel]="window()"
      (ngModelChange)="window.set($event)"
      name="window"
    />
  `,
})
class HostComponent {
  window = signal('hours');
  windows = [
    { label: 'Minutes', value: 'minutes' },
    { label: 'Hours', value: 'hours' },
    { label: 'Days', value: 'days' },
  ];
}

async function render() {
  TestBed.configureTestingModule({ imports: [HostComponent] });
  const fixture = TestBed.createComponent(HostComponent);
  fixture.detectChanges();
  await fixture.whenStable();
  fixture.detectChanges();
  const root = () => fixture.nativeElement as HTMLElement;
  const tabs = () => [...root().querySelectorAll('.tab-switch__tab')] as HTMLButtonElement[];
  return { fixture, root, tabs };
}

describe('gr-tab-switch', () => {
  it('renders one tab per option', async () => {
    const { tabs } = await render();
    expect(tabs().map((t) => t.textContent?.trim())).toEqual(['Minutes', 'Hours', 'Days']);
  });

  it('marks the selected option', async () => {
    const { tabs } = await render();
    const selected = tabs().filter((t) => t.classList.contains('is-selected'));
    expect(selected).toHaveLength(1);
    expect(selected[0].textContent?.trim()).toBe('Hours');
  });

  it('writes the picked value back', async () => {
    const { fixture, tabs } = await render();
    tabs()[2].click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.window()).toBe('days');
  });

  it('exposes the group and its state to assistive tech', async () => {
    const { root, tabs } = await render();
    const group = root().querySelector('[role=tablist]');
    expect(group?.getAttribute('aria-label')).toBe('Time window');
    const selected = tabs().find((t) => t.classList.contains('is-selected'))!;
    expect(selected.getAttribute('aria-selected')).toBe('true');
    expect(tabs()[0].getAttribute('aria-selected')).toBe('false');
    expect(tabs()[0].getAttribute('role')).toBe('tab');
  });

  it('honours a disabled state pushed in through the form API', async () => {
    const fixture = TestBed.createComponent(TabSwitchComponent);
    fixture.componentRef.setInput('options', [{ label: 'A', value: 'a' }]);
    fixture.detectChanges();
    await fixture.whenStable();
    fixture.componentInstance.setDisabledState(true);
    fixture.detectChanges();
    await fixture.whenStable();
    const tab = (fixture.nativeElement as HTMLElement).querySelector('button') as HTMLButtonElement;
    expect(tab.disabled).toBe(true);
  });
});
