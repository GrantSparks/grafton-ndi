use grafton_ndi::discovery::{InputGroupId, ReceiverId, RecvListener, SendListener, SenderId};
fn mix(
    senders: &mut SendListener,
    receivers: &mut RecvListener,
    sender: &SenderId,
    receiver: &ReceiverId,
    group: &InputGroupId,
) {
    senders.subscribe(receiver);
    senders.unsubscribe(group);
    receivers.subscribe(sender);
    receivers.unsubscribe(group);
    let _ = receivers.request_connect(sender, "Camera 1");
    let _ = receivers.request_disconnect(group);
}
fn main() {}
