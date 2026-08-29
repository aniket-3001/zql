//! Walking a table b-tree.
//!
//! # Shape of the tree
//!
//! Table b-trees have two page types. **Interior** pages (`0x05`) hold pointers
//! to children and nothing else that matters here. **Leaf** pages (`0x0d`) hold
//! the rows. Index pages (`0x0a`, `0x02`) belong to a different tree entirely
//! and are skipped — the spike's awkward fixture had an index sitting alongside
//! its tables specifically to prove that.
//!
//! # Why a cursor and not a recursion
//!
//! An explicit stack makes the walk *lazy*: rows come out one at a time, so
//! `SELECT ... LIMIT 10` over a million-row table reads a handful of pages
//! rather than all of them. It also bounds memory to the depth of the tree,
//! and it gives somewhere to put the cycle detection — a corrupt or hostile
//! file can point a page at its own ancestor, and a recursive walk would follow
//! that until the stack ran out.

use std::collections::HashSet;

use crate::error::Result;
use crate::sources::sqlite::pager::{Page, Pager};
use crate::sources::sqlite::record::{corrupt, decode_record, read_varint};
use crate::value::Value;

const LEAF_TABLE: u8 = 0x0d;
const INTERIOR_TABLE: u8 = 0x05;
const INTERIOR_INDEX: u8 = 0x02;
const LEAF_INDEX: u8 = 0x0a;

/// One row: its rowid and its decoded columns.
pub struct Record {
    pub rowid: i64,
    pub values: Vec<Value>,
}

/// A position part-way through one page.
struct Frame {
    page: Page,
    /// Which cell to visit next.
    cell: usize,
    cell_count: usize,
    is_leaf: bool,
    /// Index pages live in a different tree and carry no row data.
    is_index: bool,
    /// Interior pages have one more child than they have cells; this tracks
    /// whether that last one has been taken.
    rightmost_taken: bool,
}

/// A depth-first cursor over one table's rows.
pub struct Cursor {
    pager: Pager,
    stack: Vec<Frame>,
    /// Pages on the current path, for cycle detection.
    on_path: HashSet<u32>,
    /// A budget on total pages visited, so a corrupt tree that is a *lattice*
    /// rather than a cycle still terminates.
    pages_remaining: u32,
}

impl Cursor {
    pub fn open(mut pager: Pager, root_page: u32) -> Result<Cursor> {
        // Every page could legitimately be visited once, plus slack for
        // overflow chains, which are read outside this budget.
        let pages_remaining = pager.header.page_count.saturating_mul(2).max(16);
        let page = pager.read_page(root_page)?;
        let frame = Frame::new(page)?;

        Ok(Cursor {
            pager,
            on_path: HashSet::from([root_page]),
            stack: vec![frame],
            pages_remaining,
        })
    }

