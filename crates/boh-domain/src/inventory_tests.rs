use super::{Allocation, AllocationSource, ItemBalance, LotBalance, next_lot_serial};
use crate::DomainError;
use crate::lot::LotId;
use crate::quantity::Quantity;

const LIMIT: i64 = 1_000_000_000_000;

fn q(milli: i64) -> Quantity {
    Quantity::from_milli(milli)
}

fn lot(serial: i64, remaining: i64) -> LotBalance {
    LotBalance {
        lot_id: LotId::parse(&format!("RAW-FLOUR-20261006-{serial:03}")).unwrap(),
        remaining: q(remaining),
    }
}

fn part(serial: Option<i64>, qty: i64, source: AllocationSource) -> Allocation {
    Allocation {
        lot_id: serial.map(|serial| lot(serial, 0).lot_id),
        qty: q(qty),
        source,
    }
}

#[test]
fn specified_then_fifo_then_shortfall_share_the_updated_balance() {
    let mut balance = ItemBalance::new(vec![lot(1, 5), lot(2, 10), lot(3, 0)], q(-2)).unwrap();
    assert_eq!(balance.book().milli(), 13);
    assert_eq!(
        balance.allocate(Some(0), q(8)).unwrap(),
        vec![part(Some(1), 8, AllocationSource::Specified)]
    );
    assert_eq!(balance.book().milli(), 5);
    assert_eq!(balance.lots()[0].remaining.milli(), -3);
    assert_eq!(
        balance.allocate(None, q(12)).unwrap(),
        vec![
            part(Some(2), 10, AllocationSource::Fifo),
            part(None, 2, AllocationSource::Shortfall)
        ]
    );
    assert_eq!(balance.book().milli(), -7);
    assert_eq!(
        balance
            .lots()
            .iter()
            .map(|lot| lot.remaining.milli())
            .collect::<Vec<_>>(),
        [-3, 0, 0]
    );
    assert_eq!(
        balance.allocate(None, q(1)).unwrap(),
        vec![part(None, 1, AllocationSource::Shortfall)]
    );
    assert_eq!(balance.book().milli(), -8);
}

#[test]
fn fifo_preserves_input_order_and_exact_exhaustion_has_no_shortfall() {
    let mut balance = ItemBalance::new(vec![lot(2, 4), lot(1, 3)], q(0)).unwrap();
    assert_eq!(
        balance.allocate(None, q(7)).unwrap(),
        vec![
            part(Some(2), 4, AllocationSource::Fifo),
            part(Some(1), 3, AllocationSource::Fifo)
        ]
    );
    assert_eq!(balance.book().milli(), 0);
}

#[test]
fn initial_book_checks_the_final_sum_after_cancellation() {
    let balance =
        ItemBalance::new(vec![lot(1, LIMIT), lot(2, LIMIT), lot(3, -LIMIT)], q(0)).unwrap();
    assert_eq!(balance.book().milli(), LIMIT);
    let balance = ItemBalance::new(vec![lot(1, -LIMIT), lot(2, LIMIT)], q(-LIMIT)).unwrap();
    assert_eq!(balance.book().milli(), -LIMIT);
    assert_eq!(
        ItemBalance::new(vec![lot(1, LIMIT), lot(2, 1)], q(0)).unwrap_err(),
        DomainError::StockLimitExceeded { lot_id: None }
    );
    assert_eq!(
        ItemBalance::new(vec![lot(1, LIMIT + 1), lot(2, -1)], q(0)).unwrap_err(),
        DomainError::StockLimitExceeded {
            lot_id: Some(lot(1, 0).lot_id)
        }
    );
    assert!(ItemBalance::new(vec![], q(-LIMIT - 1)).is_err());
    assert!(ItemBalance::new(vec![lot(1, i64::MIN)], q(0)).is_err());
}

#[test]
fn every_deduction_checks_limits_and_reports_existing_lot_before_net_book() {
    let mut balance = ItemBalance::new(vec![lot(1, -LIMIT + 1)], q(0)).unwrap();
    balance.allocate(Some(0), q(1)).unwrap();
    assert_eq!(balance.book().milli(), -LIMIT);
    assert_eq!(
        balance.allocate(Some(0), q(1)).unwrap_err(),
        DomainError::StockLimitExceeded {
            lot_id: Some(lot(1, 0).lot_id)
        }
    );
    assert_eq!(
        balance.allocate(None, q(1)).unwrap_err(),
        DomainError::StockLimitExceeded { lot_id: None }
    );
    assert_eq!(
        balance.allocate(Some(0), q(i64::MAX)).unwrap_err(),
        DomainError::StockLimitExceeded {
            lot_id: Some(lot(1, 0).lot_id)
        }
    );
}

