// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use sqler_driver_api::sqler_core::{Column, DataType, Row, Value};
use sqler_driver_api::{BoxFuture, DriverError, QueryEvent, QueryStream};

/// A `QueryStream` that generates `rows` rows of `columns` columns in batches.
///
/// Column `c0` is an integer row index. Column `ck` for `k >= 1` is text for `k % 4 == 1`, float
/// for 2, boolean for 3 and integer for 0. It follows the stream protocol, so it takes the same
/// path as a driver stream.
#[derive(Debug, Clone)]
pub struct SyntheticSource {
    rows: usize,
    columns: usize,
    batch_size: usize,
    next_row: usize,
    stage: Stage,
}

#[derive(Debug, Clone, Copy)]
enum Stage {
    Columns,
    Rows,
    Done,
    End,
}

fn to_i64(x: usize) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

fn data_type(k: usize) -> DataType {
    match k % 4 {
        1 => DataType::Text,
        2 => DataType::Float,
        3 => DataType::Boolean,
        _ => DataType::Integer,
    }
}

impl SyntheticSource {
    /// A source of `rows` rows with `columns` columns, `batch_size` rows per `Rows` event
    /// (0 is treated as 1).
    #[must_use]
    pub fn new(rows: usize, columns: usize, batch_size: usize) -> Self {
        Self {
            rows,
            columns,
            batch_size: batch_size.max(1),
            next_row: 0,
            stage: Stage::Columns,
        }
    }

    #[allow(clippy::cast_precision_loss)] // Row indices stay far below 2^53.
    fn row(&self, i: usize) -> Row {
        Row((0..self.columns)
            .map(|k| match (k, k % 4) {
                (0, _) => Value::Int(to_i64(i)),
                (_, 1) => Value::Text(format!("r{i}c{k}")),
                (_, 2) => Value::Float(i as f64 + 0.5),
                (_, 3) => Value::Bool(i.is_multiple_of(2)),
                _ => Value::Int(to_i64(i).saturating_mul(to_i64(k))),
            })
            .collect())
    }

    fn next(&mut self) -> Option<QueryEvent> {
        match self.stage {
            Stage::Columns => {
                self.stage = Stage::Rows;
                Some(QueryEvent::Columns(
                    (0..self.columns)
                        .map(|k| Column::new(format!("c{k}"), data_type(k)))
                        .collect(),
                ))
            }
            Stage::Rows if self.next_row < self.rows => {
                let end = self.rows.min(self.next_row.saturating_add(self.batch_size));
                let batch = (self.next_row..end).map(|i| self.row(i)).collect();
                self.next_row = end;
                Some(QueryEvent::Rows(batch))
            }
            Stage::Rows => {
                self.stage = Stage::Done;
                self.next()
            }
            Stage::Done => {
                self.stage = Stage::End;
                Some(QueryEvent::Done {
                    rows_affected: None,
                })
            }
            Stage::End => None,
        }
    }
}

impl QueryStream for SyntheticSource {
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
        let event = self.next().map(Ok);
        Box::pin(std::future::ready(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(mut s: SyntheticSource) -> Vec<QueryEvent> {
        let mut out = Vec::new();
        while let Some(ev) = s.next() {
            out.push(ev);
        }
        out
    }

    #[test]
    fn counts_and_batches() {
        let events = drain(SyntheticSource::new(10, 6, 4));
        assert!(matches!(&events[0], QueryEvent::Columns(c) if c.len() == 6));
        let sizes: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                QueryEvent::Rows(r) => Some(r.len()),
                _ => None,
            })
            .collect();
        assert_eq!(sizes, [4, 4, 2]);
        assert_eq!(events.len(), 5);
        assert!(matches!(
            events[4],
            QueryEvent::Done {
                rows_affected: None
            }
        ));
    }

    #[test]
    fn zero_rows_and_zero_batch_size() {
        assert_eq!(drain(SyntheticSource::new(0, 3, 4)).len(), 2);
        let events = drain(SyntheticSource::new(3, 2, 0));
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn value_pattern_of_row_7() {
        let src = SyntheticSource::new(8, 6, 8);
        let events = drain(src);
        let QueryEvent::Columns(cols) = &events[0] else {
            panic!("no columns");
        };
        let types: Vec<DataType> = cols.iter().map(|c| c.data_type).collect();
        assert_eq!(
            types,
            [
                DataType::Integer,
                DataType::Text,
                DataType::Float,
                DataType::Boolean,
                DataType::Integer,
                DataType::Text
            ]
        );
        assert_eq!(cols[5].name, "c5");
        let QueryEvent::Rows(rows) = &events[1] else {
            panic!("no rows");
        };
        assert_eq!(
            rows[7].0,
            [
                Value::Int(7),
                Value::Text("r7c1".to_owned()),
                Value::Float(7.5),
                Value::Bool(false),
                Value::Int(28),
                Value::Text("r7c5".to_owned()),
            ]
        );
    }

    #[test]
    fn stream_ends_with_none() {
        let mut s = SyntheticSource::new(1, 1, 1);
        for _ in 0..3 {
            assert!(s.next().is_some());
        }
        assert!(s.next().is_none());
        assert!(s.next().is_none());
    }
}
