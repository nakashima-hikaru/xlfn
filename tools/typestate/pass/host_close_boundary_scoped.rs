// module: boundary/host
fn scoped<A: crate::Addin>(runtime: &crate::runtime::Runtime<A>) {
    super::with_host_lifecycle(std::ptr::from_ref(runtime).cast(), |frame| {
        let boundary = super::HostCloseBoundary { frame };
        assert!(boundary.belongs_to(runtime));
    });
}
