/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { ActionFormComponent } from './action-form.component';
import { ConfigService } from '@core/services/config.service';
import { ActionResponse, CreateActionRequest } from '@core/models';

function createFixture(smtpEnabled: boolean): ComponentFixture<ActionFormComponent> {
  TestBed.configureTestingModule({
    imports: [ActionFormComponent],
    providers: [provideRouter([]), { provide: ConfigService, useValue: { smtpEnabled } }],
  });
  const fixture = TestBed.createComponent(ActionFormComponent);
  fixture.componentRef.setInput('open', true);
  fixture.componentRef.setInput('gitHostConnectionsThatCanReportCommitStatus', []);
  return fixture;
}

const existing: ActionResponse = {
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
    c.onTypeChange('open_pr');
    expect(c.url()).toBe('');
    expect(c.tokenValue()).toBe('');
  });

  it('hides Send Mail option when smtp is disabled', () => {
    const fixture = createFixture(false);
    fixture.detectChanges();
    const opts = fixture.componentInstance.typeOptions().map((o) => o.value);
    expect(opts).not.toContain('send_mail');
    expect(opts).toContain('send_web_request');
  });

  it('generateToken populates a gat_-prefixed string of reasonable length', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    fixture.componentInstance.generateToken();
    const v = fixture.componentInstance.tokenValue();
    expect(v.startsWith('gat_')).toBe(true);
    expect(v.length).toBeGreaterThan(20);
  });

  it('moves focus to the first invalid field after an invalid submit', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    Element.prototype.scrollIntoView = vi.fn();
    fixture.componentInstance.onSubmit();
    TestBed.tick();
    expect(document.activeElement?.id).toBe('action-name');
  });

  it('hard-wires events to empty for open_pr (fires on the verify gate, not events)', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.onTypeChange('open_pr');
    c.gitHostConnectionId.set('int-1');
    c.name.set('updater');
    c.events.set(['build.completed']);
    let emitted: any;
    c.saved.subscribe((r) => (emitted = r));
    c.onSubmit();
    expect(emitted.events).toEqual([]);
  });

  it('says a flake lock pull request needs tracked flake inputs and links to them', () => {
    const fixture = createFixture(true);
    fixture.componentRef.setInput('project', 'acme');
    fixture.componentRef.setInput('task', 'web');
    fixture.detectChanges();
    fixture.componentInstance.onTypeChange('open_pr');
    fixture.detectChanges();
    const hint = document.body.querySelector('[data-testid="flake-override-hint"]');
    expect(hint?.textContent).toContain('Force update using the URL declared in flake.nix');
    expect(hint?.querySelector('a')?.getAttribute('href')).toBe('/project/acme/task/web/flake-inputs');
  });

  it('starts empty on the next open after a create', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.name.set('Webhook');
    c.onTypeChange('send_web_request');
    c.url.set('https://example.com/hook');
    fixture.componentRef.setInput('open', false);
    fixture.detectChanges();
    fixture.componentRef.setInput('open', true);
    fixture.detectChanges();
    expect(c.name()).toBe('');
    expect(c.url()).toBe('');
    expect(c.type()).toBe('send_mail');
  });

  it('stays open on Escape and ignores a second submit while busy', () => {
    const fixture = createFixture(true);
    fixture.componentRef.setInput('busy', true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    const emitted: unknown[] = [];
    c.saved.subscribe((r) => emitted.push(r));
    const panel = document.querySelector('.gr-dialog') as HTMLElement;
    panel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', keyCode: 27, bubbles: true }));
    Array.from(panel.querySelectorAll('button')).find((b) => b.textContent?.includes('Create'))!.click();
    fixture.detectChanges();
    expect(c.visible()).toBe(true);
    expect(panel.getAttribute('aria-busy')).toBe('true');
    expect(emitted).toEqual([]);
  });

  it('renders the submit error inside the dialog', () => {
    const fixture = createFixture(true);
    fixture.componentRef.setInput('open', true);
    fixture.componentRef.setInput('error', 'Git host connection not found');
    fixture.detectChanges();
    expect(document.body.querySelector('.gr-dialog gr-message-banner')?.textContent).toContain('Git host connection not found');
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

  it('emits a Matrix config with the access token', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.name.set('Matrix');
    c.onTypeChange('send_matrix_message');
    c.homeserver.set(' https://matrix.example.org ');
    c.roomId.set('!ops:example.org');
    c.secretValue.set('syt_abc');
    c.events.set(['build.failed']);
    let emitted: CreateActionRequest | undefined;
    c.saved.subscribe((r) => (emitted = r as CreateActionRequest));
    c.onSubmit();
    expect(emitted!.config).toEqual({
      type: 'send_matrix_message',
      homeserver: 'https://matrix.example.org',
      room_id: '!ops:example.org',
      access_token: 'syt_abc',
    });
  });

  it('omits a blank Slack webhook URL when editing', () => {
    const fixture = createFixture(true);
    fixture.componentRef.setInput('mode', 'edit');
    fixture.componentRef.setInput('existing', {
      ...existing,
      action_type: 'send_slack_message',
      config: { type: 'send_slack_message' },
    });
    fixture.detectChanges();
    const c = fixture.componentInstance;
    let emitted: any;
    c.saved.subscribe((r) => (emitted = r));
    c.onSubmit();
    expect(emitted.config).toEqual({ type: 'send_slack_message' });
  });

  it('submits start_evaluation with the picked task and its events', () => {
    const fixture = createFixture(true);
    fixture.componentRef.setInput('tasks', [{ id: 't-deploy', name: 'deploy' }]);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.name.set('evaluate-deploy');
    c.onTypeChange('start_evaluation');
    c.startTask.set('t-deploy');
    c.events.set(['exec_job.completed']);
    let emitted: CreateActionRequest | undefined;
    c.saved.subscribe((r) => (emitted = r as CreateActionRequest));
    c.onSubmit();
    expect(emitted!.config).toEqual({ type: 'start_evaluation', task: 't-deploy' });
    expect(emitted!.events).toEqual(['exec_job.completed']);
  });

  it('names each task to evaluate by display name and slug', () => {
    const fixture = createFixture(true);
    fixture.componentRef.setInput('tasks', [{ id: 't-1', name: 'deploy', display_name: 'Deploy Prod' }]);
    fixture.detectChanges();
    expect(fixture.componentInstance.taskOptions()).toEqual([{ label: 'Deploy Prod (deploy)', value: 't-1' }]);
  });

  it('offers the exec tree generator and sends it without the flake lock fields', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    expect(c.generatorOptions.map((o) => o.value)).toContain('exec_tree');
    c.name.set('exec-pr');
    c.onTypeChange('open_pr');
    c.gitHostConnectionId.set('int-1');
    c.onGeneratorChange('exec_tree');
    let emitted: CreateActionRequest | undefined;
    c.saved.subscribe((r) => (emitted = r as CreateActionRequest));
    c.onSubmit();
    expect(emitted!.config).toEqual({
      type: 'open_pr',
      git_host_connection_id: 'int-1',
      generator: 'exec_tree',
      branch_pattern: 'gradient/{entry_point}',
      update_existing: true,
    });
    expect(emitted!.events).toEqual([]);
  });

  it('offers Matrix and Slack without SMTP', () => {
    const fixture = createFixture(false);
    fixture.detectChanges();
    const opts = fixture.componentInstance.typeOptions().map((o) => o.value);
    expect(opts).toContain('send_matrix_message');
    expect(opts).toContain('send_slack_message');
  });
  it('adds a team as team:<name> to the recipients', () => {
    const fixture = TestBed.createComponent(ActionFormComponent);
    fixture.componentRef.setInput('teams', ['platform']);
    fixture.detectChanges();
    const component = fixture.componentInstance;
    component.recipientsRaw.set('ops@example.com');
    component.addTeamRecipient('platform');
    expect(component.recipientsRaw()).toBe('ops@example.com, team:platform');
  });
});

