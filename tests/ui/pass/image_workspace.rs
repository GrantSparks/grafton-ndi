#[cfg(feature = "image-encoding")]
fn encode(source: &dyn grafton_ndi::VideoSource) {
    use grafton_ndi::{ImageFormat, ImageOptions, ImageWorkspace};
    let mut workspace = ImageWorkspace::new();
    let options = ImageOptions::new(ImageFormat::Png);
    let _ = workspace.encode(source, &options);
    let _ = workspace.encode_to(source, &options, Vec::new());
    workspace.release();
}

fn main() {
    #[cfg(feature = "image-encoding")]
    let _ = encode;
}
