//! Shuffle bag: every wallpaper is shown once per cycle, in random order,
//! before anything repeats. The order and position persist across restarts.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

const HISTORY_LEN: usize = 100;
/// Fraction of a cycle's tail that may not reappear at the head of the next.
const BOUNDARY_FRACTION: usize = 10;
const BOUNDARY_MAX: usize = 200;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Bag {
    order: Vec<String>,
    pos: usize,
    /// Shown items, oldest first. `cursor` points at the current one.
    history: Vec<String>,
    cursor: usize,
    pub favorites: HashSet<String>,
    pub banned: HashSet<String>,
    #[serde(default)]
    pub cycle: u64,
}

impl Bag {
    pub fn current(&self) -> Option<&str> {
        self.history.get(self.cursor).map(String::as_str)
    }

    pub fn remaining(&self) -> usize {
        self.order.len().saturating_sub(self.pos)
    }

    /// Reconcile the bag with the files that currently exist. Missing files are
    /// dropped; new ones are inserted at random spots in the unseen part, so they
    /// show up during this cycle rather than waiting for the next.
    pub fn sync(&mut self, library: &[String], rng: &mut fastrand::Rng) {
        let available: HashSet<&str> = library.iter().map(String::as_str).collect();

        let mut kept = Vec::with_capacity(self.order.len());
        let mut new_pos = 0;
        for (i, path) in self.order.drain(..).enumerate() {
            if available.contains(path.as_str()) {
                if i < self.pos {
                    new_pos += 1;
                }
                kept.push(path);
            }
        }
        self.order = kept;
        self.pos = new_pos;

        let known: HashSet<&str> = self.order.iter().map(String::as_str).collect();
        let fresh: Vec<String> = library
            .iter()
            .filter(|p| !known.contains(p.as_str()) && !self.banned.contains(*p))
            .cloned()
            .collect();
        for path in fresh {
            let at = rng.usize(self.pos..=self.order.len());
            self.order.insert(at, path);
        }

        self.history.retain(|p| available.contains(p.as_str()));
        self.cursor = self.cursor.min(self.history.len().saturating_sub(1));
        self.favorites.retain(|p| available.contains(p.as_str()));
    }

    /// Advance to the next wallpaper. Walks forward through history first if the
    /// user had stepped back with `previous`.
    pub fn next(
        &mut self,
        library: &[String],
        eligible: &dyn Fn(&str) -> bool,
        fav_weight: u32,
        rng: &mut fastrand::Rng,
    ) -> Option<String> {
        while self.cursor + 1 < self.history.len() {
            self.cursor += 1;
            let p = &self.history[self.cursor];
            if eligible(p) && !self.banned.contains(p) {
                return Some(p.clone());
            }
        }

        let mut refilled = false;
        loop {
            if self.pos >= self.order.len() {
                if refilled {
                    return None;
                }
                self.new_cycle(library, fav_weight, rng);
                refilled = true;
                if self.order.is_empty() {
                    return None;
                }
            }
            let path = self.order[self.pos].clone();
            self.pos += 1;
            if eligible(&path) && !self.banned.contains(&path) && self.current() != Some(&path) {
                self.push_history(path.clone());
                return Some(path);
            }
        }
    }

    /// What `next` would return, without changing anything. Used to prepare the
    /// next wallpaper ahead of time. Does not look past the end of the cycle.
    pub fn peek(&self, eligible: &dyn Fn(&str) -> bool) -> Option<&str> {
        if self.cursor + 1 < self.history.len() {
            return self.history[self.cursor + 1..]
                .iter()
                .find(|p| eligible(p) && !self.banned.contains(*p))
                .map(String::as_str);
        }
        self.order[self.pos.min(self.order.len())..]
            .iter()
            .find(|p| eligible(p) && !self.banned.contains(*p) && self.current() != Some(p.as_str()))
            .map(String::as_str)
    }

