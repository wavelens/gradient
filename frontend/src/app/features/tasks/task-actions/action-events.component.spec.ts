/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { ActionEventsComponent } from './action-events.component';
import { EventsService } from '@core/services/events.service';

const catalog = [
  { name: 'evaluation.failed', durable: true },
  { name: 'build.failed', durable: true },
  { name: 'build.status_changed', durable: false },
  { name: 'task.star', durable: true },
  { name: 'proto.client.*', durable: false },
];

describe('ActionEventsComponent', () => {
  let fixture: ComponentFixture<ActionEventsComponent>;
  let component: ActionEventsComponent;

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [ActionEventsComponent],
      providers: [{ provide: EventsService, useValue: { catalog$: of(catalog) } }],
    }).compileComponents();
    fixture = TestBed.createComponent(ActionEventsComponent);
    component = fixture.componentInstance;
    fixture.componentRef.setInput('selected', []);
    fixture.detectChanges();
  });

  it('groups durable catalog events by namespace', () => {
    expect(component.grouped().map(g => g.group)).toEqual(['Evaluation', 'Build', 'Task']);
  });

  it('limits the groups to the requested families', () => {
    fixture.componentRef.setInput('families', ['build']);
    expect(component.grouped().map(g => g.group)).toEqual(['Build']);
  });

  it('toggling emits updated selection', () => {
    let emitted: string[] = [];
    component.selectedChange.subscribe((v: string[]) => { emitted = v; });
    component.toggle('build.failed', true);
    expect(emitted).toEqual(['build.failed']);
  });
});