    /// The next row in rowid order, or `None` at the end of the table.
    ///
    /// Named `next` deliberately, matching [`RowIter`](crate::exec::RowIter):
    /// it cannot *be* an `Iterator`, because it yields `Result<Option<_>>`
    /// rather than `Option<Result<_>>`, but reading like one is the point.
    ///
    /// The position is read out of the top frame before anything else happens,
    /// rather than the frame being held borrowed across the body: reading a
    /// cell may have to follow an overflow chain, which needs the pager, and
    /// the frame and the pager live in the same struct.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Record>> {
        loop {
            let Some(frame) = self.stack.last() else {
                return Ok(None);
            };
            let (is_leaf, cell, cell_count, rightmost_taken) = (
                frame.is_leaf,
                frame.cell,
                frame.cell_count,
                frame.rightmost_taken,
            );

            if is_leaf {
                if cell < cell_count {
                    self.advance_cell();
                    let offset = self.cell_offset(cell)?;
                    return Ok(Some(self.read_leaf_cell(offset)?));
                }
                self.pop();
                continue;
            }

            // Interior: visit each child in turn, then the rightmost one,
            // which lives in the page header rather than the cell array.
            let child = if cell < cell_count {
                self.advance_cell();
                let offset = self.cell_offset(cell)?;
                self.top()?.page.u32_at(offset)?
            } else if !rightmost_taken {
                self.take_rightmost();
                let frame = self.top()?;
                let header = frame.page.header_offset();
                frame.page.u32_at(header + 8)?
            } else {
                self.pop();
                continue;
            };

            self.descend(child)?;
        }
    }

    fn advance_cell(&mut self) {
        if let Some(frame) = self.stack.last_mut() {
            frame.cell += 1;
        }
    }

    fn take_rightmost(&mut self) {
        if let Some(frame) = self.stack.last_mut() {
            frame.rightmost_taken = true;
        }
    }

    fn top(&self) -> Result<&Frame> {
        self.stack
            .last()
            .ok_or_else(|| corrupt("b-tree cursor lost its position"))
    }

    fn pop(&mut self) {
        if let Some(frame) = self.stack.pop() {
            self.on_path.remove(&frame.page.number);
        }
    }

    fn descend(&mut self, child: u32) -> Result<()> {
        if !self.on_path.insert(child) {
            return Err(corrupt(format!(
                "page {child} appears twice on one path"
            )));
        }
        self.pages_remaining = self
            .pages_remaining
            .checked_sub(1)
            .ok_or_else(|| corrupt("the b-tree walk visited too many pages"))?;

        let page = self.pager.read_page(child)?;
        let frame = Frame::new(page)?;

        // An index page inside a table tree is not row data. Skipping rather
        // than failing keeps a database with a damaged index readable.
        if frame.is_index {
            self.on_path.remove(&child);
            return Ok(());
        }

        self.stack.push(frame);
        Ok(())
    }

    /// Where cell `index` begins, read from the page's cell pointer array.
    fn cell_offset(&self, index: usize) -> Result<usize> {
        let frame = self.top()?;
        let header = frame.page.header_offset();
        // Leaf headers are 8 bytes; interior headers are 12, the extra four
        // being the rightmost child pointer.
        let array = header + if frame.is_leaf { 8 } else { 12 };
        let offset = frame.page.u16_at(array + index * 2)? as usize;

        if offset >= frame.page.usable().len() {
            return Err(corrupt("cell pointer points outside the page"));
        }
        Ok(offset)
    }

    /// Reads one row out of a leaf cell.
    ///
    /// The cell is: total payload size (varint), rowid (varint), then as much
    /// of the payload as lives on this page, and — if it did not all fit — a
    /// four-byte pointer to the first overflow page.
    fn read_leaf_cell(&mut self, offset: usize) -> Result<Record> {
        let usable = self.top()?.page.usable_size;
        let page_bytes = self.top()?.page.usable();

        let (payload_size, size_len) = read_varint(page_bytes, offset)?;
        let (rowid, rowid_len) = read_varint(page_bytes, offset + size_len)?;

        let payload_size = usize::try_from(payload_size)
            .map_err(|_| corrupt("negative payload size"))?;
        let body = offset + size_len + rowid_len;

        let local_size = local_payload_size(payload_size, usable);
        let local = page_bytes
            .get(body..body + local_size)
            .ok_or_else(|| corrupt("cell payload runs past the end of the page"))?
            .to_vec();

        let payload = if local_size == payload_size {
            local
        } else {
            let next = self.top()?.page.u32_at(body + local_size)?;
            self.read_overflow(local, payload_size, next)?
        };

        let values = decode_record(&payload, self.pager.header.encoding)?;
        Ok(Record { rowid, values })
    }

    /// Follows the overflow chain until the payload is whole.
    ///
    /// Each overflow page is a four-byte pointer to the next, then payload.
    /// The chain is bounded by the page count: a file can trivially be forged
    /// to loop, and this is the one place that reads unbounded input.
    fn read_overflow(
        &mut self,
        mut payload: Vec<u8>,
        total: usize,
        first: u32,
    ) -> Result<Vec<u8>> {
        let usable = self.pager.header.usable_size as usize;
        let per_page = usable
            .checked_sub(4)
            .ok_or_else(|| corrupt("page too small to hold an overflow pointer"))?;

        let mut next = first;
        let mut seen = HashSet::new();

        while payload.len() < total {
            if next == 0 {
                return Err(corrupt("overflow chain ended before the payload did"));
            }
            if !seen.insert(next) {
                return Err(corrupt(format!("overflow chain loops at page {next}")));
            }

            let page = self.pager.read_page(next)?;
            let bytes = page.usable();
            next = page.u32_at(0)?;

            let wanted = (total - payload.len()).min(per_page);
            let chunk = bytes
                .get(4..4 + wanted)
                .ok_or_else(|| corrupt("overflow page is shorter than it claims"))?;
            payload.extend_from_slice(chunk);
        }

        Ok(payload)
    }
}

