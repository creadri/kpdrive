//! Proton Drive key chain and read operations: my-files root, listing, download.
//!
//! Key chain: user keys (unlocked by the key secret from login) → address keys
//! → share key (share passphrase encrypted to an address key) → node keys
//! (each node's passphrase encrypted to its parent's key) → file content
//! session key (packet encrypted to the node key) → blocks.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD as B64;
use futures_util::{StreamExt, stream};
use hmac::{Hmac, Mac};
use proton_crypto::crypto::{
    DataEncoding, Decryptor, DecryptorSync, Encryptor, EncryptorSync, KeyGenerator, KeyGeneratorSync, PGPMessage,
    PGPProviderSync, SessionKey, SessionKeyAlgorithm, Signer, SignerSync, VerifiedData,
};
use proton_crypto::srp::{HashedPassword, SRPProvider};
use proton_crypto_account::keys::AddressKeys;
use serde::Deserialize;
use serde_json::json;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};

/// Plaintext bytes per content block, as used by Proton's clients.
const BLOCK_SIZE: usize = 4 * 1024 * 1024;

use crate::api::Api;

#[derive(Deserialize)]
struct LinkDetails {
    #[serde(rename = "Link")]
    link: Link,
    #[serde(rename = "File")]
    file: Option<File>,
    #[serde(rename = "Folder")]
    folder: Option<Folder>,
    /// Present once the node has a share of its own (what a public link hangs off).
    #[serde(rename = "Sharing", default)]
    sharing: Option<Sharing>,
}

/// `v2/shares/my-files` and `v2/shares/photos` answer with the same shape.
#[derive(Deserialize)]
struct ShareBootstrap {
    #[serde(rename = "Volume")]
    volume: BootstrapVolume,
    #[serde(rename = "Share")]
    share: BootstrapShare,
    #[serde(rename = "Link")]
    link: LinkDetails,
}

#[derive(Deserialize)]
struct BootstrapVolume {
    #[serde(rename = "VolumeID")]
    id: String,
}

#[derive(Deserialize)]
struct BootstrapShare {
    #[serde(rename = "Key")]
    key: String,
    #[serde(rename = "Passphrase")]
    passphrase: String,
    #[serde(rename = "AddressID")]
    address_id: String,
}

#[derive(Deserialize)]
struct Sharing {
    #[serde(rename = "ShareID")]
    share_id: String,
}

#[derive(Deserialize)]
struct Folder {
    /// Armored, encrypted to the folder's node key; HMAC key for child name hashes.
    #[serde(rename = "NodeHashKey")]
    hash_key: String,
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
    /// 1 = active, 2 = trashed.
    #[serde(rename = "State")]
    state: i32,
    #[serde(rename = "TrashTime", default)]
    trash_time: Option<i64>,
    #[serde(rename = "ModifyTime")]
    modify_time: i64,
    #[serde(rename = "ParentLinkID", default)]
    parent_id: Option<String>,
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
    /// Photos live in their own volume, so a node cannot assume the files one.
    pub volume_id: String,
    pub name: String,
    pub is_folder: bool,
    /// Server-side last modification, unix seconds.
    pub modify_time: i64,
    /// Active revision id for files; changes when content changes.
    pub revision: Option<String>,
    /// `None` for the root of a share.
    pub parent_id: Option<String>,
    key: K,
    file: Option<File>,
    hash_key: Option<String>,
    /// Armored, encrypted to the *parent* key. Creating a share re-wraps their
    /// session keys so the share can read this node's name and passphrase.
    passphrase: String,
    name_armored: String,
    /// Set once the node has its own share.
    share_id: Option<String>,
}

pub struct Drive<P: PGPProviderSync> {
    pub api: Api,
    pgp: P,
    volume_id: String,
    root: Node<P::PrivateKey>,
    /// The share's address: id, email, and its primary key, which signs what we write.
    address_id: String,
    email: String,
    /// Primary address key: signs everything we write.
    signing_key: P::PrivateKey,
    /// Every unlocked key of that address, for reading what other clients wrote.
    address_keys: Vec<P::PrivateKey>,
    /// Unlocked folder nodes by id, kept for the life of the client so that a
    /// changed link can be decrypted with its parent's key without walking
    /// the tree. Folders only: files are many and never anyone's parent.
    folders: std::sync::RwLock<HashMap<String, Node<P::PrivateKey>>>,
}

impl<P: PGPProviderSync> Drive<P> {
    pub async fn open(api: Api, pgp: P) -> Result<Self> {
        #[derive(Deserialize)]
        struct Addresses {
            #[serde(rename = "Addresses")]
            addresses: Vec<Address>,
        }
        #[derive(Deserialize)]
        struct Address {
            #[serde(rename = "ID")]
            id: String,
            #[serde(rename = "Email")]
            email: String,
            #[serde(rename = "Keys")]
            keys: AddressKeys,
        }
        let secret = api.key_secret()?;
        // The three bootstrap calls do not depend on each other; only the key
        // unlocking below does, so it is the calls that run side by side.
        let (user, addresses, my_files) = tokio::try_join!(
            api.user(),
            api.get::<Addresses>("core/v4/addresses"),
            api.get::<ShareBootstrap>("drive/v2/shares/my-files"),
        )?;
        let user_keys = user.keys.unlock(&pgp, &secret).unlocked_keys;
        if user_keys.is_empty() {
            bail!("could not unlock user keys; run `kpdrive login` again");
        }

        // address id → (email, primary key, all keys)
        let mut address_keys: HashMap<String, (String, Option<P::PrivateKey>, Vec<P::PrivateKey>)> = HashMap::new();
        for a in addresses.addresses {
            let unlocked = a.keys.unlock(&pgp, &user_keys, None).unlocked_keys;
            let primary_idx = unlocked.iter().position(|k| k.primary).or(if unlocked.is_empty() { None } else { Some(0) });
            let mut keys: Vec<P::PrivateKey> = Vec::new();
            let mut primary = None;
            for (i, k) in unlocked.into_iter().enumerate() {
                if Some(i) == primary_idx {
                    primary = Some(Self::reimport_key(&pgp, &k.private_key)?);
                }
                keys.push(k.private_key);
            }
            address_keys.insert(a.id, (a.email, primary, keys));
        }

        let ShareBootstrap { volume, share, link } = my_files;
        let (email, signing_key) = address_keys
            .get(&share.address_id)
            .and_then(|(email, primary, _)| primary.as_ref().map(|k| (email.clone(), k)))
            .map(|(email, key)| Ok::<_, anyhow::Error>((email, Self::reimport_key(&pgp, key)?)))
            .transpose()?
            .ok_or_else(|| anyhow!("no primary key for the Drive share's address"))?;
        // Every address's keys, because the photos share may sit on another one.
        let address_keys: Vec<P::PrivateKey> =
            address_keys.into_values().flat_map(|(_, _, keys)| keys).collect();
        let root = decrypt_share_root(&pgp, &address_keys, &share, &volume.id, link)?;
        Ok(Self {
            api,
            pgp,
            volume_id: volume.id,
            root,
            address_id: share.address_id,
            email,
            signing_key,
            address_keys,
            folders: std::sync::RwLock::new(HashMap::new()),
        })
    }

