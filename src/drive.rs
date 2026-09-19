//! Proton Drive key chain and read operations: my-files root, listing, download.
//!
//! Key chain: user keys (unlocked by the key secret from login) → address keys
//! → share key (share passphrase encrypted to an address key) → node keys
//! (each node's passphrase encrypted to its parent's key) → file content
//! session key (packet encrypted to the node key) → blocks.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD as B64;
use proton_crypto::crypto::{DataEncoding, Decryptor, DecryptorSync, PGPProviderSync, VerifiedData};
use proton_crypto_account::keys::AddressKeys;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::io::Write;

use crate::api::Api;

#[derive(Deserialize)]
struct LinkDetails {
    #[serde(rename = "Link")]
    link: Link,
    #[serde(rename = "File")]
    file: Option<File>,
}

#[derive(Deserialize)]
struct Link {
    #[serde(rename = "LinkID")]
    id: String,
    /// 1 = folder, 2 = file.
    #[serde(rename = "Type")]
    kind: i32,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "NodeKey")]
    node_key: String,
    #[serde(rename = "NodePassphrase")]
    node_passphrase: String,
}

#[derive(Deserialize)]
struct File {
    #[serde(rename = "ContentKeyPacket")]
    content_key_packet: String,
    #[serde(rename = "ActiveRevision")]
    active_revision: Option<ActiveRevision>,
}

#[derive(Deserialize)]
struct ActiveRevision {
    #[serde(rename = "RevisionID")]
    id: String,
}

/// A decrypted node with its unlocked key.
pub struct Node<K> {
    pub id: String,
    pub name: String,
    pub is_folder: bool,
    key: K,
    file: Option<File>,
}

pub struct Drive<P: PGPProviderSync> {
    pub api: Api,
    pgp: P,
    volume_id: String,
    root: Node<P::PrivateKey>,
}

impl<P: PGPProviderSync> Drive<P> {
    pub async fn open(mut api: Api, pgp: P) -> Result<Self> {
        let secret = api.key_secret()?;
        let user = api.user().await?;
        let user_keys = user.keys.unlock(&pgp, &secret).unlocked_keys;
        if user_keys.is_empty() {
            bail!("could not unlock user keys; run `kpdrive login` again");
        }

        #[derive(Deserialize)]
        struct Addresses {
            #[serde(rename = "Addresses")]
            addresses: Vec<Address>,
        }
        #[derive(Deserialize)]
        struct Address {
            #[serde(rename = "ID")]
            id: String,
            #[serde(rename = "Keys")]
            keys: AddressKeys,
        }
        let addresses: Addresses = api.get("core/v4/addresses").await?;
        let address_keys: HashMap<String, Vec<P::PrivateKey>> = addresses
            .addresses
            .into_iter()
            .map(|a| {
                let keys = a.keys.unlock(&pgp, &user_keys, None).unlocked_keys;
                (a.id, keys.into_iter().map(|k| k.private_key).collect())
            })
            .collect();

        #[derive(Deserialize)]
        struct MyFiles {
            #[serde(rename = "Volume")]
            volume: Volume,
            #[serde(rename = "Share")]
            share: Share,
            #[serde(rename = "Link")]
            link: LinkDetails,
        }
        #[derive(Deserialize)]
        struct Volume {
            #[serde(rename = "VolumeID")]
            id: String,
        }
        #[derive(Deserialize)]
        struct Share {
            #[serde(rename = "Key")]
            key: String,
            #[serde(rename = "Passphrase")]
            passphrase: String,
            #[serde(rename = "AddressID")]
            address_id: String,
        }
        let my_files: MyFiles = api.get("drive/v2/shares/my-files").await?;
        let keys = address_keys
            .get(&my_files.share.address_id)
            .filter(|k| !k.is_empty())
            .ok_or_else(|| anyhow!("no unlocked keys for the share's address"))?;
        let passphrase = pgp
            .new_decryptor()
            .with_decryption_keys(keys.iter())
            .decrypt(&my_files.share.passphrase, DataEncoding::Armor)
            .map_err(|e| anyhow!("decrypt share passphrase: {e}"))?;
        let share_key = pgp
            .private_key_import(&my_files.share.key, passphrase.as_bytes(), DataEncoding::Armor)
            .map_err(|e| anyhow!("unlock share key: {e}"))?;
        let root = Self::decrypt_link(&pgp, &share_key, my_files.link)?;
        Ok(Self { api, pgp, volume_id: my_files.volume.id, root })
    }

    fn decrypt_link(pgp: &P, parent: &P::PrivateKey, d: LinkDetails) -> Result<Node<P::PrivateKey>> {
        let passphrase = pgp
            .new_decryptor()
            .with_decryption_key(parent)
            .decrypt(&d.link.node_passphrase, DataEncoding::Armor)
            .map_err(|e| anyhow!("decrypt node passphrase: {e}"))?;
        let key = pgp
            .private_key_import(&d.link.node_key, passphrase.as_bytes(), DataEncoding::Armor)
            .map_err(|e| anyhow!("unlock node key: {e}"))?;
        let name = pgp
            .new_decryptor()
            .with_decryption_key(parent)
            .decrypt(&d.link.name, DataEncoding::Armor)
            .map_err(|e| anyhow!("decrypt node name: {e}"))?;
        Ok(Node {
            id: d.link.id,
            name: String::from_utf8(name.to_vec()).context("node name is not UTF-8")?,
            is_folder: d.link.kind == 1,
            key,
            file: d.file,
        })
    }

