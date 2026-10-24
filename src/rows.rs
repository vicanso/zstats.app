//! A ranked list's rows, on either front end: how the region under a
//! list's pinned header is divided among whole rows, so a card ends on the
//! gap above the footer as Overview's does and the region ends on a row
//! ([`fit_rows`]), and what a full CPU meter means. The heights are each
//! front end's own — measured from its type — and only the arithmetic is
//! shared. Moved out of the gpui Processes page for the NSH Apps page
//! (phase 4d of docs/nsh-migration.md).

/// Floor for the per-row CPU bar. The original scale was 800% (eight
/// cores), which left every normal row looking empty. One core is the
/// unit the number is already in; the page max wins if something is
/// hotter, so the trough still fills on a busy machine.
pub const BAR_FLOOR_PERCENT: f32 = 100.0;

/// How far under its natural height a row may go to let one more in: a
/// point and a quarter off each padding. More, and the rows read as a
/// denser list than the one on the next tab. It was 2, and History's
/// wider ranges then held five rows on the short panel, each eleven
/// over its own height, for want of a sixth at 2.2 under.
pub const ROW_SQUEEZE: f32 = 2.5;

/// How far over it. Past this the rows would be another list's rows, so
/// the region keeps natural ones and the leftover stays under the card.
/// Only a panel dragged shorter than the short form gets there: with six
/// rows or more the largest share is `(row − ROW_SQUEEZE) ÷ 6`, about 10.
const ROW_STRETCH: f32 = 12.;

/// The rows run this far past the region's edge, so the boundary under
/// the last whole row is never a pixel short of the clip — where the row
/// after it would show its hairline as a line across the card's bottom
/// edge. What is cut is half a point of the last row's padding. It is
/// also what makes the standard regions come out even: they end in a
/// half (gpui's list header is 39.5), and the rows are whole points.
pub const ROW_SEAM: f32 = 0.5;

/// A rows region divided among whole rows ([`fit_rows`]).
///
/// The rows are whole points tall, the first few a point taller than the
/// rest, because gpui rounds each row's height to the pixel by itself (and
/// NSH a text's to the point): an equal fractional share came out three
/// pixels short over nine History rows, and the tenth row's hairline
/// stood in the gap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowFit {
    /// The scrolling region's height.
    pub viewport: f32,
    /// One collapsed row, hairline included.
    row: f32,
    /// How many rows from the top are a point taller than `row`.
    taller: usize,
    /// The row's height before the fit, and its padding at that height.
    natural: f32,
    pad: f32,
}

impl RowFit {
    /// The collapsed row at `index`, hairline included.
    pub fn height(self, index: usize) -> f32 {
        self.row + if index < self.taller { 1. } else { 0. }
    }

    /// What stands above and below that row's lines.
    pub fn pad(self, index: usize) -> f32 {
        self.pad + (self.height(index) - self.natural) / 2.
    }
}

/// Divide `budget` among whole rows of about `natural`, `pad` of which
/// stands above and below each row's lines.
///
/// A region that is not a multiple of the row slices the next one, and
/// on the short panel that slice sat on the footer's hairline. The
/// first answer cut the region back to a whole number of natural rows
/// and left the rest under the card. That was 23.5 of empty panel under
/// Apps on the short form, 38.5 under Processes and 64.5 under Apps on
/// the long one: a card that stopped short of where every other page's
/// last card stops, with the sixth row's hairline standing clear of its
/// rounded corners. So the rows take the leftover instead. Each is given
/// a share of the region, in its padding; the card ends on the gap above
/// the footer as Overview's does, and the region still ends on a row.
///
/// One more row goes in when that costs each at most [`ROW_SQUEEZE`];
/// otherwise the rows that fit are stretched, by at most
/// [`ROW_STRETCH`]. A window too short for one row keeps the budget, so
/// the card still cannot run under the footer.
pub fn fit_rows(budget: f32, natural: f32, pad: f32) -> RowFit {
    let plain = |viewport: f32, row: f32| RowFit {
        viewport,
        row,
        taller: 0,
        natural,
        pad,
    };
    if natural <= 1.0 || budget < natural {
        return plain(budget, natural);
    }
    // Whole points to share out: the region and the seam past it.
    let whole = (budget + ROW_SEAM).floor();
    let more = (whole / natural).ceil();
    let n = if whole / more >= natural - ROW_SQUEEZE {
        more
    } else {
        (more - 1.0).max(1.0)
    };
    let row = (whole / n).floor();
    if row - natural > ROW_STRETCH {
        let row = natural.ceil();
        return plain(n * row - ROW_SEAM, row);
    }
    RowFit {
        viewport: whole - ROW_SEAM,
        row,
        taller: (whole - n * row) as usize,
        natural,
        pad,
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// gpui's collapsed process row and its padding (`processes.rs`): the
    /// row these limits were tuned on.
    const COLLAPSED_ROW: f32 = 7. + 7. + 19.5 + 2. + 16. + 4. + 3. + 1.;
    const ROW_PAD: f32 = 7.;

    /// How many whole rows a fit shows, and how far they reach.
    pub fn whole_rows(fit: RowFit) -> (usize, f32) {
        let (mut rows, mut reach) = (0, 0.);
        while reach + fit.height(rows) <= fit.viewport + ROW_SEAM {
            reach += fit.height(rows);
            rows += 1;
        }
        (rows, reach)
    }

    /// One more row goes in only while that costs each at most
    /// `ROW_SQUEEZE`; a region that is a whole number of rows keeps them
    /// within a quarter point of their own padding.
    #[test]
    fn a_row_is_squeezed_a_little_or_not_at_all() {
        let exact = fit_rows(6. * COLLAPSED_ROW, COLLAPSED_ROW, ROW_PAD);
        assert_eq!(whole_rows(exact).0, 6);
        assert!((exact.pad(0) - ROW_PAD).abs() <= 0.25);

        // Seven rows at a point and a half under: seven.
        let near = fit_rows(7. * (COLLAPSED_ROW - 1.5), COLLAPSED_ROW, ROW_PAD);
        assert_eq!(whole_rows(near).0, 7);
        assert!(near.pad(0) < ROW_PAD);
        // At three under: six, stretched.
        let far = fit_rows(7. * (COLLAPSED_ROW - 3.), COLLAPSED_ROW, ROW_PAD);
        assert_eq!(whole_rows(far).0, 6);
        assert!(far.pad(0) > ROW_PAD);
    }

    /// A panel dragged down to a couple of rows keeps rows of its own
    /// height and the leftover under the card, rather than two rows of
    /// three times the padding; one too short for a single row keeps the
    /// budget.
    #[test]
    fn a_region_of_few_rows_keeps_natural_ones() {
        let budget = 2.7 * COLLAPSED_ROW;
        let fit = fit_rows(budget, COLLAPSED_ROW, ROW_PAD);
        assert_eq!(fit.height(0), COLLAPSED_ROW.ceil());
        assert_eq!(whole_rows(fit), (2, 2. * COLLAPSED_ROW.ceil()));
        assert!(fit.viewport < budget - COLLAPSED_ROW / 2.);

        let none = fit_rows(40., COLLAPSED_ROW, ROW_PAD);
        assert_eq!((none.viewport, none.height(0)), (40., COLLAPSED_ROW));
        assert_eq!(none.pad(0), ROW_PAD);
    }
}
