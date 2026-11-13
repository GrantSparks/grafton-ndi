//! Metadata a router replays to each new connection.

use crate::{
    routing::Router,
    sys::advanced::{native, RouterConnectionMetadata},
    MetadataSource, Result,
};

impl Router {
    /// Send `metadata` to every connected receiver now, and to each receiver
    /// that connects later.
    ///
    /// The SDK copies owned or captured text during the call; the input may
    /// be released immediately afterward, even for future connections.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime (or in a build without the Advanced SDK).
    pub fn add_connection_metadata<S: MetadataSource + ?Sized>(&self, metadata: &S) -> Result<()> {
        let table = self.runtime().table::<RouterConnectionMetadata>()?;
        let router = self.native_instance();
        let metadata = metadata.metadata_raw();
        // SAFETY: the router is live for this borrow, and the descriptor
        // borrows the caller's frame, which outlives the call.
        native!(RouterConnectionMetadata, table => unsafe {
            table.NDIlib_routing_add_connection_metadata(router, &metadata)
        });
        Ok(())
    }

    /// Forget the metadata added with
    /// [`add_connection_metadata`](Self::add_connection_metadata).
    ///
    /// # Errors
    ///
    /// As for [`add_connection_metadata`](Self::add_connection_metadata).
    pub fn clear_connection_metadata(&self) -> Result<()> {
        let table = self.runtime().table::<RouterConnectionMetadata>()?;
        let router = self.native_instance();
        // SAFETY: the router is live for this borrow.
        native!(RouterConnectionMetadata, table => unsafe { table.NDIlib_routing_clear_connection_metadata(router) });
        Ok(())
    }
}
