use grafton_ndi::NDI;

fn main() {
    drop(NDI { _private: () });
}