    /// What `previous` would return, without changing anything.
    pub fn peek_previous(&self, eligible: &dyn Fn(&str) -> bool) -> Option<&str> {
        self.history[..self.cursor.min(self.history.len())]
            .iter()
            .rev()
            .find(|p| eligible(p) && !self.banned.contains(*p))
            .map(String::as_str)
    }

    pub fn previous(&mut self, eligible: &dyn Fn(&str) -> bool) -> Option<String> {
        let mut c = self.cursor;
        while c > 0 {
            c -= 1;
            let p = &self.history[c];
            if eligible(p) && !self.banned.contains(p) {
                self.cursor = c;
                return Some(p.clone());
            }
        }
        None
    }

    pub fn toggle_favorite(&mut self) -> Option<bool> {
        let cur = self.current()?.to_owned();
        if self.favorites.remove(&cur) {
            Some(false)
        } else {
            self.favorites.insert(cur);
            Some(true)
        }
    }

    pub fn is_favorite(&self) -> bool {
        self.current().is_some_and(|c| self.favorites.contains(c))
    }

    pub fn ban_current(&mut self) -> Option<String> {
        let cur = self.current()?.to_owned();
        self.favorites.remove(&cur);
        self.banned.insert(cur.clone());
        Some(cur)
    }

    fn push_history(&mut self, path: String) {
        self.history.truncate(self.cursor + 1);
        self.history.push(path);
        if self.history.len() > HISTORY_LEN {
            let excess = self.history.len() - HISTORY_LEN;
            self.history.drain(..excess);
        }
        self.cursor = self.history.len() - 1;
    }

