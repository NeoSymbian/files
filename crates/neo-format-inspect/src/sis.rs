// SPDX-License-Identifier: Apache-2.0
//! App package info for Symbian 9 packages (SIS v2: .sis, .sisx): a port of the reader in NeoSymbian/legacy's
//! `tools/sis.py`, for the controller only (what the package is and asks for), never the payload.
//!
//! A SIS v2 file is the 16-byte UID header and one SISContents field. Every field is `type: u32, length: u32` (bit 31
//! of the length set: 63 bits, the next u32 holding the top) and its data, padded to 4 bytes; an array's elements
//! leave the type out (the array says it once). SISContents holds two checksums, the SISCompressed controller and
//! the SISData payload, in that order, so [`needed`] can stop before the payload.
//!
//! The field types are `TFieldType` of the SIS file format (Symbian's "SIS File Format" document and `sisfield.h` of
//! the 2010 installer, swi/sisxreader), checked against real packages from the legacy lock file: sis.py only names
//! Compressed (3), InstallBlock (28) and Data (30). Layouts used here:
//! - SISController: SISInfo, SISSupportedOptions, SISSupportedLanguages, SISPrerequisites, SISProperties, [SISLogo],
//!   SISInstallBlock, SISSignatureCertificateChain*, SISDataIndex
//! - SISInfo: SISUid, SISString vendor unique name, SISArray<SISString> names, SISArray<SISString> vendor names,
//!   SISVersion, SISDateTime, u8 install type, u8 install flags
//! - SISPrerequisites: SISArray<SISDependency> target devices, SISArray<SISDependency> dependencies; SISDependency:
//!   SISUid, [SISVersionRange], SISArray<SISString>
//! - SISInstallBlock: SISArray<SISFileDescription>, SISArray<SISController> embedded, SISArray<SISIf>; SISIf:
//!   SISExpression, SISInstallBlock, SISArray<SISElseIf>; SISElseIf: SISExpression, SISInstallBlock
//! - SISFileDescription: SISString target, SISString MIME type, [SISCapabilities], SISHash, then 28 raw bytes
//!   (u32 operation, u32 options, u64 length, u64 uncompressed length, u32 file index)
//! - SISSignatureCertificateChain: SISArray<SISSignature>, SISCertificateChain; SISCertificateChain: SISBlob (the
//!   chain's certificates, DER, back to back)

use crate::{Error, Result, cert, le32};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const UID1: u32 = 0x1020_1A7A;
/// SIS v1 (Symbian 8 and earlier): UID2 is 0x1000006D (EPOC R3/5, 6.0) or 0x10003A12 (6.1+), UID3 0x10000419.
const V1_UID2: [u32; 2] = [0x1000_006D, 0x1000_3A12];
const V1_UID3: u32 = 0x1000_0419;

const T_STRING: u32 = 1;
const T_ARRAY: u32 = 2;
const T_COMPRESSED: u32 = 3;
const T_VERSION: u32 = 4;
const T_UID: u32 = 9;
const T_CONTENTS: u32 = 12;
const T_CONTROLLER: u32 = 13;
const T_INFO: u32 = 14;
const T_PREREQUISITES: u32 = 17;
const T_CERTIFICATE_CHAIN: u32 = 22;
const T_INSTALL_BLOCK: u32 = 28;
const T_BLOB: u32 = 37;
const T_SIGNATURE_CERTIFICATE_CHAIN: u32 = 39;
const T_CAPABILITIES: u32 = 41;

const OP_INSTALL: u32 = 1;
/// The controller inflates to at most this (a bomb guard; real ones are a few kB).
const MAX_CONTROLLER: usize = 4 << 20;
/// If-blocks nest at most this deep.
const MAX_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capability {
    /// Symbian's name ("ReadUserData").
    pub name: &'static str,
    /// What it lets the app do, for the user.
    pub text: &'static str,
}

