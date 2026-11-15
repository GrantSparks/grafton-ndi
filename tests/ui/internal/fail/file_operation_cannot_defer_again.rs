fn main() {
    let _ = FileOperation::<()>::deferred(|| {
        FileOperation::deferred(|| Live::Immediate(Ok(())))
    });
}
