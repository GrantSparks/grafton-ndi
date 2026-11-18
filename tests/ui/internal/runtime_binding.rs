//! Compiled as a sibling of runtime: binding fields stay private there.
#![allow(dead_code)]
use crate::{runtime::Bound, sys, Ndi, Result};

fn borrowed_api(ndi: &Ndi) -> &sys::Api {
    ndi.api()
}

fn borrowed_optional(ndi: &Ndi) -> Result<&sys::Routing> {
    ndi.table()
}

fn retained_core(ndi: &Ndi) -> Bound<sys::Core> {
    ndi.bind_core()
}

#[cfg(ndi_test_runtime_binding)]
fn escaping_api(ndi: &Ndi) -> &'static sys::Api {
    ndi.api()
}

#[cfg(ndi_test_runtime_binding)]
fn escaping_optional(ndi: &Ndi) -> Result<&'static sys::Routing> {
    ndi.table()
}

#[cfg(ndi_test_runtime_binding)]
fn escaping_bound(bound: &Bound<sys::Core>) -> &'static sys::Core {
    bound
}

#[cfg(ndi_test_runtime_binding_privacy)]
fn forge_binding(ndi: Ndi) -> Bound<sys::Core> {
    Bound {
        owner: ndi,
        table: (),
    }
}

#[cfg(ndi_test_runtime_binding)]
fn copied_core(ndi: &Ndi) -> sys::Core {
    let copied_core = ndi.api().core;
    copied_core
}

#[cfg(ndi_test_runtime_binding)]
fn copied_optional(ndi: &Ndi) -> Result<sys::Routing> {
    let copied_optional = ndi.table().copied();
    copied_optional
}

#[cfg(ndi_test_runtime_binding)]
fn copied_bound(bound: &Bound<sys::Core>) -> sys::Core {
    let copied_bound = **bound;
    copied_bound
}

#[cfg(ndi_test_runtime_binding)]
fn detach(ndi: Ndi) -> sys::Core {
    let detach = ndi.api().core;
    drop(ndi);
    detach
}
