//! 唯一系统时钟入口，以及不随真实时间流动的共享测试时钟。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use boh_domain::UnixMillis;
use tokio::sync::watch;

#[derive(Clone, Debug)]
pub struct Clock {
    source: Source,
}

#[derive(Clone, Debug)]
enum Source {
    System,
    Manual(watch::Sender<UnixMillis>),
}

impl Clock {
    /// Subscribe before the scheduler reads time so manual-clock updates cannot be lost.
    pub fn changes(&self) -> ClockChanges {
        ClockChanges {
            clock: self.clone(),
            manual: match &self.source {
                Source::System => None,
                Source::Manual(time) => Some(time.subscribe()),
            },
        }
    }

    pub fn system() -> Clock {
        Self {
            source: Source::System,
        }
    }

    /// 取原始 UTC Unix 毫秒，不做单调钳制。超出 i64 范围时取对应边界。
    pub fn now(&self) -> UnixMillis {
        match &self.source {
            Source::System => system_now(),
            Source::Manual(time) => *time.borrow(),
        }
    }

    /// 每次醒来重新检查墙钟；回拨时继续等待。
    pub async fn sleep_until(&self, deadline: UnixMillis) {
        match &self.source {
            Source::System => loop {
                let now = self.now();
                if now >= deadline {
                    return;
                }
                // 最长每秒重新读取墙钟，也能及时发现向前跳变。
                let remaining = deadline.0.checked_sub(now.0).unwrap_or(i64::MAX);
                let millis = u64::try_from(remaining.min(1000)).unwrap_or(1000);
                tokio::time::sleep(Duration::from_millis(millis)).await;
            },
            Source::Manual(time) => {
                let mut changes = time.subscribe();
                loop {
                    if *changes.borrow_and_update() >= deadline {
                        return;
                    }
                    // self 持有发送端，等待期间通道不会关闭。订阅先于读取，避免丢失唤醒。
                    let _ = changes.changed().await;
                }
            }
        }
    }
}

pub struct ClockChanges {
    clock: Clock,
    manual: Option<watch::Receiver<UnixMillis>>,
}

impl ClockChanges {
    /// Manual changes wake immediately. System time is sampled at the deadline or
    /// after one second, whichever comes first; no deadline means sampling only.
    pub async fn changed(&mut self, deadline: Option<UnixMillis>) -> UnixMillis {
        if let Some(time) = &mut self.manual {
            let _ = time.changed().await;
            *time.borrow_and_update()
        } else {
            let millis = if let Some(deadline) = deadline {
                let now = self.clock.now();
                if now >= deadline {
                    return now;
                }
                let remaining = deadline.0.checked_sub(now.0).unwrap_or(i64::MAX);
                u64::try_from(remaining.min(1000)).unwrap_or(1000)
            } else {
                1000
            };
            tokio::time::sleep(Duration::from_millis(millis)).await;
            self.clock.now()
        }
    }
}

#[allow(clippy::disallowed_methods)] // 只有时钟模块可以读取系统时间。
fn system_now() -> UnixMillis {
    system_time_millis(SystemTime::now())
}

fn system_time_millis(time: SystemTime) -> UnixMillis {
    let millis = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::try_from(duration.as_millis()).unwrap_or(i128::MAX),
        Err(before_epoch) => i128::try_from(before_epoch.duration().as_millis())
            .unwrap_or(i128::MAX)
            .checked_neg()
            .unwrap_or(i128::MIN),
    };
    UnixMillis(i64::try_from(millis).unwrap_or(if millis < 0 { i64::MIN } else { i64::MAX }))
}

#[derive(Clone, Debug)]
pub struct ManualClock {
    time: watch::Sender<UnixMillis>,
}

impl ManualClock {
    pub fn new(start: UnixMillis) -> ManualClock {
        let (time, _) = watch::channel(start);
        Self { time }
    }

    pub fn clock(&self) -> Clock {
        Clock {
            source: Source::Manual(self.time.clone()),
        }
    }

    pub fn set(&self, now: UnixMillis) {
        self.time.send_replace(now);
    }

    /// 丢弃 Duration 的不足 1ms 部分；换算或相加超出范围时停在 i64::MAX。
    /// 更新在同一个 watch 写锁内完成，并通知所有等待者；不发生 panic。
    pub fn advance(&self, by: Duration) {
        let millis = i128::try_from(by.as_millis()).unwrap_or(i128::MAX);
        self.time.send_modify(|time| {
            let advanced = i128::from(time.0).checked_add(millis).unwrap_or(i128::MAX);
            time.0 = i64::try_from(advanced).unwrap_or(i64::MAX);
        });
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Wake, Waker};

    use super::*;

    #[derive(Default)]
    struct WakeCount(AtomicUsize);

