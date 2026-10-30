/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { CheckGroupsComponent } from './check-groups.component';

const GROUPS = [
  { label: 'Build', items: [{ value: 'build.failed', label: 'failed' }, { value: 'build.completed', label: 'completed' }] },
  { label: 'Task', items: [{ value: 'task.star', label: 'star' }] },
];

function render(selected: string[], disabled = false) {
  const fixture = TestBed.createComponent(CheckGroupsComponent);
  fixture.componentRef.setInput('groups', GROUPS);
  fixture.componentRef.setInput('selected', selected);
  fixture.componentRef.setInput('disabled', disabled);
  fixture.detectChanges();
  const emitted: string[][] = [];
  fixture.componentInstance.selectedChange.subscribe((v) => emitted.push(v));
  const root = fixture.nativeElement as HTMLElement;
  return { root, emitted, boxes: Array.from(root.querySelectorAll<HTMLInputElement>('input[type=checkbox]')) };
}

describe('CheckGroupsComponent', () => {
  it('renders one titled box per group with the selection ticked', () => {
    const { root, boxes } = render(['task.star']);
    expect(Array.from(root.querySelectorAll('legend')).map((l) => l.textContent)).toEqual(['Build', 'Task']);
    expect(boxes.map((b) => b.checked)).toEqual([false, false, true]);
  });

  it('adds and removes values from the selection', () => {
    const { boxes, emitted } = render(['task.star']);
    boxes[0].click();
    boxes[2].click();
    expect(emitted).toEqual([['task.star', 'build.failed'], []]);
  });

  it('locks every box while disabled', () => {
    const { root } = render([], true);
    expect(Array.from(root.querySelectorAll('fieldset')).every((f) => f.disabled)).toBe(true);
  });
});
