use grafton_ndi::{MetadataFrame, MetadataFrameRef, MetadataSource, Receiver, Sender};

fn forward<S: MetadataSource + ?Sized>(sender: &Sender, receiver: &Receiver, metadata: &S) {
    sender.send_metadata(metadata);
    sender.add_connection_metadata(metadata);
    let _ = receiver.send_metadata(metadata);
    let _ = receiver.add_connection_metadata(metadata);
    let mut owned = MetadataSource::to_owned(metadata);
    metadata.copy_into(&mut owned);
    assert_eq!(owned.as_bytes(), metadata.as_bytes());
}

fn captured(sender: &Sender, receiver: &Receiver, metadata: &MetadataFrameRef<'_>) {
    forward(sender, receiver, metadata);
    forward(sender, receiver, metadata as &dyn MetadataSource);
}

fn main() {
    let _ = captured;
    let owned = MetadataFrame::new("<metadata/>").unwrap();
    let source: &dyn MetadataSource = &owned;
    assert_eq!(source.to_owned(), owned.to_owned());
}