    fn new_cycle(&mut self, library: &[String], fav_weight: u32, rng: &mut fastrand::Rng) {
        let mut order: Vec<String> = library
            .iter()
            .filter(|p| !self.banned.contains(*p))
            .cloned()
            .collect();
        // Favorites get extra slots in the cycle.
        for _ in 1..fav_weight.max(1) {
            order.extend(
                library
                    .iter()
                    .filter(|p| self.favorites.contains(*p) && !self.banned.contains(*p))
                    .cloned(),
            );
        }
        rng.shuffle(&mut order);

        // Keep the end of the last cycle away from the start of this one.
        let n = order.len();
        let k = (n / BOUNDARY_FRACTION).min(BOUNDARY_MAX).min(self.history.len());
        if k > 0 && n > 2 * k {
            let recent: HashSet<&str> = self.history[self.history.len() - k..]
                .iter()
                .map(String::as_str)
                .collect();
            for i in 0..k {
                if recent.contains(order[i].as_str()) {
                    for _ in 0..16 {
                        let j = rng.usize(k..n);
                        if !recent.contains(order[j].as_str()) {
                            order.swap(i, j);
                            break;
                        }
                    }
                }
            }
        }

        self.order = order;
        self.pos = 0;
        self.cycle += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("/w/{i:05}.jpg")).collect()
    }

    fn all(_: &str) -> bool {
        true
    }

    #[test]
    fn no_repeats_within_a_cycle() {
        let library = lib(1000);
        let mut rng = fastrand::Rng::with_seed(1);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            let p = bag.next(&library, &all, 1, &mut rng).unwrap();
            assert!(seen.insert(p), "repeat inside one cycle");
        }
        assert_eq!(seen.len(), 1000);
    }

    #[test]
    fn cycle_boundary_avoids_recent() {
        let library = lib(500);
        let mut rng = fastrand::Rng::with_seed(7);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        let first: Vec<String> = (0..500).map(|_| bag.next(&library, &all, 1, &mut rng).unwrap()).collect();
        let tail: HashSet<&String> = first[450..].iter().collect();
        for _ in 0..50 {
            let p = bag.next(&library, &all, 1, &mut rng).unwrap();
            assert!(!tail.contains(&p), "item from end of last cycle reappeared immediately");
        }
    }

    #[test]
    fn new_files_join_current_cycle() {
        let mut library = lib(100);
        let mut rng = fastrand::Rng::with_seed(3);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        for _ in 0..50 {
            bag.next(&library, &all, 1, &mut rng);
        }
        library.push("/w/new.jpg".into());
        bag.sync(&library, &mut rng);
        let rest: Vec<String> = (0..51).map(|_| bag.next(&library, &all, 1, &mut rng).unwrap()).collect();
        assert!(rest.contains(&"/w/new.jpg".to_string()));
        assert_eq!(bag.cycle, 0, "should still be in the first cycle");
    }

    #[test]
    fn removed_files_are_skipped() {
        let mut library = lib(10);
        let mut rng = fastrand::Rng::with_seed(4);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        library.retain(|p| p != "/w/00003.jpg");
        bag.sync(&library, &mut rng);
        for _ in 0..30 {
            assert_ne!(bag.next(&library, &all, 1, &mut rng).unwrap(), "/w/00003.jpg");
        }
    }

    #[test]
    fn previous_then_next_replays_history() {
        let library = lib(20);
        let mut rng = fastrand::Rng::with_seed(5);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        let a = bag.next(&library, &all, 1, &mut rng).unwrap();
        let b = bag.next(&library, &all, 1, &mut rng).unwrap();
        assert_eq!(bag.previous(&all).unwrap(), a);
        assert_eq!(bag.peek(&all).unwrap(), b);
        assert_eq!(bag.next(&library, &all, 1, &mut rng).unwrap(), b);
    }

    #[test]
    fn peek_previous_matches_previous() {
        let library = lib(20);
        let mut rng = fastrand::Rng::with_seed(7);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        assert_eq!(bag.peek_previous(&all), None);
        for _ in 0..5 {
            bag.next(&library, &all, 1, &mut rng);
        }
        for _ in 0..5 {
            let peeked = bag.peek_previous(&all).map(str::to_owned);
            assert_eq!(peeked, bag.previous(&all));
        }
    }

    #[test]
    fn peek_matches_next() {
        let library = lib(50);
        let mut rng = fastrand::Rng::with_seed(6);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        bag.next(&library, &all, 1, &mut rng);
        for _ in 0..40 {
            let peeked = bag.peek(&all).map(str::to_owned);
            let got = bag.next(&library, &all, 1, &mut rng);
            assert_eq!(peeked, got);
        }
    }

    #[test]
    fn banned_never_shown() {
        let library = lib(5);
        let mut rng = fastrand::Rng::with_seed(8);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        let first = bag.next(&library, &all, 1, &mut rng).unwrap();
        bag.ban_current();
        for _ in 0..30 {
            assert_ne!(bag.next(&library, &all, 1, &mut rng).unwrap(), first);
        }
    }

    #[test]
    fn state_round_trips() {
        let library = lib(30);
        let mut rng = fastrand::Rng::with_seed(9);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        for _ in 0..10 {
            bag.next(&library, &all, 1, &mut rng);
        }
        let json = serde_json::to_string(&bag).unwrap();
        let mut restored: Bag = serde_json::from_str(&json).unwrap();
        restored.sync(&library, &mut rng);
        assert_eq!(restored.current(), bag.current());
        assert_eq!(restored.peek(&all), bag.peek(&all));
    }

    #[test]
    fn favorites_appear_more_often() {
        let library = lib(100);
        let mut rng = fastrand::Rng::with_seed(10);
        let mut bag = Bag::default();
        bag.sync(&library, &mut rng);
        bag.favorites.insert("/w/00042.jpg".into());
        let mut hits = 0;
        for _ in 0..2000 {
            if bag.next(&library, &all, 3, &mut rng).unwrap() == "/w/00042.jpg" {
                hits += 1;
            }
        }
        assert!(hits > 30, "favorite shown {hits} times in ~20 cycles");
    }
}
