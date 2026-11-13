use grafton_ndi::{Receiver, ReceiverItem, ReceiverItemRef, Result};
use std::time::Duration;

fn borrowed(receiver: &Receiver) -> Result<()> {
    if let Some(item) = receiver.capture_any_ref(Duration::ZERO)? {
        std::thread::scope(|scope| {
            scope
                .spawn(move || match item {
                    ReceiverItemRef::Video(frame) => assert!(frame.width() > 0),
                    ReceiverItemRef::Audio(frame) => assert!(frame.num_channels() > 0),
                    ReceiverItemRef::Metadata(frame) => {
                        let _ = frame.data();
                    }
                    ReceiverItemRef::Event(event) => {
                        let _ = event;
                    }
                    _ => {}
                })
                .join()
                .unwrap();
        });
    }
    Ok(())
}

fn owned(receiver: &Receiver) -> Result<Option<ReceiverItem>> {
    receiver.capture_any(Duration::ZERO)
}

#[cfg(any(feature = "tokio", feature = "smol"))]
async fn asynchronous<R: grafton_ndi::asynchronous::AsyncRuntime>(
    receiver: &grafton_ndi::asynchronous::AsyncReceiver<R>,
) -> Result<Option<ReceiverItem>> {
    let item = receiver.capture_any(Duration::ZERO).await?;
    fn owned_static<T: Send + 'static>(item: T) -> T {
        item
    }
    Ok(owned_static(item))
}

fn main() {}
