/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;

pub(crate) struct PageLockCounts {
    page_size: usize,
    counts: HashMap<usize, usize>,
}

impl PageLockCounts {
    pub(crate) fn new(page_size: usize) -> Self {
        Self {
            page_size,
            counts: HashMap::new(),
        }
    }

    pub(crate) fn page_size(&self) -> usize {
        self.page_size
    }

    pub(crate) fn lock(&mut self, address: usize, len: usize) -> Vec<usize> {
        self.pages(address, len)
            .filter(|page| {
                let count = self.counts.entry(*page).or_insert(0);
                *count += 1;
                *count == 1
            })
            .collect()
    }

    pub(crate) fn unlock(&mut self, address: usize, len: usize) -> Vec<usize> {
        self.pages(address, len)
            .filter(|page| match self.counts.get_mut(page) {
                Some(1) => {
                    self.counts.remove(page);
                    true
                }
                Some(count) => {
                    *count -= 1;
                    false
                }
                None => false,
            })
            .collect()
    }

    fn pages(&self, address: usize, len: usize) -> impl Iterator<Item = usize> + use<> {
        let page_size = self.page_size;
        let first = address / page_size;
        let last = match len {
            0 => first,
            _ => (address + len - 1) / page_size + 1,
        };
        (first..last).map(move |page| page * page_size)
    }
}

#[cfg(test)]
mod tests {
    use super::PageLockCounts;

    const PAGE: usize = 4096;

    #[test]
    fn a_page_shared_by_two_secrets_stays_locked_until_both_are_dropped() {
        let mut counts = PageLockCounts::new(PAGE);
        assert_eq!(counts.lock(PAGE + 16, 32), [PAGE]);
        assert_eq!(counts.lock(PAGE + 64, 32), Vec::<usize>::new());

        assert_eq!(counts.unlock(PAGE + 16, 32), Vec::<usize>::new());
        assert_eq!(counts.unlock(PAGE + 64, 32), [PAGE]);
    }

    #[test]
    fn a_secret_crossing_a_page_boundary_locks_both_pages() {
        let mut counts = PageLockCounts::new(PAGE);
        assert_eq!(counts.lock(2 * PAGE - 8, 16), [PAGE, 2 * PAGE]);
        assert_eq!(counts.lock(2 * PAGE + 8, 8), Vec::<usize>::new());

        assert_eq!(counts.unlock(2 * PAGE - 8, 16), [PAGE]);
        assert_eq!(counts.unlock(2 * PAGE + 8, 8), [2 * PAGE]);
    }

    #[test]
    fn an_empty_secret_locks_no_page() {
        let mut counts = PageLockCounts::new(PAGE);
        assert_eq!(counts.lock(PAGE, 0), Vec::<usize>::new());
        assert_eq!(counts.unlock(PAGE, 0), Vec::<usize>::new());
    }
}
