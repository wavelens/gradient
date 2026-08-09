/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { ActionFormComponent } from './action-form.component';
import { ConfigService } from '@core/services/config.service';
import { Action, CreateActionRequest } from '@core/models';

function createFixture(smtpEnabled: boolean): ComponentFixture<ActionFormComponent> {
  TestBed.configureTestingModule({
    imports: [ActionFormComponent],
    providers: [{ provide: ConfigService, useValue: { smtpEnabled } }],
  });
  const fixture = TestBed.createComponent(ActionFormComponent);
  fixture.componentRef.setInput('open', true);
  fixture.componentRef.setInput('outboundIntegrations', []);
  return fixture;
}

const existing: Action = {
  id: 'a-1',
  name: 'Notify Ops',
  action_type: 'send_mail',
  config: { type: 'send_mail', recipients: ['ops@example.com'], subject_template: '[hit] {{event}}' },
  events: ['build.failed'],
  active: true,
  last_fired_at: null,
  created_by: 'u',
  created_at: '2026-05-01T00:00:00Z',
  updated_at: '2026-05-01T00:00:00Z',
};

describe('ActionFormComponent', () => {
  it('initialises edit-mode form from existing action', () => {
    const fixture = createFixture(true);
    fixture.componentRef.setInput('mode', 'edit');
    fixture.componentRef.setInput('existing', existing);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    expect(c.name()).toBe('Notify Ops');
    expect(c.type()).toBe('send_mail');
    expect(c.recipientsRaw()).toBe('ops@example.com');
    expect(c.subjectTemplate()).toBe('[hit] {{event}}');
    expect(c.events()).toEqual(['build.failed']);
    expect(c.active()).toBe(true);
    expect(c.typeRadioDisabled()).toBe(true);
  });

  it('clears irrelevant fields when switching action type', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.recipientsRaw.set('a@b');
    c.subjectTemplate.set('s');
    c.onTypeChange('send_web_request');
    expect(c.recipientsRaw()).toBe('');
    expect(c.subjectTemplate()).toBe('');
    expect(c.url()).toBe('');
    c.url.set('https://x');
    c.tokenValue.set('t');
    c.onTypeChange('forge_status_report');
    expect(c.url()).toBe('');
    expect(c.tokenValue()).toBe('');
    expect(c.events().length).toBeGreaterThan(0);
  });

  it('hides Send Mail option when smtp is disabled', () => {
    const fixture = createFixture(false);
    fixture.detectChanges();
    const opts = fixture.componentInstance.typeOptions().map((o) => o.value);
    expect(opts).not.toContain('send_mail');
    expect(opts).toContain('send_web_request');
    expect(opts).toContain('forge_status_report');
  });

  it('generateToken populates a gat_-prefixed string of reasonable length', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    fixture.componentInstance.generateToken();
    const v = fixture.componentInstance.tokenValue();
    expect(v.startsWith('gat_')).toBe(true);
    expect(v.length).toBeGreaterThan(20);
  });

  it('sends empty events for forge_status_report', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.type.set('forge_status_report');
    c.integrationId.set('int-1');
    c.name.set('report');
    let emitted: any;
    c.saved.subscribe((r) => (emitted = r));
    c.onSubmit();
    expect(emitted.events).toEqual([]);
  });

  it('hard-wires events to empty for open_pr (fires on the verify gate, not events)', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.onTypeChange('open_pr');
    c.integrationId.set('int-1');
    c.name.set('updater');
    c.events.set(['build.completed']);
    expect(c.eventsHardwired()).toBe(true);
    let emitted: any;
    c.saved.subscribe((r) => (emitted = r));
    c.onSubmit();
    expect(emitted.events).toEqual([]);
  });

  it('renders the submit error inside the dialog', () => {
    const fixture = createFixture(true);
    fixture.componentRef.setInput('open', true);
    fixture.componentRef.setInput('error', 'Integration not found');
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Integration not found');
  });

  it('emits a CreateActionRequest with the correct discriminated union on submit', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.name.set('Webhook');
    c.onTypeChange('send_web_request');
    c.url.set('https://example.com/hook');
    c.tokenValue.set('gat_abc');
    c.events.set(['build.completed']);
    let emitted: CreateActionRequest | undefined;
    c.saved.subscribe((r) => (emitted = r as CreateActionRequest));
    c.onSubmit();
    expect(emitted).toBeDefined();
    expect(emitted!.name).toBe('Webhook');
    expect(emitted!.config).toEqual({
      type: 'send_web_request',
      url: 'https://example.com/hook',
      token: 'gat_abc',
    });
    expect(emitted!.events).toEqual(['build.completed']);
    expect(emitted!.active).toBe(true);
  });
});
