//! Proof-carrying work (Beyond B5): every merge the gate makes leaves a signed receipt.
//!
//! A receipt records what the gate saw when it merged: the mission, the pull request, the head
//! commit that was reviewed, the merge commit, every CI check and its state, who implemented and
//! who reviewed with what verdict, the verify result and the metered spend. Receipts form a hash
//! chain (each one hashes the one before it) in `receipts.jsonl` under Studio's own folder, and
//! each is signed with an ed25519 key generated on this machine (`keys/receipts.seed`), so
//! editing, reordering or deleting any receipt breaks verification. Writing a receipt never undoes
//! a merge: a failure is logged on the mission's timeline instead.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::missions::Mission;
use crate::store;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Check {
    pub name: String,
    pub state: String,
}

/// One merge, as the gate saw it. Field order is the hash order: never reorder.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub seq: u64,
    pub at: i64,
    pub mission: String,
    pub goal: Option<String>,
    pub repo: String,
    pub pr: u64,
    pub pr_url: String,
    pub head_commit: String,
    pub merge_commit: Option<String>,
    pub checks: Vec<Check>,
    pub implementer: String,
    pub reviewer: String,
    pub verdict: String,
    pub verify: String,
    pub spent_usd: f64,
    pub prev_hash: String,
    pub public_key: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub signature: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        bail!("odd hex length");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!("bad hex: {e}")))
        .collect()
}

/// The hash of a receipt: SHA-256 over its JSON with `hash` and `signature` empty. Pure.
pub fn digest(r: &Receipt) -> Result<String> {
    let mut bare = r.clone();
    bare.hash.clear();
    bare.signature.clear();
    Ok(hex(&Sha256::digest(serde_json::to_vec(&bare)?)))
}

/// Seal a receipt: link it to the previous hash, hash it and sign the hash. Pure.
pub fn seal(mut r: Receipt, prev_hash: &str, key: &SigningKey) -> Result<Receipt> {
    r.prev_hash = prev_hash.to_string();
    r.public_key = hex(key.verifying_key().as_bytes());
    r.hash = digest(&r)?;
    r.signature = hex(&key.sign(r.hash.as_bytes()).to_bytes());
    Ok(r)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Verification {
    pub ok: bool,
    pub receipts: usize,
    pub public_key: Option<String>,
    /// The first problem found, by receipt number.
    pub problem: Option<String>,
}

/// Check the whole chain: order, links, hashes, signatures and one key throughout. Pure.
pub fn verify_chain(chain: &[Receipt]) -> Verification {
    let fail = |p: String| Verification {
        ok: false,
        receipts: chain.len(),
        public_key: chain.first().map(|r| r.public_key.clone()),
        problem: Some(p),
    };
    let mut prev = String::new();
    for (i, r) in chain.iter().enumerate() {
        let n = i as u64 + 1;
        if r.seq != n {
            return fail(format!(
                "receipt {n} is numbered {}: one is missing or reordered",
                r.seq
            ));
        }
        if r.prev_hash != prev {
            return fail(format!("receipt {n} does not follow receipt {}", n - 1));
        }
        match digest(r) {
            Ok(h) if h == r.hash => {}
            _ => {
                return fail(format!(
                    "receipt {n} ({}) was changed after it was sealed",
                    r.mission
                ))
            }
        }
        if r.public_key != chain[0].public_key {
            return fail(format!("receipt {n} is signed with a different key"));
        }
        let ok = (|| -> Result<()> {
            let pk: [u8; 32] = unhex(&r.public_key)?
                .try_into()
                .map_err(|_| anyhow!("key length"))?;
            let sig: [u8; 64] = unhex(&r.signature)?
                .try_into()
                .map_err(|_| anyhow!("signature length"))?;
            VerifyingKey::from_bytes(&pk)?
                .verify(r.hash.as_bytes(), &Signature::from_bytes(&sig))?;
            Ok(())
        })();
        if ok.is_err() {
            return fail(format!(
                "receipt {n} ({}) has an invalid signature",
                r.mission
            ));
        }
        prev = r.hash.clone();
    }
    Verification {
        ok: true,
        receipts: chain.len(),
        public_key: chain.first().map(|r| r.public_key.clone()),
        problem: None,
    }
}

pub fn ledger_path() -> PathBuf {
    store::studio_dir().join("receipts.jsonl")
}

fn seed_path() -> PathBuf {
    store::studio_dir().join("keys").join("receipts.seed")
}

/// This machine's receipt key, made on first use from the OS random source (via uuid v4).
fn key() -> Result<SigningKey> {
    let p = seed_path();
    if let Ok(s) = std::fs::read_to_string(&p) {
        let seed: [u8; 32] = unhex(s.trim())?
            .try_into()
            .map_err(|_| anyhow!("the receipt key is not 32 bytes"))?;
        return Ok(SigningKey::from_bytes(&seed));
    }
    let mut seed = [0u8; 32];
    seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, hex(&seed))?;
    Ok(SigningKey::from_bytes(&seed))
}

pub fn read_from(path: &std::path::Path) -> Result<Vec<Receipt>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Vec::new());
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| anyhow!("a receipt line is not valid: {e}")))
        .collect()
}