/// `TCapability` order (e32capability.h): bit n of a SISCapabilities word is `CAPABILITIES[n]`.
pub const CAPABILITIES: [Capability; 20] = {
    const fn c(name: &'static str, text: &'static str) -> Capability {
        Capability { name, text }
    }
    [
        c("TCB", "Change the core of the system"),
        c("CommDD", "Use communication drivers"),
        c("PowerMgmt", "Turn the phone off and manage power"),
        c("MultimediaDD", "Use sound and video drivers"),
        c("ReadDeviceData", "Read the phone's settings"),
        c("WriteDeviceData", "Change the phone's settings"),
        c("DRM", "Open protected media"),
        c("TrustedUI", "Show trusted dialogs"),
        c("ProtServ", "Run protected system services"),
        c("DiskAdmin", "Format and mount drives"),
        c("NetworkControl", "Change network settings"),
        c("AllFiles", "Read every file on the phone"),
        c("SwEvent", "Press keys and tap for you"),
        c("NetworkServices", "Use the internet"),
        c("LocalServices", "Use Bluetooth and USB"),
        c("ReadUserData", "Read your contacts, messages and files"),
        c("WriteUserData", "Change your files"),
        c("Location", "Know where you are"),
        c("SurroundingsDD", "Use location and sensor drivers"),
        c("UserEnvironment", "Use the camera and microphone"),
    ]
};

