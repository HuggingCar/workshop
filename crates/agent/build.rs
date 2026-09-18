fn main() {
    #[cfg(windows)]
    {
        let icon = "../../packaging/huggingcar-agent.ico";
        println!("cargo:rerun-if-changed={icon}");
        winresource::WindowsResource::new()
            .set_icon(icon)
            .set("ProductName", "HuggingCar Agent")
            .set("FileDescription", "HuggingCar Agent")
            .set("CompanyName", "HuggingCar")
            .compile()
            .expect("compile Windows application resources");
    }
}
