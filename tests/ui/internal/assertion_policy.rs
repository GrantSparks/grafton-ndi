const_assert!(usize::BITS >= 32);

#[cfg(ndi_test_release_assertion)]
#[used]
static CHECK: fn(bool) = |condition| assert!(condition);