pub fn all() -> Result<Vec<Receipt>> {
    read_from(&ledger_path())
}

fn append(path: &std::path::Path, r: Receipt, key: &SigningKey) -> Result<Receipt> {
    let chain = read_from(path)?;
    let prev = chain.last().map(|r| r.hash.clone()).unwrap_or_default();
    let mut r = r;
    r.seq = chain.len() as u64 + 1;
    let sealed = seal(r, &prev, key)?;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{}", serde_json::to_string(&sealed)?)?;
    Ok(sealed)
}

/// A receipt for a merge that just happened. Fields not known yet are filled by `record`. Pure.
pub fn draft(
    m: &Mission,
    goal: Option<String>,
    pr: u64,
    head: &str,
    merge: Option<String>,
    checks: Vec<Check>,
    at: i64,
) -> Receipt {
    Receipt {
        seq: 0,
        at,
        mission: m.id.clone(),
        goal,
        repo: m.repo.clone(),
        pr,
        pr_url: m.pr_url.clone().unwrap_or_default(),
        head_commit: head.to_string(),
        merge_commit: merge,
        checks,
        implementer: m.agent.clone(),
        reviewer: m.reviewer.clone().unwrap_or_default(),
        verdict: m.review_verdict.clone().unwrap_or_default(),
        verify: m
            .verify
            .as_deref()
            .map(|v| v.chars().take(200).collect())
            .unwrap_or_default(),
        spent_usd: m.spent_usd,
        prev_hash: String::new(),
        public_key: String::new(),
        hash: String::new(),
        signature: String::new(),
    }
}

/// Seal and append a receipt for a merge the gate just made.
pub fn record(
    m: &Mission,
    repo: &str,
    pr: u64,
    head: &str,
    checks: &[crate::github::CheckRun],
) -> Result<Receipt> {
    let merge = crate::github::pr_shas(repo, pr).ok().and_then(|(_, m)| m);
    let checks = checks
        .iter()
        .map(|c| Check {
            name: c.name.clone(),
            state: c.state.clone(),
        })
        .collect();
    let r = draft(
        m,
        crate::goals::goal_of(&m.id),
        pr,
        head,
        merge,
        checks,
        store::now(),
    );
    append(&ledger_path(), r, &key()?)
}

pub fn verify() -> Result<Verification> {
    Ok(verify_chain(&all()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission() -> Mission {
        Mission {
            id: "M-080".into(),
            repo: "agentos-sandbox-1".into(),
            agent: "hermes".into(),
            reviewer: Some("antigravity".into()),
            review_verdict: Some("approve".into()),
            pr_url: Some("https://github.com/konethegreat/agentos-sandbox-1/pull/39".into()),
            ..Default::default()
        }
    }

    fn checks() -> Vec<Check> {
        vec![Check {
            name: "build-and-test".into(),
            state: "SUCCESS".into(),
        }]
    }

    fn key_from(b: u8) -> SigningKey {
        SigningKey::from_bytes(&[b; 32])
    }

    #[test]
    fn a_sealed_chain_verifies_and_any_tampering_is_named() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("r.jsonl");
        let k = key_from(7);
        for i in 0..3 {
            let mut m = mission();
            m.id = format!("M-{i}");
            append(
                &path,
                draft(&m, None, 39, "abc", Some("def".into()), checks(), 100 + i),
                &k,
            )
            .unwrap();
        }
        let chain = read_from(&path).unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[1].prev_hash, chain[0].hash);
        let v = verify_chain(&chain);
        assert!(v.ok, "{v:?}");
        assert_eq!(v.receipts, 3);

        let mut edited = chain.clone();
        edited[1].verdict = "reject".into();
        assert!(verify_chain(&edited)
            .problem
            .unwrap()
            .contains("changed after it was sealed"));

        let mut dropped = chain.clone();
        dropped.remove(1);
        assert!(
            !verify_chain(&dropped).ok,
            "a deleted receipt breaks the chain"
        );

        let mut resigned = chain.clone();
        resigned[2] = seal(resigned[2].clone(), &resigned[1].hash, &key_from(9)).unwrap();
        assert!(verify_chain(&resigned)
            .problem
            .unwrap()
            .contains("different key"));

        let mut forged = chain.clone();
        forged[0].signature = "00".repeat(64);
        assert!(verify_chain(&forged)
            .problem
            .unwrap()
            .contains("invalid signature"));

        assert!(
            verify_chain(&[]).ok,
            "no merges yet is a valid, empty ledger"
        );
    }

    #[test]
    fn a_receipt_records_what_the_gate_saw() {
        let r = draft(
            &mission(),
            Some("G-021".into()),
            39,
            "abc123",
            Some("def456".into()),
            checks(),
            5,
        );
        assert_eq!(r.mission, "M-080");
        assert_eq!(r.goal.as_deref(), Some("G-021"));
        assert_eq!(
            (r.implementer.as_str(), r.reviewer.as_str()),
            ("hermes", "antigravity")
        );
        assert_eq!(r.verdict, "approve");
        assert_eq!(r.head_commit, "abc123");
        assert_eq!(r.checks[0].state, "SUCCESS");
    }
}
