//! Typed tables of native entry points and the trait their providers implement.

use std::{
    ffi::{c_void, CStr},
    ptr::NonNull,
};

use crate::LoadRefusal;

/// The native SDK entry point that produced a result.
///
/// Values originate in the resolved native tables, so a reported operation
/// retains the exact name of the function that was called.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NativeFunction(&'static CStr);

impl NativeFunction {
    pub(in crate::sys) const fn new(name: &'static CStr) -> Self {
        Self(name)
    }

    /// The SDK entry point name, including its C-string terminator.
    pub const fn name(self) -> &'static CStr {
        self.0
    }
}

impl std::fmt::Display for NativeFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.to_string_lossy().fmt(f)
    }
}

impl std::fmt::Debug for NativeFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("NativeFunction").field(&self.0).finish()
    }
}

impl PartialEq<&CStr> for NativeFunction {
    fn eq(&self, other: &&CStr) -> bool {
        self.0 == *other
    }
}

/// A table view whose calls retain their entry-point identity.
pub(crate) struct Named<'a, T>(pub(in crate::sys) &'a T);

/// One native result together with the entry point that produced it.
#[derive(Clone, Copy)]
pub(crate) struct Called<R> {
    pub(in crate::sys) value: R,
    pub(in crate::sys) function: NativeFunction,
}

#[allow(dead_code)]
impl<R> Called<R> {
    pub(crate) fn function(&self) -> NativeFunction {
        self.function
    }

    pub(crate) fn value(&self) -> &R {
        &self.value
    }

    pub(crate) fn into_value(self) -> R {
        self.value
    }

    pub(crate) fn into_parts(self) -> (R, NativeFunction) {
        (self.value, self.function)
    }

    pub(crate) fn map<T>(self, map: impl FnOnce(R) -> T) -> Called<T> {
        Called {
            value: map(self.value),
            function: self.function,
        }
    }

    pub(crate) fn failed(&self) -> crate::Error {
        crate::Error::NativeOperationFailed {
            operation: self.function,
        }
    }
}

#[allow(dead_code)]
impl Called<bool> {
    pub(crate) fn performed(self) -> crate::Result<()> {
        if self.value {
            Ok(())
        } else {
            Err(self.failed())
        }
    }
}

#[allow(dead_code)]
impl<T> Called<*mut T> {
    pub(crate) fn created(self) -> crate::Result<NonNull<T>> {
        NonNull::new(self.value).ok_or_else(|| self.failed())
    }
}

#[allow(dead_code)]
impl<T> Called<*const T> {
    pub(crate) fn created(self) -> crate::Result<NonNull<T>> {
        NonNull::new(self.value.cast_mut()).ok_or_else(|| self.failed())
    }
}

/// A source of native entry points: a pinned shared module or the statically
/// linked SDK.
pub(crate) trait Resolver {
    /// The address of `name`, or `None` when the implementation does not export it.
    /// Names come from static entry-point declarations, so refusals retain the
    /// exact C string without allocating a copy.
    ///
    /// # Errors
    ///
    /// Fails when the name resolves to code outside the selected implementation.
    fn resolve(
        &self,
        name: &'static CStr,
    ) -> std::result::Result<Option<NonNull<c_void>>, LoadRefusal>;
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
    (@return) => { () };
    (@return $return:ty) => { $return };
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

        #[allow(non_snake_case, clippy::too_many_arguments)]
        impl $Table {
            /// Call entries while retaining their native function identity.
            #[allow(dead_code)]
            pub(crate) fn named(&self) -> $crate::sys::table::Named<'_, Self> {
                $crate::sys::table::Named(self)
            }

