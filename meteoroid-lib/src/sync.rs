pub struct StopSender {
    chan: tokio::sync::oneshot::Sender<tokio::sync::oneshot::Sender<()>>,
}

impl StopSender {
    pub async fn stop(self) -> bool {
        let (send, recv) = tokio::sync::oneshot::channel();
        if self.chan.send(send).is_err() {
            return false;
        }
        recv.await.is_ok()
    }
}

pub struct StopReceiver {
    recv: tokio::sync::oneshot::Receiver<tokio::sync::oneshot::Sender<()>>,
}

#[must_use]
pub fn stop_channel() -> (StopSender, StopReceiver) {
    let (chan, recv) = tokio::sync::oneshot::channel();
    (StopSender { chan }, StopReceiver { recv })
}

impl StopReceiver {
    /// Future needs to be cancel safe
    pub(crate) async fn with_stop<T, F: Future<Output = T>>(&mut self, future: F) -> Option<T> {
        tokio::select! {
            msg = &mut self.recv => {
               if let Ok(sender)  = msg {
                    sender.send(()).ok();
                }
                None
            },
            output = future => Some(output)
        }
    }
}
