//! One bounded FIFO sender. Only shutdown waits for delivery, with a fixed deadline.
use crate::util;
use anyhow::Result;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const CAPACITY: usize = 64;
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(17);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);
const FINAL_RESERVE: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Pending {
    messages: VecDeque<String>,
    closing: Option<(Instant, Option<String>)>,
}

pub(crate) struct Notifier {
    pending: Arc<(Mutex<Pending>, Condvar)>,
    done: mpsc::Receiver<()>,
    worker: Option<JoinHandle<()>>,
    drain: Duration,
}

impl Notifier {
    pub(crate) fn new(path: PathBuf) -> Result<Self> {
        Self::start(
            move |text, deadline| util::webhook_until(&path, text, deadline),
            DRAIN_TIMEOUT,
            FINAL_RESERVE,
        )
    }

    fn start(
        send: impl Fn(&str, Instant) -> Result<()> + Send + 'static,
        drain: Duration,
        reserve: Duration,
    ) -> Result<Self> {
        let pending = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let state = pending.clone();
        let (tx, done) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("notifier".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                deliver(&state, send, reserve);
                let _ = tx.send(());
            })?;
        Ok(Self {
            pending,
            done,
            worker: Some(worker),
            drain,
        })
    }

    pub(crate) fn send(&self, text: String) {
        let (lock, ready) = &*self.pending;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closing.is_none() && state.messages.len() < CAPACITY {
            state.messages.push_back(text);
            ready.notify_one();
        } else {
            eprintln!("Discord通知キューの上限または終了中のため通知を破棄しました");
        }
    }

    fn close(&self, final_message: Option<String>) -> Instant {
        let (lock, ready) = &*self.pending;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (deadline, _) = state
            .closing
            .get_or_insert((Instant::now() + self.drain, final_message));
        let deadline = *deadline;
        ready.notify_one();
        deadline
    }

    pub(crate) fn finish(mut self, final_message: Option<String>) {
        let deadline = self.close(final_message);
        if self
            .done
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
        {
            if self.worker.take().unwrap().join().is_err() {
                eprintln!("Discord通知スレッドが異常終了しました");
            }
        } else {
            eprintln!("Discord通知の終了待ちが期限を超えたか、送信スレッドが異常終了しました");
        }
    }
}

impl Drop for Notifier {
    fn drop(&mut self) {
        // Also release an idle worker when startup fails before supervision begins.
        self.close(None);
    }
}

fn deliver(
    pending: &(Mutex<Pending>, Condvar),
    send: impl Fn(&str, Instant) -> Result<()>,
    reserve: Duration,
) {
    let (lock, ready) = pending;
    loop {
        let mut state = ready
            .wait_while(
                lock.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                |s| s.messages.is_empty() && s.closing.is_none(),
            )
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ordinary_deadline = state.closing.as_ref().map(|(deadline, final_message)| {
            if final_message.is_some() {
                *deadline - reserve
            } else {
                *deadline
            }
        });
        if ordinary_deadline.is_some_and(|deadline| Instant::now() >= deadline)
            && !state.messages.is_empty()
        {
            eprintln!(
                "Discord通知の終了期限のため未送信の{}件を破棄しました",
                state.messages.len()
            );
            state.messages.clear();
        }
        let (message, deadline, last) = if let Some(text) = state.messages.pop_front() {
            (
                text,
                ordinary_deadline.unwrap_or_else(|| Instant::now() + MESSAGE_TIMEOUT),
                false,
            )
        } else if let Some((deadline, final_message)) = &mut state.closing {
            match final_message.take() {
                Some(text) => (text, *deadline, true),
                None => return,
            }
        } else {
            continue;
        };
        drop(state);
        if let Err(error) = send(&message, deadline) {
            eprintln!("Discord通知に失敗しました（サーバー操作は継続します）: {error:#}");
        }
        if last {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_messages_finish_in_order_and_stop_has_a_reserved_slot() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let recorded = sent.clone();
        let (entered_tx, entered) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let notifier = Notifier::start(
            move |text, _| {
                if text == "first" {
                    entered_tx.send(()).unwrap();
                    gate.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                recorded.lock().unwrap().push(text.to_string());
                Ok(())
            },
            Duration::from_secs(2),
            Duration::from_millis(100),
        )
        .unwrap();
        notifier.send("first".into());
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        for n in 0..CAPACITY {
            notifier.send(format!("logout-{n}"));
        }
        notifier.send("overflow".into());
        release.send(()).unwrap();
        notifier.finish(Some("stop".into()));
        let mut expected = vec!["first".to_string()];
        expected.extend((0..CAPACITY).map(|n| format!("logout-{n}")));
        expected.push("stop".into());
        assert_eq!(*sent.lock().unwrap(), expected);
    }

    #[test]
    fn final_notification_keeps_time_when_ordinary_delivery_exhausts_its_budget() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let recorded = sent.clone();
        let notifier = Notifier::start(
            move |text, deadline| {
                recorded.lock().unwrap().push(text.to_string());
                if text == "slow" {
                    thread::sleep(deadline.saturating_duration_since(Instant::now()));
                    anyhow::bail!("simulated network timeout")
                }
                Ok(())
            },
            Duration::from_millis(250),
            Duration::from_millis(150),
        )
        .unwrap();
        // Freeze the worker until the closing deadline is established.
        {
            let mut pending = notifier.pending.0.lock().unwrap();
            pending.messages.extend(["slow".into(), "skipped".into()]);
            pending.closing = Some((Instant::now() + notifier.drain, Some("stop".into())));
        }
        notifier.finish(None);
        assert_eq!(*sent.lock().unwrap(), ["slow", "stop"]);
    }

    #[test]
    fn unresponsive_sender_cannot_hold_shutdown_open() {
        let (release, gate) = mpsc::channel();
        let (entered_tx, entered) = mpsc::channel();
        let (exited_tx, exited) = mpsc::channel();
        let notifier = Notifier::start(
            move |_, _| {
                entered_tx.send(()).unwrap();
                gate.recv_timeout(Duration::from_secs(2)).unwrap();
                exited_tx.send(()).unwrap();
                Ok(())
            },
            Duration::from_millis(40),
            Duration::ZERO,
        )
        .unwrap();
        notifier.send("blocked".into());
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        let start = Instant::now();
        notifier.finish(None);
        assert!(start.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
        exited.recv_timeout(Duration::from_secs(2)).unwrap();
    }
}