    /// A copy of a node. Keys are not `Clone` in the provider API, so the
    /// unlocked key is round-tripped, which costs microseconds.
    pub fn dup(&self, node: &Node<P::PrivateKey>) -> Result<Node<P::PrivateKey>> {
        Ok(Node {
            id: node.id.clone(),
            volume_id: node.volume_id.clone(),
            name: node.name.clone(),
            is_folder: node.is_folder,
            modify_time: node.modify_time,
            revision: node.revision.clone(),
            parent_id: node.parent_id.clone(),
            key: self.reimport(&node.key)?,
            file: node.file.as_ref().map(|f| File {
                content_key_packet: f.content_key_packet.clone(),
                active_revision: f.active_revision.as_ref().map(|r| ActiveRevision { id: r.id.clone() }),
            }),
            hash_key: node.hash_key.clone(),
            passphrase: node.passphrase.clone(),
            name_armored: node.name_armored.clone(),
            share_id: node.share_id.clone(),
        })
    }

    fn remember(&self, node: &Node<P::PrivateKey>) {
        if node.is_folder {
            if let Ok(copy) = self.dup(node) {
                self.folders.write().expect("folder cache").insert(node.id.clone(), copy);
            }
        }
    }

    /// A folder seen earlier by this client, if any.
    pub fn cached_folder(&self, id: &str) -> Option<Node<P::PrivateKey>> {
        let cache = self.folders.read().expect("folder cache");
        cache.get(id).and_then(|n| self.dup(n).ok())
    }

