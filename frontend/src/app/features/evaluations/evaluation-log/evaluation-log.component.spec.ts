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
});
