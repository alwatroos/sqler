// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

use sqler_core::{Column, ResultSet, Row};

use crate::{BoxFuture, DriverError};

/// One step of a query result.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum QueryEvent {
    /// The result columns. Empty for statements that return no rows.
    Columns(Vec<Column>),
    /// A batch of rows.
    Rows(Vec<Row>),
    /// The end of the result.
    Done {
        /// Rows affected by the statement, if the backend reports it.
        rows_affected: Option<u64>,
    },
}

/// A stream of [`QueryEvent`]s for one statement.
///
/// Protocol: exactly one `Columns` first (an empty vec for statements that return no rows),
/// then zero or more `Rows`, then exactly one `Done`, then `None` forever. An `Err` ends
/// the stream. Dropping the stream must stop fetching. After a cancel, the stream yields
/// `Err(DriverError::Cancelled)` and then `None`.
pub trait QueryStream: Send {
    /// The next event, or `None` when the stream has ended.
    fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>>;
}

/// Reads a stream to the end into a [`ResultSet`], enforcing the stream protocol.
///
/// # Errors
/// Returns [`DriverError::Protocol`] if `Rows` come before `Columns`, `Columns` comes twice,
/// or the stream ends before `Done`; `DriverError::Core` if a row has the wrong width; and
/// any error from the stream unchanged.
pub async fn collect(stream: &mut (dyn QueryStream + '_)) -> Result<ResultSet, DriverError> {
    let mut result: Option<ResultSet> = None;
    loop {
        match stream.next_event().await {
            None => {
                return Err(DriverError::Protocol("stream ended before Done".to_owned()));
            }
            Some(Err(e)) => return Err(e),
            Some(Ok(QueryEvent::Columns(columns))) => {
                if result.is_some() {
                    return Err(DriverError::Protocol("second Columns event".to_owned()));
                }
                result = Some(ResultSet::new(columns));
            }
            Some(Ok(QueryEvent::Rows(rows))) => {
                let Some(rs) = result.as_mut() else {
                    return Err(DriverError::Protocol("Rows before Columns".to_owned()));
                };
                for row in rows {
                    rs.push_row(row)?;
                }
            }
            Some(Ok(QueryEvent::Done { rows_affected })) => {
                let Some(mut rs) = result else {
                    return Err(DriverError::Protocol("Done before Columns".to_owned()));
                };
                rs.set_rows_affected(rows_affected);
                return Ok(rs);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    use sqler_core::{CoreError, DataType, Value};

    use super::*;

    /// Polls a future to completion; the mocks are always ready.
    pub(crate) fn block_on<F: Future>(fut: F) -> F::Output {
        let mut fut = std::pin::pin!(fut);
        let mut cx = Context::from_waker(Waker::noop());
        loop {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    struct VecStream(std::vec::IntoIter<Result<QueryEvent, DriverError>>);

    impl VecStream {
        fn new(events: Vec<Result<QueryEvent, DriverError>>) -> Self {
            Self(events.into_iter())
        }
    }

    impl QueryStream for VecStream {
        fn next_event(&mut self) -> BoxFuture<'_, Option<Result<QueryEvent, DriverError>>> {
            let next = self.0.next();
            Box::pin(std::future::ready(next))
        }
    }

    fn cols() -> QueryEvent {
        QueryEvent::Columns(vec![Column::new("a", DataType::Integer)])
    }

    fn run(events: Vec<Result<QueryEvent, DriverError>>) -> Result<ResultSet, DriverError> {
        block_on(collect(&mut VecStream::new(events)))
    }

    #[test]
    fn happy_path() {
        let rs = run(vec![
            Ok(cols()),
            Ok(QueryEvent::Rows(vec![Row(vec![Value::Int(1)])])),
            Ok(QueryEvent::Rows(vec![Row(vec![Value::Int(2)])])),
            Ok(QueryEvent::Done {
                rows_affected: None,
            }),
        ])
        .unwrap();
        assert_eq!(rs.columns().len(), 1);
        assert_eq!(rs.len(), 2);
        assert_eq!(rs.rows_affected(), None);
    }

    #[test]
    fn empty_columns_with_rows_affected() {
        let rs = run(vec![
            Ok(QueryEvent::Columns(vec![])),
            Ok(QueryEvent::Done {
                rows_affected: Some(3),
            }),
        ])
        .unwrap();
        assert_eq!(rs.columns().len(), 0);
        assert!(rs.is_empty());
        assert_eq!(rs.rows_affected(), Some(3));
    }

    #[test]
    fn rows_before_columns() {
        let err = run(vec![Ok(QueryEvent::Rows(vec![]))]).unwrap_err();
        assert!(matches!(err, DriverError::Protocol(_)));
    }

    #[test]
    fn done_before_columns() {
        let err = run(vec![Ok(QueryEvent::Done {
            rows_affected: None,
        })])
        .unwrap_err();
        assert!(matches!(err, DriverError::Protocol(_)));
    }

    #[test]
    fn second_columns() {
        let err = run(vec![Ok(cols()), Ok(cols())]).unwrap_err();
        assert!(matches!(err, DriverError::Protocol(_)));
    }

    #[test]
    fn none_before_done() {
        let err = run(vec![Ok(cols())]).unwrap_err();
        assert!(matches!(err, DriverError::Protocol(_)));
        let err = run(vec![]).unwrap_err();
        assert!(matches!(err, DriverError::Protocol(_)));
    }

    #[test]
    fn width_mismatch_is_core() {
        let err = run(vec![
            Ok(cols()),
            Ok(QueryEvent::Rows(vec![Row(vec![])])),
            Ok(QueryEvent::Done {
                rows_affected: None,
            }),
        ])
        .unwrap_err();
        assert!(matches!(
            err,
            DriverError::Core(CoreError::RowWidth {
                expected: 1,
                actual: 0
            })
        ));
    }

    #[test]
    fn stream_error_passes_through() {
        let err = run(vec![Ok(cols()), Err(DriverError::Cancelled)]).unwrap_err();
        assert!(matches!(err, DriverError::Cancelled));
        let err = run(vec![Err(DriverError::Query("bad".into()))]).unwrap_err();
        assert!(matches!(err, DriverError::Query(ref s) if s == "bad"));
    }
}
