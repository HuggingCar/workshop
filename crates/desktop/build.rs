fn main() {
    println!("cargo:rerun-if-changed=../../packaging/huggingcar-fiscal.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("../../packaging/huggingcar-fiscal.ico")
            .set("ProductName", "HuggingCar Fiscal")
            .set("FileDescription", "HuggingCar Fiscal")
            .compile()
            .expect("Windows desktop icon and version resources");
    }
}