describe('ActionFormComponent - validation', () => {
  function submitAndCollect(fixture: ComponentFixture<ActionFormComponent>) {
    const emitted: unknown[] = [];
    fixture.componentInstance.saved.subscribe((r) => emitted.push(r));
    fixture.componentInstance.onSubmit();
    fixture.detectChanges();
    const errors = Array.from(document.querySelectorAll('.gr-dialog .field-error')).map((e) => e.textContent?.trim());
    return { emitted, errors };
  }

  it('names the missing name and recipients and emits nothing', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const { emitted, errors } = submitAndCollect(fixture);
    expect(errors).toEqual(['Enter a name for the action.', 'Enter at least one recipient.', 'Select at least one event.']);
    expect(emitted).toEqual([]);
  });

  it('refuses recipients that are neither email addresses nor teams', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.name.set('Mail');
    c.recipientsRaw.set('ops@example.com, ops, team:platform');
    c.events.set(['build.failed']);
    const { emitted, errors } = submitAndCollect(fixture);
    expect(errors).toEqual(['Not an email address or team:<name> entry: ops']);
    expect(emitted).toEqual([]);
  });

  it('refuses a web request URL that is not http or https', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.name.set('Webhook');
    c.onTypeChange('send_web_request');
    c.url.set('not-a-url');
    c.events.set(['build.failed']);
    const { emitted, errors } = submitAndCollect(fixture);
    expect(errors).toEqual([expect.stringContaining('https://')]);
    expect(emitted).toEqual([]);
  });

  it('refuses a Matrix room ID without the leading !', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.name.set('Matrix');
    c.onTypeChange('send_matrix_message');
    c.homeserver.set('https://matrix.example.org');
    c.roomId.set('#ops:example.org');
    c.secretValue.set('syt_abc');
    c.events.set(['build.failed']);
    const { emitted, errors } = submitAndCollect(fixture);
    expect(errors).toEqual([expect.stringContaining('Start the room ID with !')]);
    expect(emitted).toEqual([]);
  });

  it('asks for the {input} placeholder with Per Input granularity', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const c = fixture.componentInstance;
    c.name.set('updater');
    c.onTypeChange('open_pr');
    c.gitHostConnectionId.set('int-1');
    c.prGranularity.set('per_input');
    const { emitted, errors } = submitAndCollect(fixture);
    expect(errors).toEqual([expect.stringContaining('{input}')]);
    expect(emitted).toEqual([]);
  });

  function shownErrors(): (string | undefined)[] {
    return Array.from(document.querySelectorAll('.gr-dialog .field-error')).map((e) => e.textContent?.trim());
  }

  it('shows no error when the user leaves an empty field without typing', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    const name = document.querySelector<HTMLInputElement>('#action-name')!;
    name.dispatchEvent(new Event('blur'));
    fixture.detectChanges();
    expect(shownErrors()).toEqual([]);
    name.value = 'x';
    name.dispatchEvent(new Event('input'));
    name.dispatchEvent(new Event('blur'));
    name.value = '';
    name.dispatchEvent(new Event('input'));
    name.dispatchEvent(new Event('blur'));
    fixture.detectChanges();
    expect(shownErrors()).toEqual(['Enter a name for the action.']);
  });

  it('hides the errors of a failed submit after the type changes', () => {
    const fixture = createFixture(true);
    fixture.detectChanges();
    expect(submitAndCollect(fixture).errors.length).toBeGreaterThan(0);
    fixture.componentInstance.onTypeChange('start_evaluation');
    fixture.detectChanges();
    expect(shownErrors()).toEqual([]);
  });
});
