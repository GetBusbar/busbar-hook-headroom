use std::cell::RefCell;

// Stable, interior-mutable storage for the backend's advancing input slice.
// RefCell supplies UnsafeCell storage here, not a dynamic borrow guard.
pub(crate) struct SliceRefStorage<'input> {
    // Store the slice reference in a RefCell for interior mutability
    // This allows libyaml_safer to modify the inner &[u8] reference
    // The field is prefixed with _ to indicate it's kept alive for lifetime reasons
    _cell: Box<RefCell<&'input [u8]>>,
}

impl<'input> SliceRefStorage<'input> {
    /// # Safety
    /// The caller must retain this storage until the returned exclusive reference
    /// is no longer used. Its inferred lifetime is not a borrow of the storage.
    /// No second access to the cell may occur while that reference is live.
    pub(crate) unsafe fn new(slice: &'input [u8]) -> (Self, &'input mut &'input [u8]) {
        let cell = Box::new(RefCell::new(slice));
        // The allocation stays stable when Self moves. The caller, rather than
        // RefCell's runtime checks, must enforce the lifetime and exclusivity.
        let slice_ref_mut = unsafe { &mut *RefCell::as_ptr(&cell) };
        (Self { _cell: cell }, slice_ref_mut)
    }
}