const PLATFORMS: &[(u32, &str)] = &[
    (0x101F_7961, "S60 3rd Edition"),
    (0x1020_32BE, "S60 3rd Edition FP1"),
    (0x1027_52AE, "S60 3rd Edition FP2"),
    (0x1028_315F, "S60 5th Edition"),
    (0x2002_2E6D, "Symbian^3"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub vendor: String,
    pub version: (u32, u32, u32),
    pub uid: u32,
    /// The target devices it declares ("S60 3rd Edition", else "0x…").
    pub platforms: Vec<String>,
    /// The drive its files go to; None: "!", the user picks.
    pub drive: Option<char>,
    /// Asked for by any of its files, in `TCapability` order.
    pub capabilities: Vec<Capability>,
    /// The common name (else the organisation) and expiry of the first certificate of the first signature.
    pub signer: Option<(String, cert::Time)>,
    /// Files it installs.
    pub files: usize,
}

/// Given the file's first bytes (64 are always enough): how many bytes from the start hold the controller.
pub fn needed(head: &[u8]) -> Result<usize> {
    Ok(controller_at(head)?.1)
}

/// The package's controller; `bytes` needs to run to [`needed`] only.
pub fn read(bytes: &[u8]) -> Result<Package> {
    let (start, end) = controller_at(bytes)?;
    let comp = bytes.get(start..end).ok_or(Error::Truncated)?;
    let alg = le32(comp, 0)?;
    let size = u64::from(le32(comp, 4)?) | u64::from(le32(comp, 8)?) << 32;
    if size > MAX_CONTROLLER as u64 {
        return Err(Error::Bad("The package is damaged"));
    }
    let raw = comp.get(12..).ok_or(Error::Truncated)?;
    let ctl = match alg {
        0 => raw.to_vec(),
        1 => miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(raw, MAX_CONTROLLER)
            .map_err(|_| Error::Bad("The package is damaged"))?,
        _ => return Err(Error::Unsupported("this package's compression")),
    };
    let (t, ctl, _) = field(&ctl, 0, None)?;
    if t != T_CONTROLLER {
        return Err(BAD);
    }
    let parts = fields(ctl)?;
    let get = |ty| parts.iter().find(|f| f.0 == ty).map(|f| f.1).ok_or(BAD);

    let info = fields(get(T_INFO)?)?;
    let at = |i: usize, ty: u32| match info.get(i) {
        Some(&(t, d)) if t == ty => Ok(d),
        _ => Err(BAD),
    };
    let uid = le32(at(0, T_UID)?, 0)?;
    let unique_vendor = string(at(1, T_STRING)?);
    let name = array(at(2, T_ARRAY)?)?
        .first()
        .map(|s| string(s))
        .unwrap_or_default();
    let vendor = array(at(3, T_ARRAY)?)?
        .first()
        .map(|s| string(s))
        .filter(|s| !s.is_empty())
        .unwrap_or(unique_vendor);
    let v = at(4, T_VERSION)?;
    let part = |i| le32(v, i).map(|n| (n as i32).max(0) as u32);
    let version = (part(0)?, part(4)?, part(8)?);

    let mut platforms = Vec::new();
    if let Some(&(T_ARRAY, devices)) = fields(get(T_PREREQUISITES)?)?.first() {
        for dep in array(devices)? {
            match fields(dep)?.first() {
                Some(&(T_UID, u)) => {
                    let u = le32(u, 0)?;
                    platforms.push(match PLATFORMS.iter().find(|p| p.0 == u) {
                        Some(p) => String::from(p.1),
                        None => format!("0x{u:08X}"),
                    });
                }
                _ => return Err(BAD),
            }
        }
    }

    let mut files = Files::default();
    install_block(get(T_INSTALL_BLOCK)?, 0, &mut files)?;

    let signer = match parts.iter().find(|f| f.0 == T_SIGNATURE_CERTIFICATE_CHAIN) {
        Some(&(_, chain)) => signer(chain)?,
        None => None,
    };

    Ok(Package {
        name,
        vendor,
        version,
        uid,
        platforms,
        drive: files.drive.filter(|&d| d != '!'),
        capabilities: (0..CAPABILITIES.len())
            .filter(|&i| files.caps & 1 << i != 0)
            .map(|i| CAPABILITIES[i])
            .collect(),
        signer,
        files: files.count,
    })
}

const BAD: Error = Error::Bad("The package is damaged");

/// The SISCompressed controller's data: (start, end) as file offsets.
fn controller_at(b: &[u8]) -> Result<(usize, usize)> {
    let uid1 = le32(b, 0)?;
    if uid1 != UID1 {
        let (uid2, uid3) = (le32(b, 4)?, le32(b, 8)?);
        if V1_UID2.contains(&uid2) || uid3 == V1_UID3 {
            return Err(Error::Unsupported("a Symbian 8 package"));
        }
        return Err(Error::Bad("Not a Symbian package"));
    }
    let (t, _, start) = header(b, 16, None)?;
    if t != T_CONTENTS {
        return Err(BAD);
    }
    // the contents run past what we were given: walk their fields until the controller
    let mut p = start;
    loop {
        let (t, n, data) = header(b, p, None)?;
        if t == T_COMPRESSED {
            let end = data
                .checked_add(n)
                .filter(|&e| e - data <= MAX_CONTROLLER)
                .ok_or(BAD)?;
            return Ok((data, end));
        }
        if !matches!(t, 34 | 35) {
            // only the controller and data checksums come first
            return Err(BAD);
        }
        p = data
            .checked_add(n)
            .and_then(|e| e.checked_add(pad(n)))
            .ok_or(BAD)?;
    }
}

fn pad(n: usize) -> usize {
    (4 - n % 4) % 4
}

/// A field header at `p` (`ty` given: an array element, no type) → (type, length, data offset).
fn header(b: &[u8], p: usize, ty: Option<u32>) -> Result<(u32, usize, usize)> {
    let (t, mut p) = match ty {
        Some(t) => (t, p),
        None => (le32(b, p)?, p + 4),
    };
    let lo = le32(b, p)?;
    p += 4;
    let mut n = u64::from(lo);
    if lo & 0x8000_0000 != 0 {
        n = u64::from(lo & 0x7FFF_FFFF) | u64::from(le32(b, p)?) << 31;
        p += 4;
    }
    let n = usize::try_from(n).map_err(|_| BAD)?;
    Ok((t, n, p))
}

/// A whole field at `p` → (type, data, the next field's offset).
fn field(b: &[u8], p: usize, ty: Option<u32>) -> Result<(u32, &[u8], usize)> {
    let (t, n, p) = header(b, p, ty)?;
    let end = p.checked_add(n).ok_or(BAD)?;
    let d = b.get(p..end).ok_or(Error::Truncated)?;
    Ok((t, d, end.saturating_add(pad(n))))
}

/// A field's children, (type, data).
fn fields(b: &[u8]) -> Result<Vec<(u32, &[u8])>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p.saturating_add(8) <= b.len() {
        let (t, d, next) = field(b, p, None)?;
        out.push((t, d));
        p = next;
    }
    Ok(out)
}