#[test]
fn lot_serial_reaches_999_without_wrapping_or_reuse() {
    assert_eq!(next_lot_serial(0).unwrap(), 1);
    assert_eq!(next_lot_serial(998).unwrap(), 999);
    assert!(next_lot_serial(999).is_err());
    assert!(next_lot_serial(i64::MAX).is_err());
}

#[test]
fn allocation_returns_domain_error_for_out_of_bounds_lot() {
    let mut balance = ItemBalance::new(vec![lot(1, 5)], q(0)).unwrap();
    assert!(matches!(
        balance.allocate(Some(1), q(1)),
        Err(crate::DomainError::InvalidField(_))
    ));
}

fn snapshot(balance: &ItemBalance) -> (i64, Vec<(LotId, i64)>, i64) {
    (
        balance.book().milli(),
        balance
            .lots()
            .iter()
            .map(|lot| (lot.lot_id.clone(), lot.remaining.milli()))
            .collect(),
        balance.unallocated.milli(),
    )
}

#[test]
fn failed_allocations_preserve_all_balances_and_subsequent_results() {
    let cases = [
        (
            "net limit",
            ItemBalance::new(vec![lot(1, 2)], q(-LIMIT)).unwrap(),
            None,
            3,
            None,
        ),
        (
            "specified limit",
            ItemBalance::new(vec![lot(1, -LIMIT), lot(2, 3)], q(0)).unwrap(),
            Some(0),
            1,
            Some(1),
        ),
        (
            "negative quantity",
            ItemBalance::new(vec![lot(1, 5)], q(0)).unwrap(),
            None,
            -1,
            None,
        ),
        (
            "zero quantity",
            ItemBalance::new(vec![lot(1, 5)], q(0)).unwrap(),
            Some(0),
            0,
            None,
        ),
        // Deliberately inconsistent cached book reaches the defensive shortfall
        // check after FIFO has already planned a lot update.
        (
            "shortfall limit",
            ItemBalance {
                lots: vec![lot(1, 5)],
                unallocated: q(-LIMIT),
                book: q(10),
            },
            None,
            6,
            None,
        ),
        (
            "invalid lot index",
            ItemBalance::new(vec![lot(1, 5)], q(-1)).unwrap(),
            Some(1),
            1,
            None,
        ),
    ];
    for (name, mut balance, specified, qty, next_specified) in cases {
        let before = snapshot(&balance);
        let mut control = ItemBalance {
            lots: balance
                .lots()
                .iter()
                .map(|lot| LotBalance {
                    lot_id: lot.lot_id.clone(),
                    remaining: lot.remaining,
                })
                .collect(),
            unallocated: balance.unallocated,
            book: balance.book(),
        };
        assert!(balance.allocate(specified, q(qty)).is_err(), "{name}");
        assert_eq!(snapshot(&balance), before, "{name}");
        assert_eq!(
            balance.allocate(next_specified, q(1)).unwrap(),
            control.allocate(next_specified, q(1)).unwrap(),
            "{name}"
        );
        assert_eq!(snapshot(&balance), snapshot(&control), "{name}");
    }
}

#[test]
fn receipts_check_new_lot_and_cumulative_net_limits_without_mutating_on_failure() {
    let mut balance = ItemBalance::new(vec![lot(1, -LIMIT)], q(0)).unwrap();
    let initial = snapshot(&balance);
    assert_eq!(
        balance.receive(lot(2, 0).lot_id, q(LIMIT + 1)),
        Err(DomainError::StockLimitExceeded { lot_id: None })
    );
    assert_eq!(snapshot(&balance), initial);
    balance.receive(lot(2, 0).lot_id, q(LIMIT)).unwrap();
    balance.receive(lot(3, 0).lot_id, q(LIMIT)).unwrap();
    assert_eq!(balance.book().milli(), LIMIT);
    let before = snapshot(&balance);
    assert_eq!(
        balance.receive(lot(4, 0).lot_id, q(1)),
        Err(DomainError::StockLimitExceeded { lot_id: None })
    );
    assert_eq!(snapshot(&balance), before);
    balance.allocate(Some(2), q(1)).unwrap();
    balance.receive(lot(4, 0).lot_id, q(1)).unwrap();
    assert_eq!(balance.book().milli(), LIMIT);
}

#[test]
fn received_lots_are_visible_to_later_fifo_lines_in_lot_date_order() {
    let mut balance = ItemBalance::new(vec![lot(2, 3)], q(0)).unwrap();
    balance.receive(lot(1, 0).lot_id, q(2)).unwrap();
    assert_eq!(
        balance.allocate(None, q(4)).unwrap(),
        vec![
            part(Some(1), 2, AllocationSource::Fifo),
            part(Some(2), 2, AllocationSource::Fifo)
        ]
    );
    assert_eq!(balance.book().milli(), 1);
}