    fn decrypt_link(pgp: &P, volume_id: &str, parent: &P::PrivateKey, d: LinkDetails) -> Result<Node<P::PrivateKey>> {
        let (armored_passphrase, name_armored) = (d.link.node_passphrase.clone(), d.link.name.clone());
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
            volume_id: volume_id.to_owned(),
            name: String::from_utf8(name.to_vec()).context("node name is not UTF-8")?,
            is_folder: d.link.kind == 1,
            modify_time: d.link.modify_time,
            revision: d.file.as_ref().and_then(|f| f.active_revision.as_ref()).map(|r| r.id.clone()),
            parent_id: d.link.parent_id.clone(),
            key,
            file: d.file,
            hash_key: d.folder.map(|f| f.hash_key),
            passphrase: armored_passphrase,
            name_armored,
            share_id: d.sharing.map(|s| s.share_id),
        })
    }

    pub async fn list(&self, folder: &Node<P::PrivateKey>) -> Result<Vec<Node<P::PrivateKey>>> {
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
            let mut path = format!("drive/v2/volumes/{}/folders/{}/children", folder.volume_id, folder.id);
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
        // The detail chunks do not depend on each other; a big folder is
        // several of them, so fetch them side by side.
        let path = format!("drive/v2/volumes/{}/links", folder.volume_id);
        let api = &self.api;
        let pages: Vec<Result<Links>> = stream::iter(ids.chunks(150))
            .map(|chunk| {
                let path = path.clone();
                async move { api.post(&path, &json!({ "LinkIDs": chunk })).await }
            })
            .buffered(4)
            .collect()
            .await;
        let mut nodes = Vec::with_capacity(ids.len());
        for links in pages {
            for d in links?.links {
                if d.link.state != 1 || d.link.trash_time.is_some() {
                    continue; // trashed, draft or deleted
                }
                let node = Self::decrypt_link(&self.pgp, &folder.volume_id, &folder.key, d)?;
                self.remember(&node);
                nodes.push(node);
            }
        }
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(nodes)
    }

    /// What the event stream says happened since `cursor`. A `None` cursor
    /// (first sync) reports `refresh`, as does the API when the cursor is too
    /// old: both mean "walk the tree". Several events for one link collapse to
    /// its latest state, in the order links were first mentioned, so a folder
    /// still comes before what was created inside it.
    pub async fn events(&self, cursor: Option<&str>) -> Result<Events> {
        #[derive(Deserialize)]
        struct Latest {
            #[serde(rename = "EventID")]
            event_id: String,
        }
        #[derive(Deserialize)]
        struct Page {
            #[serde(rename = "EventID")]
            event_id: String,
            #[serde(rename = "Events", default)]
            events: Vec<Event>,
            #[serde(rename = "More", default)]
            more: bool,
            #[serde(rename = "Refresh", default)]
            refresh: bool,
        }
        #[derive(Deserialize)]
        struct Event {
            /// 0 deleted; 1 created, 2 updated, 3 moved.
            #[serde(rename = "EventType")]
            kind: i32,
            #[serde(rename = "Link")]
            link: EventLink,
        }
        #[derive(Deserialize)]
        struct EventLink {
            #[serde(rename = "LinkID")]
            id: String,
            #[serde(rename = "ParentLinkID", default)]
            parent_id: Option<String>,
            #[serde(rename = "IsTrashed", default)]
            trashed: bool,
        }
        let Some(mut cursor) = cursor.map(str::to_owned) else {
            let path = format!("drive/volumes/{}/events/latest", self.volume_id);
            let cursor = self.api.get::<Latest>(&path).await?.event_id;
            return Ok(Events { cursor, refresh: true, changes: Vec::new() });
        };
        let mut order: Vec<String> = Vec::new();
        let mut latest: HashMap<String, Change> = HashMap::new();
        let mut refresh = false;
        loop {
            let path = format!("drive/v2/volumes/{}/events/{cursor}", self.volume_id);
            let page: Page = self.api.get(&path).await?;
            if std::env::var_os("KPDRIVE_DEBUG").is_some() {
                eprintln!("events page: {} events, more={}, refresh={}, next={}", page.events.len(), page.more, page.refresh, page.event_id);
            }
            for e in page.events {
                let change = Change { link_id: e.link.id.clone(), parent_id: e.link.parent_id, gone: e.kind == 0 || e.link.trashed };
                if latest.insert(e.link.id.clone(), change).is_none() {
                    order.push(e.link.id);
                }
            }
            refresh |= page.refresh;
            cursor = page.event_id;
            if page.refresh || !page.more {
                break;
            }
        }
        let changes = order.into_iter().filter_map(|id| latest.remove(&id)).collect();
        Ok(Events { cursor, refresh, changes })
    }

    /// Fetches and decrypts nodes by id, in the files volume. Trashed or
    /// deleted links come back as `None`. Parents are found in the folder
    /// cache, or fetched and cached on the way up.
    pub async fn nodes_by_ids(&self, ids: &[String]) -> Result<Vec<(String, Option<Node<P::PrivateKey>>)>> {
        #[derive(Deserialize)]
        struct Links {
            #[serde(rename = "Links")]
            links: Vec<LinkDetails>,
        }
        let mut out = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(150) {
            let path = format!("drive/v2/volumes/{}/links", self.volume_id);
            let links: Links = self.api.post(&path, &json!({ "LinkIDs": chunk })).await?;
            let mut found: HashMap<String, LinkDetails> = links.links.into_iter().map(|d| (d.link.id.clone(), d)).collect();
            for id in chunk {
                let Some(d) = found.remove(id) else {
                    out.push((id.clone(), None));
                    continue;
                };
                if d.link.state != 1 || d.link.trash_time.is_some() {
                    out.push((id.clone(), None));
                    continue;
                }
                let node = match &d.link.parent_id {
                    None => Self::decrypt_link(&self.pgp, &self.volume_id, &self.root.key, d)?, // a share root
                    Some(p) => {
                        let Some(parent) = self.folder(p).await? else {
                            out.push((id.clone(), None));
                            continue;
                        };
                        Self::decrypt_link(&self.pgp, &self.volume_id, &parent.key, d)?
                    }
                };
                self.remember(&node);
                out.push((id.clone(), Some(node)));
            }
        }
        Ok(out)
    }

    /// A folder by id: the root, the cache, or a fetch that recurses upward
    /// through parents that are not cached yet.
    async fn folder(&self, id: &str) -> Result<Option<Node<P::PrivateKey>>> {
        if id == self.root.id {
            return self.root().map(Some);
        }
        if let Some(cached) = self.cached_folder(id) {
            return Ok(Some(cached));
        }
        // Recursion in async needs a box; depth is the tree's depth.
        let fetched = Box::pin(self.nodes_by_ids(std::slice::from_ref(&id.to_owned()))).await?;
        Ok(fetched.into_iter().next().and_then(|(_, n)| n).filter(|n| n.is_folder))
    }

    /// Resolves `path` to its parent folder and the node itself. The root has
    /// no parent, so it is rejected.
    pub async fn resolve_with_parent(&self, path: &str) -> Result<(Node<P::PrivateKey>, Node<P::PrivateKey>)> {
        let trimmed = path.trim_matches('/');
        let (parent_path, name) = trimmed.rsplit_once('/').unwrap_or(("", trimmed));
        if name.is_empty() {
            bail!("the Drive root itself cannot be shared; name a file or folder inside it");
        }
        let parent = self.resolve(parent_path).await?;
        let node = self
            .list(&parent)
            .await?
            .into_iter()
            .find(|n| n.name == name)
            .ok_or_else(|| anyhow!("no such file or folder: {name}"))?;
        Ok((parent, node))
    }

    /// Walks `path` ("a/b/c", leading slash optional) from the root.
    pub async fn resolve(&self, path: &str) -> Result<Node<P::PrivateKey>> {
        let mut node = self.root()?;
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

    /// The my-files root. Node keys aren't Clone in the provider API, so the
    /// unlocked key is round-tripped through export/import.
    pub fn root(&self) -> Result<Node<P::PrivateKey>> {
        Ok(Node {
            id: self.root.id.clone(),
            volume_id: self.root.volume_id.clone(),
            name: self.root.name.clone(),
            is_folder: true,
            modify_time: self.root.modify_time,
            revision: None,
            parent_id: None,
            key: self.reimport(&self.root.key)?,
            file: None,
            hash_key: self.root.hash_key.clone(),
            passphrase: self.root.passphrase.clone(),
            name_armored: self.root.name_armored.clone(),
            share_id: self.root.share_id.clone(),
        })
    }

    fn reimport(&self, key: &P::PrivateKey) -> Result<P::PrivateKey> {
        Self::reimport_key(&self.pgp, key)
    }

    fn reimport_key(pgp: &P, key: &P::PrivateKey) -> Result<P::PrivateKey> {
        let exported = pgp
            .private_key_export_unlocked(key, DataEncoding::Bytes)
            .map_err(|e| anyhow!("export key: {e}"))?;
        pgp.private_key_import_unlocked(exported, DataEncoding::Bytes)
            .map_err(|e| anyhow!("import key: {e}"))
    }

    // ---- write side -------------------------------------------------------

    /// Armored PGP message to `key`'s public half, optionally inline-signed.
    fn encrypt_to(&self, key: &P::PrivateKey, data: &[u8], signer: Option<&P::PrivateKey>, text: bool) -> Result<String> {
        let public = self.pgp.private_key_to_public_key(key).map_err(|e| anyhow!("public key: {e}"))?;
        let mut enc = self.pgp.new_encryptor().with_encryption_key(&public);
        if let Some(s) = signer {
            enc = enc.with_signing_key(s);
        }
        if text {
            enc = enc.with_utf8();
        }
        let out = enc.encrypt_raw(data, DataEncoding::Armor).map_err(|e| anyhow!("encrypt: {e}"))?;
        Ok(String::from_utf8(out)?)
    }

    fn sign_detached(&self, key: &P::PrivateKey, data: &[u8]) -> Result<String> {
        let sig = self
            .pgp
            .new_signer()
            .with_signing_key(key)
            .sign_detached(data, DataEncoding::Armor)
            .map_err(|e| anyhow!("sign: {e}"))?;
        Ok(String::from_utf8(sig)?)
    }

    /// Fresh node key: (unlocked key, locked armored key, passphrase).
    fn new_node_key(&self) -> Result<(P::PrivateKey, String, Vec<u8>)> {
        let passphrase = B64.encode(proton_crypto::generate_secure_random_bytes::<32>()).into_bytes();
        let key = self
            .pgp
            .new_key_generator()
            .with_user_id("Drive key", "no-reply@proton.me")
            .generate()
            .map_err(|e| anyhow!("generate node key: {e}"))?;
        let locked = String::from_utf8(
            self.pgp
                .private_key_export(&key, &passphrase, DataEncoding::Armor)
                .map_err(|e| anyhow!("lock node key: {e}"))?
                .as_ref()
                .to_vec(),
        )?;
        Ok((key, locked, passphrase))
    }

    /// Decrypts a folder's HMAC key for child name hashes.
    fn hash_key(&self, folder: &Node<P::PrivateKey>) -> Result<Vec<u8>> {
        let armored = folder.hash_key.as_ref().ok_or_else(|| anyhow!("{} has no hash key", folder.name))?;
        Ok(self
            .pgp
            .new_decryptor()
            .with_decryption_key(&folder.key)
            .decrypt(armored, DataEncoding::Armor)
            .map_err(|e| anyhow!("decrypt hash key: {e}"))?
            .to_vec())
    }

    /// The fields every new node shares: encrypted name, name hash, locked key,
    /// encrypted+signed passphrase.
    fn node_material(&self, parent: &Node<P::PrivateKey>, name: &str) -> Result<(P::PrivateKey, serde_json::Map<String, serde_json::Value>)> {
        let hash_key = self.hash_key(parent)?;
        let (key, locked, passphrase) = self.new_node_key()?;
        let mut m = serde_json::Map::new();
        m.insert("Name".into(), self.encrypt_to(&parent.key, name.as_bytes(), Some(&self.signing_key), true)?.into());
        m.insert("Hash".into(), name_hash(&hash_key, name).into());
        m.insert("ParentLinkID".into(), parent.id.clone().into());
        m.insert("NodeKey".into(), locked.into());
        m.insert("NodePassphrase".into(), self.encrypt_to(&parent.key, &passphrase, None, false)?.into());
        m.insert("NodePassphraseSignature".into(), self.sign_detached(&self.signing_key, &passphrase)?.into());
        Ok((key, m))
    }

    pub async fn create_folder(&self, parent: &Node<P::PrivateKey>, name: &str) -> Result<Node<P::PrivateKey>> {
        let (key, mut body) = self.node_material(parent, name)?;
        let hash_key_armored = self.encrypt_to(&key, &proton_crypto::generate_secure_random_bytes::<32>(), Some(&key), false)?;
        body.insert("NodeHashKey".into(), hash_key_armored.clone().into());
        body.insert("SignatureEmail".into(), self.email.clone().into());
        #[derive(Deserialize)]
        struct R {
            #[serde(rename = "Folder")]
            folder: Id,
        }
        #[derive(Deserialize)]
        struct Id {
            #[serde(rename = "ID")]
            id: String,
        }
        let path = format!("drive/v2/volumes/{}/folders", parent.volume_id);
        let r: R = self.api.post(&path, &serde_json::Value::Object(body)).await?;
        Ok(Node {
            id: r.folder.id,
            volume_id: parent.volume_id.clone(),
            name: name.to_owned(),
            is_folder: true,
            modify_time: now(),
            revision: None,
            parent_id: Some(parent.id.clone()),
            key,
            file: None,
            hash_key: Some(hash_key_armored),
            passphrase: String::new(),
            name_armored: String::new(),
            share_id: None,
        })
    }

    /// Moves nodes to the trash (the user can restore them in the web app).
    pub async fn trash(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let path = format!("drive/v2/volumes/{}/trash_multiple", self.volume_id);
        let _: serde_json::Value = self.api.post(&path, &json!({ "LinkIDs": ids })).await?;
        Ok(())
    }

    /// Uploads `data` as a new file under `parent`, or as a new revision of
    /// `existing`. Returns (link id, revision id).
    pub async fn upload(
        &self,
        parent: &Node<P::PrivateKey>,
        name: &str,
        existing: Option<&Node<P::PrivateKey>>,
        data: &mut impl Read,
        mtime: i64,
    ) -> Result<(String, String)> {
        // 1. Draft: a new file node, or a new revision on an existing one.
        let (link_id, revision_id, node_key, session_key) = match existing {
            Some(node) => {
                let f = node.file.as_ref().ok_or_else(|| anyhow!("{name} is not a file"))?;
                let current = node.revision.clone().ok_or_else(|| anyhow!("{name} has no active revision"))?;
                let packet = B64.decode(&f.content_key_packet).context("content key packet base64")?;
                let session_key = self
                    .pgp
                    .new_decryptor()
                    .with_decryption_key(&node.key)
                    .decrypt_session_key(&packet)
                    .map_err(|e| anyhow!("decrypt content key: {e}"))?;
                #[derive(Deserialize)]
                struct R {
                    #[serde(rename = "Revision")]
                    revision: Id,
                }
                #[derive(Deserialize)]
                struct Id {
                    #[serde(rename = "ID")]
                    id: String,
                }
                let path = format!("drive/v2/volumes/{}/files/{}/revisions", node.volume_id, node.id);
                let uid = self.api.uid();
                let r: R = self.api.post(&path, &json!({ "CurrentRevisionID": current, "ClientUID": uid })).await?;
                (node.id.clone(), r.revision.id, self.reimport(&node.key)?, session_key)
            }
            None => {
                let (key, mut body) = self.node_material(parent, name)?;
                let session_key = self
                    .pgp
                    .session_key_generate(SessionKeyAlgorithm::Aes256)
                    .map_err(|e| anyhow!("generate content key: {e}"))?;
                let public = self.pgp.private_key_to_public_key(&key).map_err(|e| anyhow!("public key: {e}"))?;
                let packet = self
                    .pgp
                    .new_encryptor()
                    .with_encryption_key(&public)
                    .encrypt_session_key(&session_key)
                    .map_err(|e| anyhow!("content key packet: {e}"))?;
                body.insert("MIMEType".into(), "application/octet-stream".into());
                body.insert("ContentKeyPacket".into(), B64.encode(&packet).into());
                body.insert("ContentKeyPacketSignature".into(), self.sign_detached(&key, session_key.export().as_ref())?.into());
                body.insert("SignatureAddress".into(), self.email.clone().into());
                body.insert("ClientUID".into(), self.api.uid().into());
                #[derive(Deserialize)]
                struct R {
                    #[serde(rename = "File")]
                    file: Ids,
                }
                #[derive(Deserialize)]
                struct Ids {
                    #[serde(rename = "ID")]
                    id: String,
                    #[serde(rename = "RevisionID")]
                    revision_id: String,
                }
                let path = format!("drive/v2/volumes/{}/files", parent.volume_id);
                let r: R = self.api.post(&path, &serde_json::Value::Object(body)).await?;
                (r.file.id, r.file.revision_id, key, session_key)
            }
        };

        // 2. Verification code: XORed into each block's ciphertext prefix so the
        //    server can check we hold the content key.
        #[derive(Deserialize)]
        struct Verification {
            #[serde(rename = "VerificationCode")]
            code: String,
        }
        let path = format!("drive/v2/volumes/{}/links/{link_id}/revisions/{revision_id}/verification", parent.volume_id);
        let code = B64.decode(self.api.get::<Verification>(&path).await?.code).context("verification code base64")?;

        // 3. Blocks, in batches: encrypt a batch sequentially (the manifest is
        //    extended in that order, so its order is safe by construction), ask
        //    for all its upload targets in one call, then PUT them concurrently.
        /// Blocks per prepare call and upload burst; bounds memory at about
        /// this many 4 MiB ciphertexts plus one plaintext buffer.
        const BATCH: usize = 16;
        const IN_FLIGHT: usize = 6;

        #[derive(Deserialize)]
        struct Prep {
            #[serde(rename = "UploadLinks")]
            links: Vec<Target>,
        }
        #[derive(Deserialize)]
        struct Target {
            #[serde(rename = "BareURL")]
            bare_url: String,
            #[serde(rename = "Token")]
            token: String,
            /// Present in current responses; older ones matched by position.
            #[serde(rename = "Index", default)]
            index: Option<i64>,
        }
        struct Prepared {
            index: i64,
            ciphertext: Vec<u8>,
            entry: serde_json::Value,
        }

        let mut manifest = Vec::new();
        let mut block_sizes = Vec::new();
        let mut sha1 = Sha1::new();
        let mut total = 0u64;
        let mut buf = vec![0u8; BLOCK_SIZE];
        let mut index = 1i64;
        loop {
            let mut batch: Vec<Prepared> = Vec::with_capacity(BATCH);
            while batch.len() < BATCH {
                let n = read_full(data, &mut buf)?;
                if n == 0 {
                    break;
                }
                let plain = &buf[..n];
                sha1.update(plain);
                total += n as u64;
                block_sizes.push(n);

                let ciphertext = self
                    .pgp
                    .new_encryptor()
                    .with_session_key_ref(&session_key)
                    .encrypt_raw(plain, DataEncoding::Bytes)
                    .map_err(|e| anyhow!("encrypt block {index}: {e}"))?;
                let digest = Sha256::digest(&ciphertext);
                manifest.extend_from_slice(&digest);
                let token: Vec<u8> = code.iter().enumerate().map(|(i, c)| c ^ ciphertext.get(i).copied().unwrap_or(0)).collect();
                let plain_sig = self.sign_detached(&self.signing_key, plain)?;
                let enc_sig = self.encrypt_to(&node_key, plain_sig.as_bytes(), None, false)?;
                batch.push(Prepared {
                    index,
                    entry: json!({
                        "Index": index,
                        "Size": ciphertext.len(),
                        "EncSignature": enc_sig,
                        "Hash": B64.encode(digest),
                        "Verifier": { "Token": B64.encode(&token) },
                    }),
                    ciphertext,
                });
                index += 1;
            }
            if batch.is_empty() {
                break;
            }
            let last_batch = batch.len() < BATCH;

            let entries: Vec<&serde_json::Value> = batch.iter().map(|p| &p.entry).collect();
            let prep: Prep = self
                .api
                .post(
                    "drive/blocks",
                    &json!({
                        "AddressID": self.address_id,
                        "VolumeID": parent.volume_id,
                        "LinkID": link_id,
                        "RevisionID": revision_id,
                        "BlockList": entries,
                        "ThumbnailList": [],
                    }),
                )
                .await?;

            // Pair each block with its target: by Index when the API says which,
            // else by position, as the reference client does.
            let mut targets = prep.links;
            let mut jobs = Vec::with_capacity(batch.len());
            for p in batch {
                let at = targets
                    .iter()
                    .position(|t| t.index == Some(p.index))
                    .or_else(|| targets.iter().position(|t| t.index.is_none()))
                    .ok_or_else(|| anyhow!("no upload link for block {}", p.index))?;
                jobs.push((targets.remove(at), p));
            }

            let api = &self.api;
            let outcomes: Vec<Result<()>> = stream::iter(jobs)
                .map(|(t, p)| async move {
                    api.post_block(&t.bare_url, &t.token, p.ciphertext)
                        .await
                        .with_context(|| format!("upload block {}", p.index))
                })
                .buffer_unordered(IN_FLIGHT)
                .collect()
                .await;
            outcomes.into_iter().collect::<Result<Vec<()>>>()?;

            if last_batch {
                break;
            }
        }

        // 4. Seal the revision: signed manifest plus encrypted extended attributes.
        let xattr = json!({ "Common": {
            "Size": total,
            "ModificationTime": iso8601(mtime),
            "BlockSizes": block_sizes,
            "Digests": { "SHA1": hex::encode(sha1.finalize()) },
        }});
        let path = format!("drive/v2/volumes/{}/files/{link_id}/revisions/{revision_id}", self.volume_id);
        let _: serde_json::Value = self
            .api
            .put(
                &path,
                &json!({
                    "ManifestSignature": self.sign_detached(&self.signing_key, &manifest)?,
                    "SignatureAddress": self.email,
                    "ChecksumVerified": false,
                    "XAttr": self.encrypt_to(&node_key, xattr.to_string().as_bytes(), Some(&self.signing_key), false)?,
                }),
            )
            .await?;
        Ok((link_id, revision_id))
    }

    /// Streams the active revision of `file` into `out`, block by block.
    pub async fn download(&self, file: &Node<P::PrivateKey>, out: &mut impl Write) -> Result<u64> {
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
        /// Blocks in flight at once; bounds memory at this many 4 MiB ciphertexts.
        const IN_FLIGHT: usize = 6;
        let mut written = 0u64;
        let mut next_index = 1i64;
        let mut urls_renewed = false;
        loop {
            let path = format!(
                "drive/v2/volumes/{}/files/{}/revisions/{}?FromBlockIndex={next_index}&PageSize={PAGE}&NoBlockUrls=0",
                file.volume_id, file.id, revision.id
            );
            let mut blocks = self.api.get::<R>(&path).await?.revision.blocks;
            blocks.sort_by_key(|b| b.index);
            let page_len = blocks.len();
            if let Some((b, want)) = blocks.iter().zip(next_index..).find(|(b, want)| b.index != *want) {
                bail!("block table gap: expected {want}, got {}", b.index);
            }

            // Fetch ahead concurrently. `buffered` (not `buffer_unordered`) hands
            // results back in index order, so the file is still written
            // sequentially and no more than IN_FLIGHT blocks sit in memory.
            let api = &self.api;
            let mut fetched = stream::iter(blocks)
                .map(|b| async move { (b.index, api.fetch_block(&b.bare_url, &b.token).await) })
                .buffered(IN_FLIGHT);

            let mut renew = false;
            while let Some((index, result)) = fetched.next().await {
                let ciphertext = match result {
                    Ok(c) => c,
                    // Storage URLs expire. Refetching the block list renews every
                    // URL at once; blocks already written are skipped by next_index.
                    Err(e) if !urls_renewed && matches!(crate::api::api_status(&e), Some(401 | 403 | 404)) => {
                        urls_renewed = true;
                        renew = true;
                        break;
                    }
                    Err(e) => return Err(e.context(format!("block {index}"))),
                };
                let plain = self
                    .pgp
                    .new_decryptor()
                    .with_session_key_ref(&session_key)
                    .decrypt(&ciphertext, DataEncoding::Bytes)
                    .map_err(|e| anyhow!("decrypt block {index}: {e}"))?;
                out.write_all(plain.as_bytes())?;
                written += plain.as_bytes().len() as u64;
                next_index += 1;
            }
            drop(fetched);
            if renew {
                continue;
            }
            if page_len < PAGE {
                break;
            }
        }
        Ok(written)
    }
    // ---- photos -----------------------------------------------------------

    /// The photos timeline root, or `None` when the account has no photos
    /// volume. Photos live in their own volume with their own share.
    pub async fn photos_root(&self) -> Result<Option<Node<P::PrivateKey>>> {
        let bootstrap: ShareBootstrap = match self.api.get("drive/v2/shares/photos").await {
            Ok(b) => b,
            Err(e) if crate::api::api_code(&e) == Some(crate::api::DOES_NOT_EXIST) => return Ok(None),
            Err(e) => return Err(e),
        };
        let ShareBootstrap { volume, share, link } = bootstrap;
        Ok(Some(decrypt_share_root(&self.pgp, &self.address_keys, &share, &volume.id, link)?))
    }

    /// Every photo on the timeline, newest first, with its capture time. The
    /// timeline is flat: it is not the folder tree.
    pub async fn timeline(&self, root: &Node<P::PrivateKey>) -> Result<Vec<TimelinePhoto>> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(rename = "Photos", default)]
            photos: Vec<TimelinePhoto>,
        }
        const PAGE: usize = 500;
        let mut all: Vec<TimelinePhoto> = Vec::new();
        let mut anchor: Option<String> = None;
        loop {
            let mut path = format!("drive/volumes/{}/photos", root.volume_id);
            if let Some(a) = &anchor {
                path.push_str(&format!("?PreviousPageLastLinkID={a}"));
            }
            let page: Page = self.api.get(&path).await?;
            let count = page.photos.len();
            let last = page.photos.last().map(|p| p.id.clone());
            all.extend(page.photos);
            // Stop on a short page, and also if the cursor fails to advance —
            // a repeated page would otherwise loop forever.
            if count < PAGE || last.is_none() || last == anchor {
                return Ok(all);
            }
            anchor = last;
        }
    }

    /// Decrypts photo nodes by id. Photos have their own link-details route.
    pub async fn photo_nodes(&self, root: &Node<P::PrivateKey>, ids: &[String]) -> Result<Vec<Node<P::PrivateKey>>> {
        #[derive(Deserialize)]
        struct Links {
            #[serde(rename = "Links")]
            links: Vec<LinkDetails>,
        }
        let mut nodes = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(150) {
            let path = format!("drive/photos/volumes/{}/links", root.volume_id);
            let links: Links = self.api.post(&path, &json!({ "LinkIDs": chunk })).await?;
            for d in links.links {
                if d.link.state != 1 || d.link.trash_time.is_some() {
                    continue;
                }
                nodes.push(Self::decrypt_link(&self.pgp, &root.volume_id, &root.key, d)?);
            }
        }
        Ok(nodes)
    }

    // ---- public links -----------------------------------------------------

    /// Recovers the session key a PGP message was encrypted with.
    fn message_session_key(&self, key: &P::PrivateKey, armored: &str) -> Result<P::SessionKey> {
        let message = self
            .pgp
            .pgp_message_import(armored, DataEncoding::Armor)
            .map_err(|e| anyhow!("parse message: {e}"))?;
        self.pgp
            .new_decryptor()
            .with_decryption_key(key)
            .decrypt_session_key(message.as_key_packets())
            .map_err(|e| anyhow!("recover session key: {e}"))
    }

    /// Re-wraps a session key for `key`, base64 as the API wants it.
    fn wrap_session_key(&self, key: &P::PrivateKey, session_key: &P::SessionKey) -> Result<String> {
        let public = self.pgp.private_key_to_public_key(key).map_err(|e| anyhow!("public key: {e}"))?;
        let packet = self
            .pgp
            .new_encryptor()
            .with_encryption_key(&public)
            .encrypt_session_key(session_key)
            .map_err(|e| anyhow!("wrap session key: {e}"))?;
        Ok(B64.encode(packet))
    }

    /// The share hanging off `node`, created if it has none. A public link is
    /// always attached to such a share, never to the node directly.
    async fn ensure_share(
        &self,
        parent: &Node<P::PrivateKey>,
        node: &Node<P::PrivateKey>,
    ) -> Result<(String, P::SessionKey)> {
        if let Some(share_id) = &node.share_id {
            // Unlike the my-files lookup, this route puts the share's own fields
            // at the top level of the envelope.
            #[derive(Deserialize)]
            struct Share {
                #[serde(rename = "Passphrase")]
                passphrase: String,
            }
            let path = format!("drive/shares/{share_id}");
            let share: Share = self.api.get(&path).await?;
            let session_key = self.message_session_key(&node.key, &share.passphrase)?;
            return Ok((share_id.clone(), session_key));
        }

        // A new share gets its own key. Its passphrase is encrypted to both the
        // node key and our address key, under a session key we keep: the public
        // link is exactly that session key re-wrapped under the link password.
        let (share_key, share_key_armored, share_passphrase) = self.new_node_key()?;
        let session_key = self
            .pgp
            .session_key_generate(SessionKeyAlgorithm::Aes256)
            .map_err(|e| anyhow!("generate share session key: {e}"))?;
        let node_public = self.pgp.private_key_to_public_key(&node.key).map_err(|e| anyhow!("public key: {e}"))?;
        let address_public = self
            .pgp
            .private_key_to_public_key(&self.signing_key)
            .map_err(|e| anyhow!("public key: {e}"))?;
        let armored_passphrase = String::from_utf8(
            self.pgp
                .new_encryptor()
                .with_encryption_keys([&node_public, &address_public])
                .with_session_key(session_key.clone())
                .encrypt_raw(&share_passphrase, DataEncoding::Armor)
                .map_err(|e| anyhow!("encrypt share passphrase: {e}"))?,
        )?;

        // The share must be able to read the node's name and passphrase, so
        // their session keys are re-wrapped for the share key.
        let node_passphrase_sk = self.message_session_key(&parent.key, &node.passphrase)?;
        let node_name_sk = self.message_session_key(&parent.key, &node.name_armored)?;

        #[derive(Deserialize)]
        struct R {
            #[serde(rename = "Share")]
            share: Id,
        }
        #[derive(Deserialize)]
        struct Id {
            #[serde(rename = "ID")]
            id: String,
        }
        let path = format!("drive/volumes/{}/shares", node.volume_id);
        let created: R = self
            .api
            .post(
                &path,
                &json!({
                    "RootLinkID": node.id,
                    "AddressID": self.address_id,
                    "Name": "New Share",
                    "ShareKey": share_key_armored,
                    "SharePassphrase": armored_passphrase,
                    "SharePassphraseSignature": self.sign_detached(&self.signing_key, &share_passphrase)?,
                    "PassphraseKeyPacket": self.wrap_session_key(&share_key, &node_passphrase_sk)?,
                    "NameKeyPacket": self.wrap_session_key(&share_key, &node_name_sk)?,
                }),
            )
            .await?;
        Ok((created.share.id, session_key))
    }

    /// The public link already on `node`, if any, password fragment included.
    pub async fn public_link(&self, node: &Node<P::PrivateKey>) -> Result<Option<String>> {
        let Some(share_id) = &node.share_id else { return Ok(None) };
        let path = format!("drive/shares/{share_id}/urls");
        let Some(url) = self.api.get::<ShareUrls>(&path).await?.urls.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(self.link_url(&url)))
    }

    /// Rebuilds the shareable URL. The generated password is stored encrypted to
    /// our address key precisely so any of our clients can show the link again.
    fn link_url(&self, url: &ShareUrl) -> String {
        let generated = url.password.as_deref().and_then(|armored| {
            let plain = self
                .pgp
                .new_decryptor()
                .with_decryption_keys(self.address_keys.iter())
                .decrypt(armored, DataEncoding::Armor)
                .ok()?;
            let text = String::from_utf8(plain.as_bytes().to_vec()).ok()?;
            Some(text.chars().take(LINK_PASSWORD_LEN).collect::<String>())
        });
        match generated {
            Some(p) => format!("{}#{p}", url.public_url),
            None => url.public_url.clone(),
        }
    }

    /// Creates a read-only public link for `node`, or returns the existing one.
    /// `custom_password` is appended to the generated half and is *not* part of
    /// the URL, so it has to be sent to the recipient separately.
    pub async fn share(
        &self,
        parent: &Node<P::PrivateKey>,
        node: &Node<P::PrivateKey>,
        custom_password: Option<&str>,
        expires_days: Option<i64>,
    ) -> Result<String> {
        if let Some(existing) = self.public_link(node).await? {
            return Ok(existing);
        }
        let (share_id, session_key) = self.ensure_share(parent, node).await?;

        let generated = generated_password();
        let full = match custom_password {
            Some(custom) if !custom.is_empty() => format!("{generated}{custom}"),
            _ => generated.clone(),
        };

        // Visitors prove they know the password by SRP, against a verifier built
        // on a modulus the server signs.
        #[derive(Deserialize)]
        struct Modulus {
            #[serde(rename = "Modulus")]
            modulus: String,
            #[serde(rename = "ModulusID")]
            modulus_id: String,
        }
        let modulus: Modulus = self.api.get("auth/v4/modulus").await?;
        let srp = proton_crypto::new_srp_provider();
        let verifier = srp
            .generate_client_verifier(&full, &modulus.modulus)
            .map_err(|e| anyhow!("srp verifier: {e}"))?;

        // The share's session key, re-wrapped under a key derived from the
        // password: that is what lets a visitor decrypt the share at all.
        let salt: [u8; 16] = proton_crypto::generate_secure_random_bytes();
        let derived = link_password_key(&full, &salt)?;
        let key_packet = self
            .pgp
            .new_encryptor()
            .with_passphrase(&derived)
            .encrypt_session_key(&session_key)
            .map_err(|e| anyhow!("wrap share key under the link password: {e}"))?;

        let address_public = self
            .pgp
            .private_key_to_public_key(&self.signing_key)
            .map_err(|e| anyhow!("public key: {e}"))?;
        let stored_password = String::from_utf8(
            self.pgp
                .new_encryptor()
                .with_encryption_key(&address_public)
                .encrypt_raw(full.as_bytes(), DataEncoding::Armor)
                .map_err(|e| anyhow!("store link password: {e}"))?,
        )?;

        let mut body = json!({
            "CreatorEmail": self.email,
            "Permissions": 4, // viewer
            "Flags": if custom_password.map(|p| !p.is_empty()).unwrap_or(false) { 3 } else { 2 },
            "SharePasswordSalt": B64.encode(salt),
            "SharePassphraseKeyPacket": B64.encode(key_packet),
            "Password": stored_password,
            "UrlPasswordSalt": verifier.salt,
            "SRPVerifier": verifier.verifier,
            "SRPModulusID": modulus.modulus_id,
            "MaxAccesses": 0,
        });
        if let Some(days) = expires_days {
            body["ExpirationTime"] = json!(now() + days * 86_400);
        }

        #[derive(Deserialize)]
        struct R {
            #[serde(rename = "ShareURL")]
            url: ShareUrl,
        }
        let path = format!("drive/shares/{share_id}/urls");
        let created: R = self.api.post(&path, &body).await?;
        Ok(format!("{}#{generated}", created.url.public_url))
    }

    /// Removes every public link on `node`. Returns how many were removed.
    pub async fn unshare(&self, node: &Node<P::PrivateKey>) -> Result<usize> {
        let Some(share_id) = node.share_id.clone() else { return Ok(0) };
        let path = format!("drive/shares/{share_id}/urls");
        let urls = self.api.get::<ShareUrls>(&path).await?.urls;
        for url in &urls {
            let path = format!("drive/shares/{share_id}/urls/{}", url.id);
            let _: serde_json::Value = self.api.delete(&path).await?;
        }
        Ok(urls.len())
    }
}

