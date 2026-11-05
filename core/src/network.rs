use anyhow::Result;
use iroh::{Endpoint, PublicKey as NodeId};
use iroh_blobs::{
    BlobsProtocol,
    store::fs::FsStore,
    ticket::BlobTicket,
    Hash,
};
use std::path::Path;
use tokio::fs;

pub struct P2pNetwork {
    endpoint: Endpoint,
    store: iroh_blobs::api::Store,
    _router: iroh::protocol::Router,
    node_id: NodeId,
}

impl P2pNetwork {
    pub async fn start(blob_store_path: &str) -> Result<Self> {
        let blobs_dir = std::path::PathBuf::from(blob_store_path);
        fs::create_dir_all(&blobs_dir).await?;

        
        let fs_store = FsStore::load(&blobs_dir).await?;
        let store: iroh_blobs::api::Store = fs_store.into();

        println!("💾 Persistenter DiskStore aktiv unter: \"{}\"", blob_store_path);

        let endpoint = Endpoint::builder(iroh::endpoint::presets::N0).bind().await?;
        let node_id = endpoint.id();

        let blobs_protocol = BlobsProtocol::new(&store, None);

        let router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, blobs_protocol)
            .spawn();

        println!("💾 Persistenter DiskStore aktiv unter: {:?}", blobs_dir);

        Ok(Self {
            endpoint,
            store,
            _router: router,
            node_id,
        })
    }

    /// Gibt die gecachte Node-ID ohne Endpoint-Overhead zurück
    pub fn get_node_id(&self) -> NodeId {
        self.node_id
    }

    pub async fn publish_file(&self, path: &str) -> Result<String> {
        let file_path = Path::new(path);
        if !file_path.exists() {
            anyhow::bail!("Datei nicht gefunden: {}", path);
        }

        // Wichtig: iroh-blobs benötigt zwingend einen absoluten Pfad
        let abs_path = tokio::fs::canonicalize(file_path).await?;

        let added = self.store.add_path(&abs_path).await?;
        let hash = added.hash;

        let node_addr = self.node_id.into();
        let ticket = BlobTicket::new(node_addr, hash, iroh_blobs::BlobFormat::Raw);

        Ok(ticket.to_string())
    }

    pub async fn download_file(&self, ticket_str: &str, output_path: &str) -> Result<()> {
        let ticket = ticket_str.parse::<BlobTicket>()?;
        let hash = ticket.hash();
        
        let providers = vec![ticket.addr().id];

        println!("📥 Starte P2P-Download über Ticket...");
        
        let downloader = iroh_blobs::api::downloader::Downloader::new(&self.store, &self.endpoint);
        let _stats = downloader.download(hash, providers).await?;

        // Wichtig: iroh-blobs export benötigt zwingend einen absoluten Pfad
        let out_path = Path::new(output_path);
        let abs_output_path = if out_path.is_absolute() {
            out_path.to_path_buf()
        } else {
            std::env::current_dir()?.join(out_path)
        };

        self.store.export(hash, abs_output_path).await?;

        println!("✅ Datei erfolgreich heruntergeladen nach: {}", output_path);
        Ok(())
    }

    pub async fn delete_blob(&self, _hash: &Hash) -> Result<()> {
        // Inhaltsbasierter Store: Das Bereinigen des Ledger-Eintrags 
        // in SQLite steuert den Lebenszyklus der Daten vollständig.
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.endpoint.close().await;
    }
}
