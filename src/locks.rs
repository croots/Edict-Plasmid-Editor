//! Locked regions: bases a running task (a BLAST search) depends on. Edits elsewhere
//! move a lock along; edits that would change its bases are refused. Locks live
//! only in memory and are never saved or logged.

use crate::model::Op;

/// `[start, end)`, or on a circular sequence `[start, len) + [0, end)` when
/// `start > end` (through the origin). Always at least one base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lock {
    /// The task holding the lock.
    pub id: u64,
    pub start: usize,
    pub end: usize,
}

impl Lock {
    /// A lock on `start..end` (wrapping when `start > end`), normalized for a
    /// sequence of `len` bases. None if it would cover nothing.
    pub fn new(id: u64, start: usize, end: usize, len: usize) -> Option<Lock> {
        let (start, end) = normalize(start.min(len), end.min(len), len);
        (start != end).then_some(Lock { id, start, end })
    }

    pub fn wraps(&self) -> bool {
        self.start > self.end
    }

    /// Whether base `i` is locked.
    pub fn contains(&self, i: usize) -> bool {
        if self.wraps() {
            i >= self.start || i < self.end
        } else {
            self.start <= i && i < self.end
        }
    }

    /// The locked ranges, in order along the sequence.
    pub fn parts(&self, len: usize) -> Vec<(usize, usize)> {
        if self.wraps() {
            [(0, self.end), (self.start, len)].into_iter().filter(|(a, b)| a < b).collect()
        } else {
            vec![(self.start, self.end)]
        }
    }

    #[cfg(test)]
    pub fn bp(&self, len: usize) -> usize {
        self.parts(len).iter().map(|(a, b)| b - a).sum()
    }

    /// The locked bases, in order from `start` (through the origin if it wraps).
    pub fn bases(&self, seq: &[u8]) -> Vec<u8> {
        if self.wraps() {
            let mut v = seq[self.start.min(seq.len())..].to_vec();
            v.extend_from_slice(&seq[..self.end.min(seq.len())]);
            v
        } else {
            seq[self.start.min(seq.len())..self.end.min(seq.len())].to_vec()
        }
    }

    /// Inserting at cursor position `pos` would put bases inside the lock.
    fn blocks_insert(&self, pos: usize) -> bool {
        if self.wraps() {
            pos > self.start || pos < self.end
        } else {
            self.start < pos && pos < self.end
        }
    }

    /// Deleting `[a, b)` would remove locked bases.
    fn blocks_delete(&self, a: usize, b: usize) -> bool {
        if a >= b {
            return false;
        }
        if self.wraps() {
            a < self.end || b > self.start
        } else {
            a < self.end && self.start < b
        }
    }

    fn after_insert(&mut self, pos: usize, n: usize) {
        if self.wraps() {
            // Only allowed between end and start; the head of the lock doesn't move.
            self.start += n;
        } else if pos <= self.start {
            self.start += n;
            self.end += n;
        }
    }

}

fn normalize(start: usize, end: usize, len: usize) -> (usize, usize) {
    // A wrapping range that ends at the origin is a plain range to the end.
    if start > end && end == 0 {
        (start, len)
    } else {
        (start, end)
    }
}

/// How a run of logged base edits moves the locks: either they all pass by the
/// locks, or one of them would change locked bases (its lock's id is returned).
/// `len` is the sequence length before the edits. Moving bases from one end to the
/// other (setting the origin, or undoing that) rotates the locks instead.
pub fn through(locks: &[Lock], ops: &[Op], len: usize) -> Result<Vec<Lock>, u64> {
    let seq: Vec<&Op> = ops.iter().filter(|op| op.is_sequence()).collect();
    let mut locks = locks.to_vec();
    if locks.is_empty() || seq.is_empty() {
        return Ok(locks);
    }
    if let Some(origin) = rotation(&seq, len) {
        for l in &mut locks {
            let rot = |x: usize| (x + len - origin) % len;
            let (s, mut e) = (rot(l.start), rot(l.end));
            if e == 0 {
                e = len;
            }
            *l = if s == e { Lock { id: l.id, start: 0, end: len } } else { Lock { id: l.id, start: s, end: e } };
        }
        return Ok(locks);
    }
    let mut len = len;
    for op in seq {
        match op {
            Op::Insert { pos, bases } => {
                if let Some(l) = locks.iter().find(|l| l.blocks_insert(*pos)) {
                    return Err(l.id);
                }
                for l in &mut locks {
                    l.after_insert(*pos, bases.len());
                }
            }
            Op::Delete { pos, bases } => {
                let (a, b) = (*pos, pos + bases.len());
                if let Some(l) = locks.iter().find(|l| l.blocks_delete(a, b)) {
                    return Err(l.id);
                }
                let left = len - bases.len();
                for l in &mut locks {
                    shift_for_delete(l, a, b);
                    if l.start == l.end {
                        // Everything but the lock went: it now covers the whole sequence.
                        *l = Lock { id: l.id, start: 0, end: left };
                    }
                }
            }
            _ => {}
        }
        len = match op {
            Op::Insert { bases, .. } => len + bases.len(),
            Op::Delete { bases, .. } => len - bases.len(),
            _ => len,
        };
    }
    Ok(locks)
}

