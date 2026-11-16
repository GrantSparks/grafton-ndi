#[cfg(feature = "image-encoding")]
fn encode(source: &dyn grafton_ndi::VideoSource) {
    use grafton_ndi::image_encoding::{ImageFormat, ImageOptions, ImageWorkspace};
    let mut workspace = ImageWorkspace::new();
    let options = ImageOptions::new(ImageFormat::Png);
    let _ = workspace.encode(source, &options);
    let _ = workspace.encode_to(source, &options, Vec::new());
    let _ = workspace.encode_data_url(source, &options);
    let _ = source.encode_image(&options);
    let _ = source.encode_image_data_url(&options);
    workspace.release();
}

fn main() {
    #[cfg(feature = "image-encoding")]
    let _ = encode;
}

#[cfg(feature = "image-encoding")]
fn generic<T: grafton_ndi::VideoSource>(source: &T) {
    use grafton_ndi::image_encoding::{ImageFormat, ImageOptions};
    let options = ImageOptions::new(ImageFormat::Png);
    let _ = source.encode_image(&options);
    let _ = source.encode_image_to(&options, Vec::new());
    let _ = source.encode_image_data_url(&options);
}
