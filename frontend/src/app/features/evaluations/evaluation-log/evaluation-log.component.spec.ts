/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { EvaluationLogComponent } from './evaluation-log.component';
import { BuildItem } from '@core/services/evaluations.service';

function build(id: string, name: string, status = 'Completed'): BuildItem {
  return { id, name, status, has_artefacts: false, updated_at: '', build_time_ms: null };
}

function setup(): { fixture: ComponentFixture<EvaluationLogComponent>; cmp: EvaluationLogComponent } {
  TestBed.configureTestingModule({
    imports: [EvaluationLogComponent],
    providers: [provideRouter([]), provideHttpClient(), provideHttpClientTesting()],
  });
  const fixture = TestBed.createComponent(EvaluationLogComponent);
  return { fixture, cmp: fixture.componentInstance };
}

describe('EvaluationLogComponent', () => {
  // #341: the live line-count must update on the streaming (Building) render
  // path, identical to chunked (Completed) builds.
  describe('line count', () => {
    it('renderLog sets logLineCount from logLines', () => {
      const { cmp } = setup();
      (cmp as unknown as { logLines: string[] }).logLines = ['a', 'b', 'c'];
      (cmp as unknown as { renderLog: () => void }).renderLog();
      expect(cmp.logLineCount()).toBe(3);
    });
  });

  // #341: sidebar search filters the build list by name without disturbing the
  // status-sorted indices used for keyboard navigation.
  describe('sidebar search', () => {
    it('filters grouped builds by name, case-insensitively', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('a', '/nix/store/aaa-hello'), build('b', '/nix/store/bbb-world')]);
      cmp.sidebarSearchQuery.set('HELLO');
      const names = cmp.groupedBuilds().flatMap((g) => g.builds.map((x) => x.build.name));
      expect(names).toEqual(['/nix/store/aaa-hello']);
    });

    it('keeps every build when the query is empty', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('a', 'aaa'), build('b', 'bbb')]);
      cmp.sidebarSearchQuery.set('');
      const names = cmp.groupedBuilds().flatMap((g) => g.builds.map((x) => x.build.name));
      expect(names).toEqual(['aaa', 'bbb']);
    });

    it('preserves visibleBuilds index for matched builds (arrow-nav stays correct)', () => {
      const { cmp } = setup();
      cmp.visibleBuilds.set([build('a', 'aaa'), build('b', 'bbb'), build('c', 'ccc')]);
      cmp.sidebarSearchQuery.set('ccc');
      const indices = cmp.groupedBuilds().flatMap((g) => g.builds.map((x) => x.index));
      expect(indices).toEqual([2]);
    });
  });

  // The builds search bar is hidden until revealed via Ctrl/Cmd+F while the
  // sidebar holds focus, then dismissed with Escape (which also resets the filter).
  describe('sidebar search visibility', () => {
    const key = (init: KeyboardEventInit) => new KeyboardEvent('keydown', { cancelable: true, ...init });

    it('is closed by default', () => {
      const { cmp } = setup();
      expect(cmp.sidebarSearchOpen()).toBe(false);
    });

    it('opens on Ctrl+F while the sidebar holds focus, preventing default find', () => {
      const { cmp } = setup();
      cmp.setSidebarFocus(true);
      const ev = key({ key: 'f', ctrlKey: true });
      cmp.onKeydown(ev);
      expect(cmp.sidebarSearchOpen()).toBe(true);
      expect(ev.defaultPrevented).toBe(true);
    });

    it('stays closed on Ctrl+F when the sidebar is not focused', () => {
      const { cmp } = setup();
      cmp.onKeydown(key({ key: 'f', ctrlKey: true }));
      expect(cmp.sidebarSearchOpen()).toBe(false);
    });

    it('opens on "/" when not typing in a field', () => {
      const { cmp } = setup();
      cmp.onKeydown(key({ key: '/' }));
      expect(cmp.sidebarSearchOpen()).toBe(true);
    });

    it('ignores "/" typed inside an input', () => {
      const { cmp } = setup();
      const ev = key({ key: '/' });
      Object.defineProperty(ev, 'target', { value: document.createElement('input') });
      cmp.onKeydown(ev);
      expect(cmp.sidebarSearchOpen()).toBe(false);
    });

    it('Escape closes the bar and clears the query', () => {
      const { cmp } = setup();
      cmp.openSidebarSearch();
      cmp.sidebarSearchQuery.set('hello');
      cmp.onKeydown(key({ key: 'Escape' }));
      expect(cmp.sidebarSearchOpen()).toBe(false);
      expect(cmp.sidebarSearchQuery()).toBe('');
    });
  });
});
