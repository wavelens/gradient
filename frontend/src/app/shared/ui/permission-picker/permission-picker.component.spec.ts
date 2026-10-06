/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { PermissionPickerComponent } from './permission-picker.component';

@Component({
  standalone: true,
  imports: [PermissionPickerComponent],
  template: `<app-permission-picker [permissions]="permissions" [(selection)]="selection" />`,
})
class Host {
  permissions = [
    { id: 'viewProject', mutating: false },
    { id: 'deleteProject', mutating: true },
  ];
  selection = signal<Record<string, boolean>>({ viewProject: true });
}

describe('PermissionPickerComponent', () => {
  function render() {
    const fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    return { fixture, root: fixture.nativeElement as HTMLElement };
  }

  it('labels each permission and marks whether it changes anything', () => {
    const { root } = render();
    const rows = Array.from(root.querySelectorAll('.permission-row')).map((r) => r.textContent?.replace(/\s+/g, ' ').trim());
    expect(rows).toEqual(['view project read-only', 'delete project mutating']);
  });

  it('hands back a new selection when a permission is ticked', async () => {
    const { fixture, root } = render();
    const before = fixture.componentInstance.selection();
    (root.querySelector('#perm-deleteProject') as HTMLInputElement).click();
    fixture.detectChanges();
    await fixture.whenStable();
    expect(fixture.componentInstance.selection()).toEqual({ viewProject: true, deleteProject: true });
    expect(before).toEqual({ viewProject: true });
  });
});