    impl Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn advancing_manual_time_wakes_registered_waiters() {
        let manual = ManualClock::new(UnixMillis(0));
        let clock_a = manual.clock();
        let clock_b = manual.clock();
        let mut wait_a = Box::pin(clock_a.sleep_until(UnixMillis(1)));
        let mut wait_b = Box::pin(clock_b.sleep_until(UnixMillis(1)));
        let count_a = Arc::new(WakeCount::default());
        let count_b = Arc::new(WakeCount::default());
        let waker_a = Waker::from(count_a.clone());
        let waker_b = Waker::from(count_b.clone());
        assert!(
            wait_a
                .as_mut()
                .poll(&mut Context::from_waker(&waker_a))
                .is_pending()
        );
        assert!(
            wait_b
                .as_mut()
                .poll(&mut Context::from_waker(&waker_b))
                .is_pending()
        );
        manual.advance(Duration::from_millis(1));
        assert!(count_a.0.load(Ordering::SeqCst) > 0);
        assert!(count_b.0.load(Ordering::SeqCst) > 0);
        assert!(
            wait_a
                .as_mut()
                .poll(&mut Context::from_waker(&waker_a))
                .is_ready()
        );
        assert!(
            wait_b
                .as_mut()
                .poll(&mut Context::from_waker(&waker_b))
                .is_ready()
        );
    }

    async fn assert_pending(future: Pin<&mut impl Future<Output = ()>>) {
        let mut future = future;
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn manual_wait_finishes_only_at_deadline_and_wakes_all_clocks() {
        let manual = ManualClock::new(UnixMillis(100));
        let first = manual.clock();
        let second = manual.clock();
        let mut wait_a = Box::pin(first.sleep_until(UnixMillis(200)));
        let mut wait_b = Box::pin(second.sleep_until(UnixMillis(200)));
        assert_pending(wait_a.as_mut()).await;
        assert_pending(wait_b.as_mut()).await;
        manual.advance(Duration::from_millis(99));
        assert_pending(wait_a.as_mut()).await;
        assert_pending(wait_b.as_mut()).await;
        manual.advance(Duration::from_millis(1));
        assert_eq!(first.now(), UnixMillis(200));
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(wait_a, wait_b);
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn manual_wait_with_past_deadline_is_immediately_ready() {
        let clock = ManualClock::new(UnixMillis(200)).clock();
        for deadline in [UnixMillis(199), UnixMillis(200)] {
            let mut wait = Box::pin(clock.sleep_until(deadline));
            std::future::poll_fn(|cx| {
                assert!(wait.as_mut().poll(cx).is_ready());
                Poll::Ready(())
            })
            .await;
        }
    }

    #[tokio::test]
    async fn manual_rollback_does_not_complete_a_wait() {
        let manual = ManualClock::new(UnixMillis(100));
        let clock = manual.clock();
        let mut wait = Box::pin(clock.sleep_until(UnixMillis(200)));
        assert_pending(wait.as_mut()).await;
        manual.set(UnixMillis(50));
        assert_eq!(clock.now(), UnixMillis(50));
        assert_pending(wait.as_mut()).await;
        manual.set(UnixMillis(199));
        assert_pending(wait.as_mut()).await;
        manual.set(UnixMillis(200));
        tokio::time::timeout(Duration::from_secs(1), wait)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn change_subscription_reports_rollback_before_a_future_deadline() {
        let manual = ManualClock::new(UnixMillis(100));
        let mut changes = manual.clock().changes();
        // Updates made before the wait begins are retained by the subscription.
        manual.set(UnixMillis(50));
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                changes.changed(Some(UnixMillis(200))),
            )
            .await
            .unwrap(),
            UnixMillis(50)
        );
        manual.set(UnixMillis(200));
        assert_eq!(changes.changed(None).await, UnixMillis(200));
    }

    #[test]
    fn manual_advance_handles_large_durations_without_panic() {
        let manual = ManualClock::new(UnixMillis(i64::MIN));
        manual.advance(Duration::from_millis(1_u64 << 63));
        assert_eq!(manual.clock().now(), UnixMillis(0));
        manual.advance(Duration::MAX);
        assert_eq!(manual.clock().now(), UnixMillis(i64::MAX));
        manual.advance(Duration::from_millis(1));
        assert_eq!(manual.clock().now(), UnixMillis(i64::MAX));
    }

    #[test]
    fn system_time_conversion_supports_both_sides_of_epoch() {
        assert_eq!(system_time_millis(UNIX_EPOCH), UnixMillis(0));
        assert_eq!(
            system_time_millis(UNIX_EPOCH + Duration::from_millis(42)),
            UnixMillis(42)
        );
        assert_eq!(
            system_time_millis(UNIX_EPOCH - Duration::from_millis(42)),
            UnixMillis(-42)
        );
    }
}