    pub async fn list(&mut self, folder: &Node<P::PrivateKey>) -> Result<Vec<Node<P::PrivateKey>>> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(rename = "LinkIDs")]
            link_ids: Vec<String>,
            #[serde(rename = "AnchorID")]
            anchor_id: Option<String>,
            #[serde(rename = "More", default)]
            more: bool,
        }
        let mut ids = Vec::new();
        let mut anchor: Option<String> = None;
        loop {
            let mut path = format!("drive/v2/volumes/{}/folders/{}/children", self.volume_id, folder.id);
            if let Some(a) = &anchor {
                path.push_str(&format!("?AnchorID={a}"));
            }
            let page: Page = self.api.get(&path).await?;
            ids.extend(page.link_ids);
            anchor = page.anchor_id;
            if !page.more || anchor.is_none() {
                break;
            }
        }

        #[derive(Deserialize)]
        struct Links {
            #[serde(rename = "Links")]
            links: Vec<LinkDetails>,
        }
        let mut nodes = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(150) {
            let path = format!("drive/v2/volumes/{}/links", self.volume_id);
            let links: Links = self.api.post(&path, &json!({ "LinkIDs": chunk })).await?;
            for d in links.links {
                nodes.push(Self::decrypt_link(&self.pgp, &folder.key, d)?);
            }
        }
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(nodes)
    }

    /// Walks `path` ("a/b/c", leading slash optional) from the root.
    pub async fn resolve(&mut self, path: &str) -> Result<Node<P::PrivateKey>> {
        let mut node = self.root_clone_key()?;
        for part in path.split('/').filter(|s| !s.is_empty()) {
            if !node.is_folder {
                bail!("{} is not a folder", node.name);
            }
            node = self
                .list(&node)
                .await?
                .into_iter()
                .find(|n| n.name == part)
                .ok_or_else(|| anyhow!("no such file or folder: {part}"))?;
        }
        Ok(node)
    }

    // Node keys aren't Clone in the provider API; re-derive the root cheaply
    // by re-reading it. ponytail: one extra request per resolve, cache if it matters.
    fn root_clone_key(&self) -> Result<Node<P::PrivateKey>> {
        Ok(Node {
            id: self.root.id.clone(),
            name: self.root.name.clone(),
            is_folder: true,
            key: self.reimport(&self.root.key)?,
            file: None,
        })
    }

    fn reimport(&self, key: &P::PrivateKey) -> Result<P::PrivateKey> {
        let exported = self
            .pgp
            .private_key_export_unlocked(key, DataEncoding::Bytes)
            .map_err(|e| anyhow!("export key: {e}"))?;
        self.pgp
            .private_key_import_unlocked(exported, DataEncoding::Bytes)
            .map_err(|e| anyhow!("import key: {e}"))
    }

    /// Streams the active revision of `file` into `out`, block by block.
    pub async fn download(&mut self, file: &Node<P::PrivateKey>, out: &mut impl Write) -> Result<u64> {
        let f = file.file.as_ref().ok_or_else(|| anyhow!("{} is not a file", file.name))?;
        let revision = f.active_revision.as_ref().ok_or_else(|| anyhow!("{} has no active revision", file.name))?;
        let packet = B64.decode(&f.content_key_packet).context("content key packet base64")?;
        let session_key = self
            .pgp
            .new_decryptor()
            .with_decryption_key(&file.key)
            .decrypt_session_key(&packet)
            .map_err(|e| anyhow!("decrypt content key: {e}"))?;

        #[derive(Deserialize)]
        struct R {
            #[serde(rename = "Revision")]
            revision: Rev,
        }
        #[derive(Deserialize)]
        struct Rev {
            #[serde(rename = "Blocks", default)]
            blocks: Vec<Block>,
        }
        #[derive(Deserialize)]
        struct Block {
            #[serde(rename = "Index")]
            index: i64,
            #[serde(rename = "BareURL")]
            bare_url: String,
            #[serde(rename = "Token")]
            token: String,
        }
        const PAGE: usize = 50;
        let mut written = 0u64;
        let mut next_index = 1i64;
        loop {
            let path = format!(
                "drive/v2/volumes/{}/files/{}/revisions/{}?FromBlockIndex={next_index}&PageSize={PAGE}&NoBlockUrls=0",
                self.volume_id, file.id, revision.id
            );
            let mut blocks = self.api.get::<R>(&path).await?.revision.blocks;
            blocks.sort_by_key(|b| b.index);
            // ponytail: blocks fetched sequentially; parallelise when download speed matters.
            for b in &blocks {
                if b.index != next_index {
                    bail!("block table gap: expected {next_index}, got {}", b.index);
                }
                let ciphertext = self.api.fetch_block(&b.bare_url, &b.token).await?;
                let plain = self
                    .pgp
                    .new_decryptor()
                    .with_session_key_ref(&session_key)
                    .decrypt(&ciphertext, DataEncoding::Bytes)
                    .map_err(|e| anyhow!("decrypt block {}: {e}", b.index))?;
                out.write_all(plain.as_bytes())?;
                written += plain.as_bytes().len() as u64;
                next_index += 1;
            }
            if blocks.len() < PAGE {
                break;
            }
        }
        Ok(written)
    }
}