#[derive(Deserialize)]
struct ShareUrls {
    #[serde(rename = "ShareURLs", default)]
    urls: Vec<ShareUrl>,
}

#[derive(Deserialize)]
struct ShareUrl {
    #[serde(rename = "ShareURLID")]
    id: String,
    #[serde(rename = "PublicUrl", default)]
    public_url: String,
    /// The full link password, encrypted to the creator's address key.
    #[serde(rename = "Password", default)]
    password: Option<String>,
}

/// Proton's alphabet and length for the generated half of a link password.
const LINK_PASSWORD_LEN: usize = 12;
const LINK_PASSWORD_CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// What changed on the volume since a cursor.
pub struct Events {
    pub cursor: String,
    /// The cursor was too old (or absent): walk the tree instead.
    pub refresh: bool,
    pub changes: Vec<Change>,
}

/// One link's latest state in an event batch.
pub struct Change {
    pub link_id: String,
    pub parent_id: Option<String>,
    /// Deleted or trashed; nothing to fetch.
    pub gone: bool,
}

/// One timeline entry. The capture time is what orders the timeline, and it is
/// what a photo is filed under locally.
#[derive(Deserialize)]
pub struct TimelinePhoto {
    #[serde(rename = "LinkID")]
    pub id: String,
    #[serde(rename = "CaptureTime", default)]
    pub capture_time: i64,
}

