/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { SettingsSectionComponent } from './settings-section.component';

@Component({
  standalone: true,
  imports: [SettingsSectionComponent],
  template: `<gr-settings-section title="General" description="Names."><p class="body">x</p></gr-settings-section>`,
})
class Host {}

describe('gr-settings-section', () => {
  it('renders the header when a title is given', async () => {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('h2')?.textContent).toContain('General');
    expect(root.querySelector('.settings-section__description')?.textContent).toContain('Names.');
  });

  it('projects its body', async () => {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.settings-section__body .body')).not.toBeNull();
  });

  it('omits the header entirely without a title', async () => {
    const fixture = TestBed.createComponent(SettingsSectionComponent);
    fixture.detectChanges();
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).querySelector('.settings-section__header')).toBeNull();
  });
  it('is not a danger zone by default', async () => {
    const fixture = TestBed.createComponent(SettingsSectionComponent);
    fixture.componentRef.setInput('title', 'General');
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.settings-section')?.classList).not.toContain('is-danger');
  });

  it('marks a danger zone so its heading and card read as destructive', async () => {
    const fixture = TestBed.createComponent(SettingsSectionComponent);
    fixture.componentRef.setInput('title', 'Danger Zone');
    fixture.componentRef.setInput('danger', true);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    expect(root.querySelector('.settings-section')?.classList).toContain('is-danger');
  });
  it('projects a header action beside the title', async () => {
    TestBed.configureTestingModule({ imports: [ActionHost] });
    const fixture = TestBed.createComponent(ActionHost);
    fixture.detectChanges();
    await fixture.whenStable();
    const root = fixture.nativeElement as HTMLElement;
    const action = root.querySelector('.settings-section__actions button');
    expect(action?.textContent).toContain('Add Member');
  });
});

@Component({
  standalone: true,
  imports: [SettingsSectionComponent],
  template: `
    <gr-settings-section title="Members">
      <button slot="actions">Add Member</button>
      <p>body</p>
    </gr-settings-section>
  `,
})
class ActionHost {}

