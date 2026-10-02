/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Kuhn's augmenting-path matching is seating every member on a distinct worker, or reporting the
//! lack of a full seating.

pub fn kuhn(eligible: &[Vec<usize>]) -> Option<Vec<usize>> {
    let right = eligible.iter().flatten().max().map_or(0, |m| m + 1);
    let mut owner: Vec<Option<usize>> = vec![None; right];
    for left in 0..eligible.len() {
        let mut seen = vec![false; right];
        if !augment(left, eligible, &mut owner, &mut seen) {
            return None;
        }
    }

    let mut seats = vec![0; eligible.len()];
    for (right, left) in owner.iter().enumerate() {
        if let Some(left) = left {
            seats[*left] = right;
        }
    }

    Some(seats)
}

fn augment(
    left: usize,
    eligible: &[Vec<usize>],
    owner: &mut [Option<usize>],
    seen: &mut [bool],
) -> bool {
    for &right in &eligible[left] {
        if seen[right] {
            continue;
        }
        seen[right] = true;
        let free = match owner[right] {
            None => true,
            Some(other) => augment(other, eligible, owner, seen),
        };
        if free {
            owner[right] = Some(left);
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::kuhn;

    #[test]
    fn a_later_member_displaces_an_earlier_one_to_its_second_choice() {
        let seats = kuhn(&[vec![0, 1], vec![0]]).expect("full matching");

        assert_eq!(seats, vec![1, 0]);
    }

    #[test]
    fn two_members_sharing_one_worker_have_no_matching() {
        assert_eq!(kuhn(&[vec![0], vec![0]]), None);
    }

    #[test]
    fn a_member_without_workers_has_no_matching() {
        assert_eq!(kuhn(&[vec![0], vec![]]), None);
    }

    #[test]
    fn the_first_preference_wins_when_nothing_competes() {
        assert_eq!(kuhn(&[vec![2, 0], vec![1]]), Some(vec![2, 1]));
    }
}