/// Unlocks a share key with the account's address keys, then decrypts the
/// share's root node with it.
fn decrypt_share_root<P: PGPProviderSync>(
    pgp: &P,
    address_keys: &[P::PrivateKey],
    share: &BootstrapShare,
    volume_id: &str,
    link: LinkDetails,
) -> Result<Node<P::PrivateKey>> {
    let passphrase = pgp
        .new_decryptor()
        .with_decryption_keys(address_keys.iter())
        .decrypt(&share.passphrase, DataEncoding::Armor)
        .map_err(|e| anyhow!("decrypt share passphrase: {e}"))?;
    let share_key = pgp
        .private_key_import(&share.key, passphrase.as_bytes(), DataEncoding::Armor)
        .map_err(|e| anyhow!("unlock share key: {e}"))?;
    Drive::<P>::decrypt_link(pgp, volume_id, &share_key, link)
}

/// The key a visitor derives from the link password to unwrap the share's
/// session key: bcrypt over the password and salt, keeping only the hash half.
fn link_password_key(password: &str, salt: &[u8; 16]) -> Result<String> {
    let hashed = proton_crypto::new_srp_provider()
        .mailbox_password(password.as_bytes(), salt)
        .map_err(|e| anyhow!("derive link key: {e}"))?;
    Ok(std::str::from_utf8(hashed.password_hash()).context("derived key is not ascii")?.to_owned())
}