/// A SISArray's elements.
fn array(b: &[u8]) -> Result<Vec<&[u8]>> {
    let et = le32(b, 0)?;
    let mut out = Vec::new();
    let mut p = 4;
    while p < b.len() {
        let (_, d, next) = field(b, p, Some(et))?;
        out.push(d);
        p = next;
    }
    Ok(out)
}

fn string(b: &[u8]) -> String {
    let units = b.as_chunks::<2>().0.iter().map(|&u| u16::from_le_bytes(u));
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

#[derive(Default)]
struct Files {
    count: usize,
    caps: u32,
    drive: Option<char>,
}

fn install_block(b: &[u8], depth: usize, out: &mut Files) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(BAD);
    }
    let parts = fields(b)?;
    let list = |i: usize| match parts.get(i) {
        Some(&(T_ARRAY, d)) => array(d),
        _ => Err(BAD),
    };
    for fd in list(0)? {
        let (t, target, p) = field(fd, 0, None)?;
        let (_, _, p) = field(fd, p, None)?; // the MIME type
        if t != T_STRING {
            return Err(BAD);
        }
        if let Ok((T_CAPABILITIES, caps, _)) = field(fd, p, None) {
            out.caps |= le32(caps, 0).unwrap_or(0);
        }
        let op = fd.len().checked_sub(28).map(|i| le32(fd, i)).ok_or(BAD)??;
        if op == OP_INSTALL {
            out.count += 1;
            let target = string(target);
            let mut cs = target.chars();
            if let (None, Some(d), Some(':')) = (out.drive, cs.next(), cs.next()) {
                out.drive = Some(d.to_ascii_uppercase());
            }
        }
    }
    for if_block in list(2)? {
        let parts = fields(if_block)?;
        let (Some(&(T_INSTALL_BLOCK, blk)), Some(&(T_ARRAY, elses))) = (parts.get(1), parts.get(2))
        else {
            return Err(BAD);
        };
        install_block(blk, depth + 1, out)?;
        for e in array(elses)? {
            match fields(e)?.get(1) {
                Some(&(T_INSTALL_BLOCK, blk)) => install_block(blk, depth + 1, out)?,
                _ => return Err(BAD),
            }
        }
    }
    Ok(())
}

fn signer(chain: &[u8]) -> Result<Option<(String, cert::Time)>> {
    let Some(&(T_CERTIFICATE_CHAIN, certs)) = fields(chain)?.get(1) else {
        return Err(BAD);
    };
    let Some(&(T_BLOB, der)) = fields(certs)?.first() else {
        return Err(BAD);
    };
    // a certificate we can't read hides the signer, not the package
    Ok(cert::parse(der).ok().and_then(|cs| {
        let c = cs.into_iter().next()?;
        let who = c.subject.cn.or(c.subject.o).unwrap_or_default();
        Some((who, c.not_after))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn capability_order_is_symbian_s() {
        assert_eq!(CAPABILITIES[0].name, "TCB");
        assert_eq!(CAPABILITIES[13].name, "NetworkServices");
        assert_eq!(CAPABILITIES[15].name, "ReadUserData");
        assert_eq!(CAPABILITIES[19].name, "UserEnvironment");
    }

    #[test]
    fn version_1_is_unsupported() {
        let mut b = vec![0u8; 64];
        b[0..4].copy_from_slice(&0x1000_1234u32.to_le_bytes());
        b[4..8].copy_from_slice(&0x1000_3A12u32.to_le_bytes());
        b[8..12].copy_from_slice(&V1_UID3.to_le_bytes());
        assert_eq!(read(&b), Err(Error::Unsupported("a Symbian 8 package")));
        assert_eq!(needed(&b[..8]), Err(Error::Truncated));
    }
}
