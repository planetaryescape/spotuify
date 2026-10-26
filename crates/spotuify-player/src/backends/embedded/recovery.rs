use std::time::Duration;

use tokio::task::JoinHandle;

use crate::{PlayerError, PlayerResult};

pub(super) async fn finish_spirc(
    task: &mut JoinHandle<()>,
    deadline: Duration,
) -> PlayerResult<()> {
    match tokio::time::timeout(deadline, &mut *task).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(PlayerError::Playback(format!(
            "player exited before saving its queue: {error}"
        ))),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(PlayerError::Playback(
                "timed out saving the player queue for reconnect".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn reconnect_waits_for_native_state_capture() {
        let captured = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let captured_in_task = captured.clone();
        let mut task = tokio::spawn(async move {
            tokio::task::yield_now().await;
            captured_in_task.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        finish_spirc(&mut task, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(captured.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stalled_native_state_capture_is_bounded_and_cancelled() {
        let mut task = tokio::spawn(std::future::pending());
        let error = finish_spirc(&mut task, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out saving"));
        assert!(task.is_finished());
    }
}