/// Rejection sampling, so every character is equally likely: taking bytes mod 62
/// would quietly favour the first few letters.
fn generated_password() -> String {
    let limit = 256 - 256 % LINK_PASSWORD_CHARSET.len();
    let mut out = String::with_capacity(LINK_PASSWORD_LEN);
    while out.len() < LINK_PASSWORD_LEN {
        for b in proton_crypto::generate_secure_random_bytes::<32>() {
            if (b as usize) < limit {
                out.push(LINK_PASSWORD_CHARSET[b as usize % LINK_PASSWORD_CHARSET.len()] as char);
                if out.len() == LINK_PASSWORD_LEN {
                    break;
                }
            }
        }
    }
    out
}

fn name_hash(hash_key: &[u8], name: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(hash_key).expect("hmac accepts any key length");
    mac.update(name.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Unix seconds → (year, month, day, hour, minute, second) UTC
/// (Howard Hinnant's civil-from-days).
pub fn civil_utc(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// Unix seconds → "YYYY-MM-DDTHH:MM:SSZ", the form Drive's extended attributes use.
fn iso8601(secs: i64) -> String {
    let (y, m, d, hh, mm, ss) = civil_utc(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }

    /// The step a visitor depends on: the share's session key must come back out
    /// of the packet using nothing but the link password and the stored salt.
    #[test]
    fn link_password_unwraps_the_share_key() {
        let pgp = proton_crypto::new_pgp_provider();
        let session_key = pgp.session_key_generate(SessionKeyAlgorithm::Aes256).unwrap();
        let salt: [u8; 16] = proton_crypto::generate_secure_random_bytes();
        let derived = link_password_key("oHVAuzd2NcS2", &salt).unwrap();
        let packet = pgp.new_encryptor().with_passphrase(&derived).encrypt_session_key(&session_key).unwrap();

        let recovered = pgp
            .new_decryptor()
            .with_passphrase(&derived)
            .decrypt_session_key(&packet)
            .expect("the derived key must open the packet");
        assert_eq!(recovered.export().as_ref(), session_key.export().as_ref());

        let wrong = link_password_key("oHVAuzd2NcS3", &salt).unwrap();
        assert!(pgp.new_decryptor().with_passphrase(&wrong).decrypt_session_key(&packet).is_err());
    }

    /// `cargo test --release -- --ignored --nocapture unlock_cost`: what one
    /// node costs to unlock. Drives the decision on caching node keys.
    #[test]
    #[ignore]
    fn unlock_cost() {
        let pgp = proton_crypto::new_pgp_provider();
        let passphrase = B64.encode(proton_crypto::generate_secure_random_bytes::<32>()).into_bytes();
        let key = pgp.new_key_generator().with_user_id("Drive key", "no-reply@proton.me").generate().unwrap();
        let locked = pgp.private_key_export(&key, &passphrase, DataEncoding::Armor).unwrap();
        let locked = locked.as_ref().to_vec();
        let public = pgp.private_key_to_public_key(&key).unwrap();
        let name = pgp.new_encryptor().with_encryption_key(&public).encrypt_raw(b"a name", DataEncoding::Armor).unwrap();
        let n = 50;
        let t = std::time::Instant::now();
        for _ in 0..n {
            let _ = pgp.private_key_import(&locked, &passphrase, DataEncoding::Armor).unwrap();
        }
        let import = t.elapsed() / n;
        let t = std::time::Instant::now();
        for _ in 0..n {
            let _ = pgp.new_decryptor().with_decryption_key(&key).decrypt(&name, DataEncoding::Armor).unwrap();
        }
        let decrypt = t.elapsed() / n;
        println!("private_key_import (S2K): {import:?} per node; small decrypt: {decrypt:?} each; per listed node ~{:?}", import + decrypt * 2);
    }

    #[test]
    fn link_passwords() {
        let a = generated_password();
        assert_eq!(a.chars().count(), LINK_PASSWORD_LEN);
        assert!(a.bytes().all(|b| LINK_PASSWORD_CHARSET.contains(&b)), "{a}");
        assert_ne!(a, generated_password(), "two draws must not match");
    }

    #[test]
    fn hmac_name_hash() {
        // HMAC-SHA256(key="key", "The quick brown fox jumps over the lazy dog") — RFC-style known answer.
        let h = name_hash(b"key", "The quick brown fox jumps over the lazy dog");
        assert_eq!(h, "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8");
    }
}
