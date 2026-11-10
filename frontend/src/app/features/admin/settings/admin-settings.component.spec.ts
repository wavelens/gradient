/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { TestBed } from '@angular/core/testing';
import { of } from 'rxjs';
import { AdminSettingsComponent } from './admin-settings.component';
import { AdminService } from '@core/services/admin.service';
import { EffectiveConfigOption } from '@core/models';

const OPTIONS: EffectiveConfigOption[] = [
  { group: 'Database', field: 'url', flag: 'database-url', env: 'GRADIENT_DATABASE_URL', value: null, secret: true, source: 'env' },
  { group: 'Evaluation', field: 'max_keep', flag: 'eval-max-keep', env: 'GRADIENT_EVAL_MAX_KEEP', value: '30', secret: false, source: 'flag' },
  { group: 'Evaluation', field: 'timeout', flag: 'eval-timeout', env: null, value: null, secret: false, source: 'unset' },
];

describe('AdminSettingsComponent', () => {
  it('groups the options and names where each value came from, hiding secrets', () => {
    TestBed.configureTestingModule({
      imports: [AdminSettingsComponent],
      providers: [{ provide: AdminService, useValue: { getServerOptionsWithValuesAndSources: () => of(OPTIONS) } }],
    });
    const fixture = TestBed.createComponent(AdminSettingsComponent);
    fixture.detectChanges();
    const root = fixture.nativeElement as HTMLElement;
    expect(Array.from(root.querySelectorAll('h2')).map((h) => h.textContent)).toEqual(['Database', 'Evaluation']);
    const rows = root.querySelectorAll('tbody tr');
    expect(rows[0].textContent).toContain('--database-url');
    expect(rows[0].textContent).toContain('Hidden');
    expect(rows[0].textContent).toContain('Environment variables');
    expect(rows[1].textContent).toContain('30');
    expect(rows[1].textContent).toContain('Flags for the command line');
    expect(rows[2].textContent).toContain('Not set');
    TestBed.resetTestingModule();
  });
});
