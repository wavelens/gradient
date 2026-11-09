/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { removeErrorPageRouterStateFromHistory } from './access-denied';

function historyWith(state: unknown): History & { replaceState: ReturnType<typeof vi.fn> } {
  return { state, replaceState: vi.fn() } as unknown as History & { replaceState: ReturnType<typeof vi.fn> };
}

describe('removeErrorPageRouterStateFromHistory', () => {
  it('drops a restored error page so a reload retries the real page', () => {
    const history = historyWith({ ɵrouterUrl: '/error/500?from=%2Fproject%2Fa', navigationId: 3 });
    removeErrorPageRouterStateFromHistory(history);
    expect(history.replaceState).toHaveBeenCalledWith({ navigationId: 3 }, '');
  });

  it('keeps the state of any other page', () => {
    const history = historyWith({ ɵrouterUrl: '/project/a', navigationId: 3 });
    removeErrorPageRouterStateFromHistory(history);
    expect(history.replaceState).not.toHaveBeenCalled();
  });

  it('ignores an empty history state', () => {
    const history = historyWith(null);
    removeErrorPageRouterStateFromHistory(history);
    expect(history.replaceState).not.toHaveBeenCalled();
  });
});
