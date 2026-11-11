//! Typed tables of native entry points and the trait their providers implement.

use std::{
    ffi::{c_void, CStr},
    ptr::NonNull,
};

use crate::Result;

/// A source of native entry points: a pinned shared module or the statically
/// linked SDK.
pub(crate) trait Resolver {
    /// The address of `name`, or `None` when the implementation does not export it.
    ///
    /// # Errors
    ///
    /// Fails when the name resolves to code outside the selected implementation.
    fn resolve(&self, name: &CStr) -> Result<Option<NonNull<c_void>>>;
}

/// A table's exported symbol name, validated at compile time.
pub(crate) const fn symbol_name(nul_terminated: &'static str) -> &'static CStr {
    match CStr::from_bytes_with_nul(nul_terminated.as_bytes()) {
        Ok(name) => name,
        Err(_) => panic!("a native symbol name must end with its only NUL"),
    }
}

/// Define a table of native functions that is resolved as one unit.
///
/// Every signature is checked at compile time against the declaration bindgen
/// produced from the SDK header in the `from` module, so a table cannot drift
/// from the ABI. That check is the only use of the declarations on dynamically
/// loaded targets, where nothing links them. Call an entry through the method of the same name; the field
/// holds the raw pointer for the rare caller that passes it on.
macro_rules! native_table {
    (
        $(#[$meta:meta])*
        $vis:vis struct $Table:ident from $($declarations:ident)::+ {
            $(
                $(#[$function_meta:meta])*
                fn $name:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)?;
            )+
        }
    ) => {
        $(#[$meta])*
        #[allow(non_snake_case)]
        #[derive(Clone, Copy)]
        $vis struct $Table {
            $(pub(crate) $name: unsafe extern "C" fn($($type),*) $(-> $return)?,)+
        }

        const _: () = {
            use $($declarations)::+ as declarations;
            $(const _: unsafe extern "C" fn($($type),*) $(-> $return)? = declarations::$name;)+
        };

        #[allow(non_snake_case, dead_code, clippy::too_many_arguments)]
        impl $Table {
            /// Exported names, in declaration order.
            pub(crate) const SYMBOLS: &'static [&'static ::std::ffi::CStr] = &[
                $($crate::sys::table::symbol_name(concat!(stringify!($name), "\0")),)+
            ];

            /// Resolve every entry point, or name the first one `resolver` lacks.
            pub(crate) fn load(
                resolver: &dyn $crate::sys::table::Resolver,
            ) -> $crate::Result<::std::result::Result<Self, &'static ::std::ffi::CStr>> {
                Ok(Ok(Self {
                    $($name: {
                        const NAME: &::std::ffi::CStr =
                            $crate::sys::table::symbol_name(concat!(stringify!($name), "\0"));
                        match resolver.resolve(NAME)? {
                            // SAFETY: the resolver returned the address the selected SDK
                            // implementation exports under this name, and the target type
                            // is the signature its header declares (asserted above).
                            Some(address) => unsafe {
                                ::std::mem::transmute::<
                                    *mut ::std::ffi::c_void,
                                    unsafe extern "C" fn($($type),*) $(-> $return)?,
                                >(address.as_ptr())
                            },
                            None => return Ok(Err(NAME)),
                        }
                    },)+
                }))
            }

            $(
                $(#[$function_meta])*
                ///
                /// # Safety
                ///
                /// The caller upholds the SDK's contract for this function.
                #[inline]
                pub(crate) unsafe fn $name(&self, $($argument: $type),*) $(-> $return)? {
                    // SAFETY: forwarded to the caller's obligations above.
                    unsafe { (self.$name)($($argument),*) }
                }
            )+
        }
    };
}

pub(crate) use native_table;

/// A provider for unit tests that exports the names a predicate accepts. The
/// addresses it returns are placeholders and must never be called.
#[cfg(test)]
pub(crate) struct ExportsWhere<F: Fn(&str) -> bool>(pub(crate) F);

#[cfg(test)]
impl<F: Fn(&str) -> bool> Resolver for ExportsWhere<F> {
    fn resolve(&self, name: &CStr) -> Result<Option<NonNull<c_void>>> {
        let exported = (self.0)(name.to_str().expect("symbol names are ASCII"));
        Ok(exported.then(NonNull::dangling))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::bindings::*;

    native_table! {
        /// Two entry points, to exercise the generated loader.
        struct Pair from crate::sys::declarations {
            fn NDIlib_initialize() -> bool;
            fn NDIlib_recv_destroy(p_instance: NDIlib_recv_instance_t);
        }
    }

    #[test]
    fn a_table_lists_its_symbols_in_declaration_order() {
        assert_eq!(
            Pair::SYMBOLS,
            [c"NDIlib_initialize", c"NDIlib_recv_destroy"]
        );
    }

    #[test]
    fn a_table_loads_only_when_every_symbol_resolves() {
        assert!(Pair::load(&ExportsWhere(|_| true)).unwrap().is_ok());
        let missing = Pair::load(&ExportsWhere(|name| name != "NDIlib_recv_destroy"))
            .unwrap()
            .err();
        assert_eq!(missing, Some(c"NDIlib_recv_destroy"));
    }

    #[test]
    fn a_provenance_failure_is_an_error_not_a_missing_symbol() {
        struct Foreign;
        impl Resolver for Foreign {
            fn resolve(&self, _name: &CStr) -> Result<Option<NonNull<c_void>>> {
                Err(crate::Error::RuntimeLoadFailed("outside the module".into()))
            }
        }
        assert!(Pair::load(&Foreign).is_err());
    }
}
