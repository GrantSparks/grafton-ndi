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
    /// The router keeps its own copy of owned or captured text; the input
    /// may be released immediately afterward, even for future connections.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime (or in a build without the Advanced SDK).
    #[doc(alias = "NDIlib_routing_add_connection_metadata")]
    pub fn add_connection_metadata<S: MetadataSource + ?Sized>(
        &mut self,
        metadata: &S,
    ) -> Result<()> {
        let table = self.runtime().table::<RouterConnectionMetadata>()?;
        native!(RouterConnectionMetadata, table, uses(metadata) => {
            let table = *table;
            let router = self.native_instance();
            self.connection_metadata.add(metadata, |metadata| {
                // SAFETY: the router owns this stable descriptor and its text
                // until native clear returns or after native destruction.
                unsafe { table.NDIlib_routing_add_connection_metadata(router, metadata) };
            });
            Ok(())
        })
    }

    /// Forget the metadata added with
    /// [`add_connection_metadata`](Self::add_connection_metadata).
    ///
    /// # Errors
    ///
    /// As for [`add_connection_metadata`](Self::add_connection_metadata).
    #[doc(alias = "NDIlib_routing_clear_connection_metadata")]
    pub fn clear_connection_metadata(&mut self) -> Result<()> {
        let table = self.runtime().table::<RouterConnectionMetadata>()?;
        native!(RouterConnectionMetadata, table => {
            let table = *table;
            let router = self.native_instance();
            self.connection_metadata.clear(|| {
                // SAFETY: the router and its registrations remain live until
                // this native clear returns.
                unsafe { table.NDIlib_routing_clear_connection_metadata(router) };
            });
            Ok(())
        })
    }
}
