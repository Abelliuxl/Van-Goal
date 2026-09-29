fn main() {
    println!("cargo:rerun-if-changed=../../assets/VanGoal.ico");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/VanGoal.ico");
        resource.set("FileDescription", "Van-Goal");
        resource.set("ProductName", "Van-Goal");
        resource
            .compile()
            .expect("failed to embed the Van-Goal Windows icon");
    }
}
