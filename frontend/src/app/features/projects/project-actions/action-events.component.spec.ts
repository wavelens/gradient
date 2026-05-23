/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActionEventsComponent } from './action-events.component';

describe('ActionEventsComponent', () => {
  let fixture: ComponentFixture<ActionEventsComponent>;
  let component: ActionEventsComponent;

  beforeEach(async () => {
    await TestBed.configureTestingModule({ imports: [ActionEventsComponent] }).compileComponents();
    fixture = TestBed.createComponent(ActionEventsComponent);
    component = fixture.componentInstance;
    fixture.componentRef.setInput('selected', []);
    fixture.detectChanges();
  });

  it('groups events by namespace', () => {
    expect(component.grouped().map(g => g.group)).toEqual(['Evaluation', 'Build']);
  });

  it('toggling emits updated selection', () => {
    let emitted: string[] = [];
    component.selectedChange.subscribe((v: string[]) => { emitted = v; });
    component.toggle('build.failed', true);
    expect(emitted).toEqual(['build.failed']);
  });
});