impl Frame {
    fn new(page: Page) -> Result<Frame> {
        let header = page.header_offset();
        let kind = page.byte_at(header)?;

        let (is_leaf, is_index) = match kind {
            LEAF_TABLE => (true, false),
            INTERIOR_TABLE => (false, false),
            LEAF_INDEX => (true, true),
            INTERIOR_INDEX => (false, true),
            other => {
                return Err(corrupt(format!(
                    "page {} has unknown type 0x{other:02x}",
                    page.number
                )))
            }
        };

        let cell_count = page.u16_at(header + 3)? as usize;

        Ok(Frame {
            cell: 0,
            cell_count,
            is_leaf,
            is_index,
            rightmost_taken: false,
            page,
        })
    }
}

/// How much of a payload lives on the b-tree page itself.
///
/// **This is the trap in the format.** The answer is *not* "as much as fits":
/// SQLite deliberately picks a smaller size so that pages stay dense and the
/// tree stays shallow. An implementation that assumes "fill the page, then
/// overflow" reads the right bytes for small values and silently wrong ones
/// for large — which is exactly the class of bug that survives casual testing.
fn local_payload_size(total: usize, usable: usize) -> usize {
    let max_local = usable.saturating_sub(35);
    if total <= max_local {
        return total;
    }

    let min_local = ((usable.saturating_sub(12) * 32) / 255).saturating_sub(23);

    // A page too small to hold an overflow pointer makes this span zero, and
    // the modulus below would divide by it. The header check rejects such a
    // page long before this runs, but a function that computes an offset from
    // file-controlled numbers should not depend on being called correctly.
    let span = usable.saturating_sub(4);
    let surplus = if span == 0 {
        min_local
    } else {
        min_local + total.saturating_sub(min_local) % span
    };

    let local = if surplus <= max_local {
        surplus
    } else {
        min_local
    };
    // Never claim more of the payload than there is.
    local.min(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_payload_is_entirely_local() {
        // 4096-byte pages: anything up to 4061 bytes stays put.
        assert_eq!(local_payload_size(100, 4096), 100);
        assert_eq!(local_payload_size(4061, 4096), 4061);
    }

    #[test]
    fn a_large_payload_keeps_less_than_the_page_could_hold() {
        // The whole point: 9000 bytes into a 4096-byte page does *not* put
        // 4061 bytes locally.
        let local = local_payload_size(9000, 4096);
        assert!(local < 4061, "local was {local}, so overflow was mis-sized");
        assert!(local >= 489, "local was {local}, below min_local");
    }

    #[test]
    fn the_local_size_never_exceeds_max_local() {
        for usable in [512usize, 1024, 4096, 8192, 65_536] {
            let max_local = usable - 35;
            for total in [
                usable / 2,
                usable,
                usable * 2,
                usable * 10 + 7,
                30_000,
                1_000_000,
            ] {
                let local = local_payload_size(total, usable);
                assert!(
                    local <= max_local && local <= total,
                    "usable={usable} total={total} local={local}"
                );
            }
        }
    }

    #[test]
    fn the_min_local_formula_matches_the_published_constants() {
        // Worked by hand from the format reference, for the two page sizes the
        // spike exercised.
        assert_eq!(((4096usize - 12) * 32 / 255) - 23, 489);
        assert_eq!(((8192usize - 12) * 32 / 255) - 23, 1003);
    }

    #[test]
    fn a_tiny_page_does_not_underflow_the_arithmetic() {
        // Saturating throughout, so a corrupt header cannot panic here — and
        // in particular the `% (usable - 4)` cannot divide by zero.
        assert_eq!(local_payload_size(100, 0), 0);
        assert_eq!(local_payload_size(100, 4), 0);
        assert_eq!(local_payload_size(100, 30), 0);
        assert_eq!(local_payload_size(0, 0), 0);
    }
}