            /// Exported names, in declaration order.
            #[cfg(test)]
            pub(crate) const SYMBOLS: &'static [&'static ::std::ffi::CStr] = &[
                $($crate::sys::table::symbol_name(concat!(stringify!($name), "\0")),)+
            ];

            /// Resolve every entry point, or name the first one `resolver` lacks.
            pub(crate) fn load(
                resolver: &dyn $crate::sys::table::Resolver,
            ) -> ::std::result::Result<::std::result::Result<Self, &'static ::std::ffi::CStr>, $crate::LoadRefusal> {
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
                #[allow(dead_code)]
                pub(crate) unsafe fn $name(&self, $($argument: $type),*) $(-> $return)? {
                    // SAFETY: forwarded to the caller's obligations above.
                    unsafe { (self.$name)($($argument),*) }
                }
            )+
        }

        #[allow(non_snake_case, clippy::too_many_arguments, dead_code)]
        impl $crate::sys::table::Named<'_, $Table> {
            $(
                /// Call this SDK entry point and retain its name.
                ///
                /// # Safety
                ///
                /// The caller upholds the SDK's contract for this function.
                #[inline]
                pub(crate) unsafe fn $name(&self, $($argument: $type),*)
                    -> $crate::sys::table::Called<$crate::sys::table::native_table!(@return $($return)?)>
                {
                    $crate::sys::table::Called {
                        // SAFETY: forwarded to the caller's obligations above.
                        value: unsafe { (self.0.$name)($($argument),*) },
                        function: $crate::sys::table::NativeFunction::new(
                            $crate::sys::table::symbol_name(concat!(stringify!($name), "\0")),
                        ),
                    }
                }
            )+
        }
    };
}

pub(crate) use native_table;

/// Construct a synthetic native result for tests of SDK-output policies.
#[cfg(test)]
pub(crate) fn for_test<R>(value: R, name: &'static CStr) -> Called<R> {
    Called {
        value,
        function: NativeFunction::new(name),
    }
}

/// A provider for unit tests that exports the names a predicate accepts. The
/// addresses it returns are placeholders and must never be called.
#[cfg(test)]
pub(crate) struct ExportsWhere<F: Fn(&str) -> bool>(pub(crate) F);

#[cfg(test)]
impl<F: Fn(&str) -> bool> Resolver for ExportsWhere<F> {
    fn resolve(
        &self,
        name: &'static CStr,
    ) -> std::result::Result<Option<NonNull<c_void>>, LoadRefusal> {
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
    fn a_loaded_table_forwards_arguments_and_return_values() {
        unsafe extern "C" fn initialize() -> bool {
            true
        }
        unsafe extern "C" fn destroy(instance: NDIlib_recv_instance_t) {
            // SAFETY: this stub receives the uniquely borrowed flag below,
            // passed through the opaque native handle without dereferencing it.
            unsafe { instance.cast::<bool>().write(true) };
        }

        struct Stubs;
        impl Resolver for Stubs {
            fn resolve(
                &self,
                name: &'static CStr,
            ) -> std::result::Result<Option<NonNull<c_void>>, LoadRefusal> {
                Ok(NonNull::new(match name.to_bytes() {
                    b"NDIlib_initialize" => initialize as *mut c_void,
                    b"NDIlib_recv_destroy" => destroy as *mut c_void,
                    _ => return Ok(None),
                }))
            }
        }

        let table = Pair::load(&Stubs).unwrap().ok().unwrap();
        let mut destroyed = false;
        // SAFETY: this table resolved the callable stubs above. Initialization
        // has no preconditions, and destruction receives the flag it expects.
        unsafe {
            assert!(table.NDIlib_initialize());
            let called = table.named().NDIlib_initialize();
            assert_eq!(called.function(), c"NDIlib_initialize");
            assert!(called.into_value());
            let called = table
                .named()
                .NDIlib_recv_destroy(std::ptr::from_mut(&mut destroyed).cast());
            assert_eq!(called.function().name(), c"NDIlib_recv_destroy");
            called.into_value();
            table.NDIlib_recv_destroy(std::ptr::from_mut(&mut destroyed).cast());
        }
        assert!(destroyed);
    }

    #[test]
    fn a_provenance_failure_is_an_error_not_a_missing_symbol() {
        struct Foreign;
        impl Resolver for Foreign {
            fn resolve(
                &self,
                _name: &'static CStr,
            ) -> std::result::Result<Option<NonNull<c_void>>, LoadRefusal> {
                Err(LoadRefusal::EntryPointOutsideModule {
                    symbol: c"NDIlib_initialize",
                    module: "selected module".into(),
                })
            }
        }
        assert!(Pair::load(&Foreign).is_err());
    }
}
