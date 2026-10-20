// SPDX-License-Identifier: Apache-2.0
//! Synthetic, column-oriented 1M-row data source. Cells are formatted on demand.

/// Number of rows in the synthetic source.
pub const ROW_COUNT: usize = 1_000_000;
/// Number of columns in the synthetic source.
pub const COL_COUNT: usize = 8;
/// Column headers, in column order.
pub const HEADERS: [&str; COL_COUNT] = [
    "id",
    "customer",
    "qty",
    "price",
    "shipped",
    "order_date",
    "note",
    "region",
];

const CUSTOMER_POOL: usize = 64;
const REGIONS: [&str; 8] = [
    "north", "south", "east", "west", "central", "coastal", "alpine", "island",
];
const NOTES: [&str; 16] = [
    "",
    "gift wrap",
    "leave at door",
    "call before delivery",
    "fragile",
    "express",
    "return pending",
    "backordered item, expected to ship within two weeks of the original order date as agreed",
    "customer asked for a replacement unit after the first shipment arrived damaged in transit",
    "billing address differs from the shipping address; verified by phone with the account owner",
    "part of a bulk order",
    "loyalty discount applied",
    "signature required",
    "do not stack",
    "address corrected by support, see the ticket history for the full conversation log and notes",
    "none",
];

/// Column-stored rows, built from a deterministic formula of the row index.
pub struct SyntheticRows {
    id: Vec<i64>,
    customer: Vec<u16>,
    qty: Vec<i32>,
    price: Vec<f64>,
    shipped: Vec<bool>,
    order_date: Vec<i32>,
    note: Vec<u16>,
    region: Vec<u8>,
    customers: Vec<String>,
}

impl SyntheticRows {
    /// Builds all `rows` rows.
    #[must_use]
    // Every cast is on a value already reduced by a small modulo, so none can truncate.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_precision_loss
    )]
    pub fn new(rows: usize) -> Self {
        let mut this = Self {
            id: Vec::with_capacity(rows),
            customer: Vec::with_capacity(rows),
            qty: Vec::with_capacity(rows),
            price: Vec::with_capacity(rows),
            shipped: Vec::with_capacity(rows),
            order_date: Vec::with_capacity(rows),
            note: Vec::with_capacity(rows),
            region: Vec::with_capacity(rows),
            customers: (0..CUSTOMER_POOL)
                .map(|i| format!("Customer {i:02} Ltd"))
                .collect(),
        };
        for r in 0..rows {
            // The casts are bounded by the modulo on the left of each one.
            this.id.push(i64::try_from(r).unwrap_or(i64::MAX));
            this.customer.push(((r * 7 + 3) % CUSTOMER_POOL) as u16);
            this.qty.push((r % 97) as i32 + 1);
            this.price.push(((r * 37) % 100_000) as f64 / 100.0);
            this.shipped.push(r % 3 != 0);
            this.order_date.push(18_262 + (r % 3_650) as i32);
            this.note.push(((r * 5) % NOTES.len()) as u16);
            this.region.push((r % REGIONS.len()) as u8);
        }
        this
    }

    /// Number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.id.len()
    }

    /// Formats one cell. Out-of-range coordinates give an empty string.
    #[must_use]
    pub fn cell(&self, row: usize, col: usize) -> String {
        if row >= self.len() {
            return String::new();
        }
        match col {
            0 => self.id[row].to_string(),
            1 => self.customers[usize::from(self.customer[row])].clone(),
            2 => self.qty[row].to_string(),
            3 => format!("{:.2}", self.price[row]),
            4 => self.shipped[row].to_string(),
            5 => format_days(self.order_date[row]),
            6 => NOTES[usize::from(self.note[row])].to_owned(),
            7 => REGIONS[usize::from(self.region[row])].to_owned(),
            _ => String::new(),
        }
    }
}

/// Days since 1970-01-01 as `YYYY-MM-DD` (civil-from-days algorithm).
fn format_days(days: i32) -> String {
    let z = i64::from(days) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_deterministic_and_in_range() {
        let a = SyntheticRows::new(ROW_COUNT);
        let b = SyntheticRows::new(ROW_COUNT);
        assert_eq!(a.len(), ROW_COUNT);
        for row in [0, 1, 500_000, ROW_COUNT - 1] {
            for col in 0..COL_COUNT {
                assert_eq!(a.cell(row, col), b.cell(row, col));
            }
        }
        assert_eq!(a.cell(0, 0), "0");
        assert_eq!(a.cell(999_999, 0), "999999");
        assert_eq!(a.cell(0, 5), "2020-01-01");
        assert_eq!(a.cell(0, 4), "false");
        assert!(a.cell(999_999, 1).starts_with("Customer "));
    }

    #[test]
    fn out_of_range_is_empty() {
        let a = SyntheticRows::new(10);
        assert_eq!(a.cell(10, 0), "");
        assert_eq!(a.cell(0, COL_COUNT), "");
    }

    #[test]
    fn some_notes_are_long() {
        assert!(NOTES.iter().any(|n| n.len() >= 80));
    }
}
