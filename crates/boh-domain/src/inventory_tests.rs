use super::{Allocation, AllocationSource, ItemBalance, LotBalance, next_lot_serial};
use crate::lot::LotId;

fn lot(serial: i64, remaining: i64) -> LotBalance {
    LotBalance {
        lot_id: LotId::parse(&format!("RAW-FLOUR-20261006-{serial:03}")).unwrap(),
        remaining,
    }
}

fn part(serial: Option<i64>, qty: i64, source: AllocationSource) -> Allocation {
    Allocation {
        lot_id: serial.map(|serial| lot(serial, 0).lot_id),
        qty,
        source,
    }
}

#[test]
fn specified_then_fifo_then_shortfall_share_the_updated_balance() {
    let mut balance = ItemBalance::new(vec![lot(1, 5), lot(2, 10), lot(3, 0)], -2).unwrap();
    assert_eq!(balance.book(), 13);
    assert_eq!(
        balance.allocate(Some(0), 8).unwrap(),
        vec![part(Some(1), 8, AllocationSource::Specified)]
    );
    assert_eq!(balance.book(), 5);
    assert_eq!(balance.lots()[0].remaining, -3);
    assert_eq!(
        balance.allocate(None, 12).unwrap(),
        vec![
            part(Some(2), 10, AllocationSource::Fifo),
            part(None, 2, AllocationSource::Shortfall)
        ]
    );
    assert_eq!(balance.book(), -7);
    assert_eq!(
        balance
            .lots()
            .iter()
            .map(|lot| lot.remaining)
            .collect::<Vec<_>>(),
        [-3, 0, 0]
    );
    assert_eq!(
        balance.allocate(None, 1).unwrap(),
        vec![part(None, 1, AllocationSource::Shortfall)]
    );
    assert_eq!(balance.book(), -8);
}

#[test]
fn fifo_preserves_input_order_and_exact_exhaustion_has_no_shortfall() {
    let mut balance = ItemBalance::new(vec![lot(2, 4), lot(1, 3)], 0).unwrap();
    assert_eq!(
        balance.allocate(None, 7).unwrap(),
        vec![
            part(Some(2), 4, AllocationSource::Fifo),
            part(Some(1), 3, AllocationSource::Fifo)
        ]
    );
    assert_eq!(balance.book(), 0);
}

#[test]
fn book_sum_uses_wide_intermediates_and_only_checks_the_final_total() {
    // In FIFO order the first two lots exceed i64; the last lot offsets them.
    let balance = ItemBalance::new(vec![lot(1, i64::MAX), lot(2, 1), lot(3, -1)], 0).unwrap();
    assert_eq!(balance.book(), i64::MAX);
    let balance = ItemBalance::new(vec![lot(1, -1), lot(2, 1)], i64::MIN).unwrap();
    assert_eq!(balance.book(), i64::MIN);
    assert!(ItemBalance::new(vec![lot(1, i64::MAX), lot(2, 1)], 0).is_err());
    assert!(ItemBalance::new(vec![lot(1, -1)], i64::MIN).is_err());
}

#[test]
fn every_deduction_checks_net_lot_and_unallocated_overflow() {
    let mut net = ItemBalance::new(vec![lot(1, i64::MIN)], 0).unwrap();
    assert!(net.allocate(None, 1).is_err());
    let mut specified = ItemBalance::new(vec![lot(1, i64::MIN), lot(2, 1)], 0).unwrap();
    assert!(specified.allocate(Some(0), 1).is_err());
    let mut shortfall = ItemBalance::new(vec![lot(1, 1)], i64::MIN).unwrap();
    assert!(shortfall.allocate(None, 2).is_err());
    let mut last_line = ItemBalance::new(vec![lot(1, i64::MIN + 1)], 0).unwrap();
    last_line.allocate(Some(0), 1).unwrap();
    assert_eq!(last_line.book(), i64::MIN);
    assert!(last_line.allocate(Some(0), 1).is_err());
}

#[test]
fn lot_serial_reaches_999_without_wrapping_or_reuse() {
    assert_eq!(next_lot_serial(0).unwrap(), 1);
    assert_eq!(next_lot_serial(998).unwrap(), 999);
    assert!(next_lot_serial(999).is_err());
    assert!(next_lot_serial(i64::MAX).is_err());
}