fn shift_for_delete(l: &mut Lock, a: usize, b: usize) {
    let n = b - a;
    if l.wraps() {
        // Only allowed between end and start.
        l.start -= n;
    } else if b <= l.start {
        l.start -= n;
        l.end -= n;
    }
}

/// `Delete{0, X}` then `Insert{len - |X|, X}` makes the base at |X| the origin;
/// `Delete{len - |X|, X}` then `Insert{0, X}` makes the base at len - |X| the origin.
fn rotation(seq: &[&Op], len: usize) -> Option<usize> {
    let [Op::Delete { pos: dp, bases: d }, Op::Insert { pos: ip, bases: i }] = seq else { return None };
    let n = d.len();
    if d != i || n == 0 || n >= len {
        return None;
    }
    if *dp == 0 && *ip == len - n {
        Some(n)
    } else if *dp == len - n && *ip == 0 {
        Some(len - n)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(pos: usize, n: usize) -> Op {
        Op::Insert { pos, bases: "A".repeat(n) }
    }
    fn del(pos: usize, n: usize) -> Op {
        Op::Delete { pos, bases: "A".repeat(n) }
    }
    fn lock(s: usize, e: usize) -> Lock {
        Lock { id: 7, start: s, end: e }
    }

    #[test]
    fn edits_before_a_lock_move_it_and_after_leave_it() {
        let l = [lock(10, 20)];
        assert_eq!(through(&l, &[ins(5, 3)], 100), Ok(vec![lock(13, 23)]));
        assert_eq!(through(&l, &[ins(10, 3)], 100), Ok(vec![lock(13, 23)]));
        assert_eq!(through(&l, &[ins(20, 3)], 100), Ok(vec![lock(10, 20)]));
        assert_eq!(through(&l, &[del(2, 8)], 100), Ok(vec![lock(2, 12)]));
        assert_eq!(through(&l, &[del(20, 30)], 100), Ok(vec![lock(10, 20)]));
    }

    #[test]
    fn edits_inside_a_lock_are_refused() {
        let l = [lock(10, 20)];
        assert_eq!(through(&l, &[ins(11, 1)], 100), Err(7));
        assert_eq!(through(&l, &[del(19, 1)], 100), Err(7));
        assert_eq!(through(&l, &[del(5, 6)], 100), Err(7));
        assert_eq!(through(&l, &[del(0, 100)], 100), Err(7));
        // A later op in the same step is checked against where the lock has moved to.
        assert_eq!(through(&l, &[ins(0, 5), del(14, 2)], 100), Err(7));
        assert_eq!(through(&l, &[ins(0, 5), del(10, 5)], 100), Ok(vec![lock(10, 20)]));
    }

    #[test]
    fn wrapping_locks() {
        // Bases 90..100 and 0..5 of 100.
        let l = [lock(90, 5)];
        assert!(l[0].contains(95) && l[0].contains(0) && !l[0].contains(5) && !l[0].contains(89));
        assert_eq!(through(&l, &[ins(0, 1)], 100), Err(7));
        assert_eq!(through(&l, &[ins(100, 1)], 100), Err(7));
        assert_eq!(through(&l, &[ins(5, 2)], 100), Ok(vec![lock(92, 5)]));
        assert_eq!(through(&l, &[ins(90, 2)], 100), Ok(vec![lock(92, 5)]));
        assert_eq!(through(&l, &[del(10, 10)], 100), Ok(vec![lock(80, 5)]));
        assert_eq!(through(&l, &[del(85, 6)], 100), Err(7));
        assert_eq!(through(&l, &[del(5, 85)], 100), Ok(vec![lock(0, 15)]));
        assert_eq!(l[0].parts(100), vec![(0, 5), (90, 100)]);
        assert_eq!(l[0].bp(100), 15);
        assert_eq!(Lock::new(1, 90, 0, 100), Some(lock(90, 100)).map(|l| Lock { id: 1, ..l }));
        assert_eq!(Lock::new(1, 3, 3, 100), None);
    }

    #[test]
    fn reindexing_rotates_locks() {
        let x = "A".repeat(30);
        let set_origin = [Op::Delete { pos: 0, bases: x.clone() }, Op::Insert { pos: 70, bases: x.clone() }];
        // Origin moves to base 30: 40..50 becomes 10..20, 20..35 wraps, 90..100 ends at the end.
        assert_eq!(through(&[lock(40, 50)], &set_origin, 100), Ok(vec![lock(10, 20)]));
        assert_eq!(through(&[lock(20, 35)], &set_origin, 100), Ok(vec![lock(90, 5)]));
        assert_eq!(through(&[lock(20, 30)], &set_origin, 100), Ok(vec![lock(90, 100)]));
        // Undoing it rotates back.
        let undo = [Op::Delete { pos: 70, bases: x.clone() }, Op::Insert { pos: 0, bases: x }];
        assert_eq!(through(&[lock(90, 5)], &undo, 100), Ok(vec![lock(20, 35)]));
        assert_eq!(through(&[lock(10, 20)], &undo, 100), Ok(vec![lock(40, 50)]));
    }

    #[test]
    fn bases_read_through_the_origin() {
        assert_eq!(lock(8, 2).bases(b"abcdefghij"), b"ijab".to_vec());
        assert_eq!(lock(2, 4).bases(b"abcdefghij"), b"cd".to_vec());
    }
}
