//! Proton Drive key chain and read operations: my-files root, listing, download.
//!
//! Key chain: user keys (unlocked by the key secret from login) → address keys
//! → share key (share passphrase encrypted to an address key) → node keys
//! (each node's passphrase encrypted to its parent's key) → file content
//! session key (packet encrypted to the node key) → blocks.

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD as B64;
use hmac::{Hmac, Mac};
use proton_crypto::crypto::{
    DataEncoding, Decryptor, DecryptorSync, Encryptor, EncryptorSync, KeyGenerator, KeyGeneratorSync, PGPProviderSync,
    SessionKey, SessionKeyAlgorithm, Signer, SignerSync, VerifiedData,
};
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
    /// Server-side last modification, unix seconds.
    pub modify_time: i64,
    /// Active revision id for files; changes when content changes.
    pub revision: Option<String>,
    key: K,
    file: Option<File>,
    hash_key: Option<String>,
}

pub struct Drive<P: PGPProviderSync> {
    pub api: Api,
    pgp: P,
    volume_id: String,
    root: Node<P::PrivateKey>,
    /// The share's address: id, email, and its primary key, which signs what we write.
    address_id: String,
    email: String,
    signing_key: P::PrivateKey,
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
            #[serde(rename = "Email")]
            email: String,
            #[serde(rename = "Keys")]
            keys: AddressKeys,
        }
        let addresses: Addresses = api.get("core/v4/addresses").await?;
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
        let (email, primary, keys) = address_keys
            .remove(&my_files.share.address_id)
            .filter(|(_, _, k)| !k.is_empty())
            .ok_or_else(|| anyhow!("no unlocked keys for the share's address"))?;
        let signing_key = primary.ok_or_else(|| anyhow!("share address has no primary key"))?;
        let passphrase = pgp
            .new_decryptor()
            .with_decryption_keys(keys.iter())
            .decrypt(&my_files.share.passphrase, DataEncoding::Armor)
            .map_err(|e| anyhow!("decrypt share passphrase: {e}"))?;
        let share_key = pgp
            .private_key_import(&my_files.share.key, passphrase.as_bytes(), DataEncoding::Armor)
            .map_err(|e| anyhow!("unlock share key: {e}"))?;
        let root = Self::decrypt_link(&pgp, &share_key, my_files.link)?;
        Ok(Self {
            api,
            pgp,
            volume_id: my_files.volume.id,
            root,
            address_id: my_files.share.address_id,
            email,
            signing_key,
        })
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
            modify_time: d.link.modify_time,
            revision: d.file.as_ref().and_then(|f| f.active_revision.as_ref()).map(|r| r.id.clone()),
            key,
            file: d.file,
            hash_key: d.folder.map(|f| f.hash_key),
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
                if d.link.state != 1 || d.link.trash_time.is_some() {
                    continue; // trashed, draft or deleted
                }
                nodes.push(Self::decrypt_link(&self.pgp, &folder.key, d)?);
            }
        }
        nodes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(nodes)
    }

    /// Advances the event cursor. Returns the new cursor and whether anything
    /// happened since `cursor`. A `None` cursor (first sync) always counts as changed.
    /// The "latest" id is opaque and differs per call, so only the events list is trusted.
    pub async fn events_since(&mut self, cursor: Option<&str>) -> Result<(String, bool)> {
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
            events: Vec<serde_json::Value>,
            #[serde(rename = "More", default)]
            more: bool,
            #[serde(rename = "Refresh", default)]
            refresh: bool,
        }
        let Some(mut cursor) = cursor.map(str::to_owned) else {
            let path = format!("drive/volumes/{}/events/latest", self.volume_id);
            return Ok((self.api.get::<Latest>(&path).await?.event_id, true));
        };
        let mut changed = false;
        loop {
            let path = format!("drive/v2/volumes/{}/events/{cursor}", self.volume_id);
            let page: Page = self.api.get(&path).await?;
            if std::env::var_os("KPDRIVE_DEBUG").is_some() {
                eprintln!("events page: {} events, more={}, refresh={}, next={}", page.events.len(), page.more, page.refresh, page.event_id);
            }
            changed |= page.refresh || !page.events.is_empty();
            cursor = page.event_id;
            if page.refresh || !page.more {
                return Ok((cursor, changed));
            }
        }
    }

    /// Walks `path` ("a/b/c", leading slash optional) from the root.
    pub async fn resolve(&mut self, path: &str) -> Result<Node<P::PrivateKey>> {
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
            name: self.root.name.clone(),
            is_folder: true,
            modify_time: self.root.modify_time,
            revision: None,
            key: self.reimport(&self.root.key)?,
            file: None,
            hash_key: self.root.hash_key.clone(),
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

    pub async fn create_folder(&mut self, parent: &Node<P::PrivateKey>, name: &str) -> Result<Node<P::PrivateKey>> {
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
        let path = format!("drive/v2/volumes/{}/folders", self.volume_id);
        let r: R = self.api.post(&path, &serde_json::Value::Object(body)).await?;
        Ok(Node {
            id: r.folder.id,
            name: name.to_owned(),
            is_folder: true,
            modify_time: now(),
            revision: None,
            key,
            file: None,
            hash_key: Some(hash_key_armored),
        })
    }

    /// Moves nodes to the trash (the user can restore them in the web app).
    pub async fn trash(&mut self, ids: &[String]) -> Result<()> {
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
        &mut self,
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
                let path = format!("drive/v2/volumes/{}/files/{}/revisions", self.volume_id, node.id);
                let uid = self.api.session.as_ref().map(|s| s.uid.clone()).unwrap_or_default();
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
                body.insert("ClientUID".into(), self.api.session.as_ref().map(|s| s.uid.clone()).unwrap_or_default().into());
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
                let path = format!("drive/v2/volumes/{}/files", self.volume_id);
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
        let path = format!("drive/v2/volumes/{}/links/{link_id}/revisions/{revision_id}/verification", self.volume_id);
        let code = B64.decode(self.api.get::<Verification>(&path).await?.code).context("verification code base64")?;

        // 3. Blocks. ponytail: one prepare request and one upload per block, sequential.
        let mut manifest = Vec::new();
        let mut block_sizes = Vec::new();
        let mut sha1 = Sha1::new();
        let mut total = 0u64;
        let mut buf = vec![0u8; BLOCK_SIZE];
        let mut index = 1i64;
        loop {
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
            }
            let prep: Prep = self
                .api
                .post(
                    "drive/blocks",
                    &json!({
                        "AddressID": self.address_id,
                        "VolumeID": self.volume_id,
                        "LinkID": link_id,
                        "RevisionID": revision_id,
                        "BlockList": [{
                            "Index": index,
                            "Size": ciphertext.len(),
                            "EncSignature": enc_sig,
                            "Hash": B64.encode(digest),
                            "Verifier": { "Token": B64.encode(&token) },
                        }],
                        "ThumbnailList": [],
                    }),
                )
                .await?;
            let target = prep.links.into_iter().next().ok_or_else(|| anyhow!("no upload link for block {index}"))?;
            self.api.post_block(&target.bare_url, &target.token, ciphertext).await?;
            index += 1;
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

/// Unix seconds → "YYYY-MM-DDTHH:MM:SSZ" (Howard Hinnant's civil-from-days).
fn iso8601(secs: i64) -> String {
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
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", tod / 3600, (tod % 3600) / 60, tod % 60)
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

    #[test]
    fn hmac_name_hash() {
        // HMAC-SHA256(key="key", "The quick brown fox jumps over the lazy dog") — RFC-style known answer.
        let h = name_hash(b"key", "The quick brown fox jumps over the lazy dog");
        assert_eq!(h, "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8");
    }
}
