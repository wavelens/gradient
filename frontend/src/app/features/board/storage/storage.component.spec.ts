import { ComponentFixture, TestBed } from '@angular/core/testing';
import { of, throwError } from 'rxjs';
import { vi } from 'vitest';
import { BoardStorageComponent } from './storage.component';
import { BoardService, BoardStorage } from '@core/services/board.service';

const EMPTY: BoardStorage = {
  granularity: 'minute',
  op_latency: [],
  op_errors: [],
  lane_fill: [],
  send_stalls: [],
  serve_queue: [],
  serve_failures: [],
};

describe('BoardStorageComponent', () => {
  let fixture: ComponentFixture<BoardStorageComponent>;
  const getStorage = vi.fn((_hours: number) => of(EMPTY));

  beforeEach(async () => {
    getStorage.mockClear();
    await TestBed.configureTestingModule({
      imports: [BoardStorageComponent],
      providers: [{ provide: BoardService, useValue: { getStorage } }],
    }).compileComponents();
    fixture = TestBed.createComponent(BoardStorageComponent);
    fixture.detectChanges();
  });

  it('loads a 6 hour window and renders four charts', () => {
    expect(getStorage).toHaveBeenCalledWith(6);
    expect(fixture.nativeElement.querySelectorAll('gr-metric-chart').length).toBe(4);
  });

  it('refetches when the window changes', () => {
    const button = [...fixture.nativeElement.querySelectorAll('button.window')].find(
      (b: HTMLButtonElement) => b.textContent?.trim() === '24h'
    ) as HTMLButtonElement;
    button.click();
    fixture.detectChanges();

    expect(getStorage).toHaveBeenLastCalledWith(24);
  });

  it('puts every chart on one axis spanning the whole window', () => {
    const errors = {
      label: 'get/error',
      points: [{ bucket_start: new Date(Math.floor(Date.now() / 60_000) * 60_000).toISOString(), count: 3, avg: 1, max: 1 }],
    };
    getStorage.mockReturnValueOnce(of({ ...EMPTY, op_errors: [errors] }));
    fixture.componentInstance.select(1);
    const c = fixture.componentInstance;

    expect(c.categories()).toHaveLength(60);
    expect(c.errors()[0].data).toHaveLength(60);
    expect(c.errors()[0].data.at(-1)).toBe(3);
    expect(c.errors()[0].data.slice(0, -1).every((v) => v === 0)).toBe(true);
  });

  it('labels buckets by granularity', () => {
    const c = fixture.componentInstance;
    const at = Date.parse('2026-09-29T15:04:00Z');
    const labelAs = (granularity: BoardStorage['granularity']) => {
      c.view.set({ stats: { ...EMPTY, granularity }, buckets: [at] });

      return c.categories();
    };

    expect(labelAs('minute')).toEqual(['15:04']);
    expect(labelAs('hour')).toEqual(['09-29 15:04']);
    expect(labelAs('day')).toEqual(['09-29']);
  });

  it('keeps polling after a failed fetch', () => {
    getStorage.mockReturnValueOnce(throwError(() => new Error('down')));
    fixture.componentInstance.select(1);
    fixture.componentInstance.select(24);

    expect(getStorage).toHaveBeenLastCalledWith(24);
  });
});
